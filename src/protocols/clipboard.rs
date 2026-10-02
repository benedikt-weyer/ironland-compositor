//! Server side of the `ironland-clipboard-history-v1` protocol (see
//! `crate::ironland_protocols::clipboard_history` for the generated bindings
//! and `protocols/ironland-clipboard-history-v1.xml` for the wire format).
//!
//! ## Capture
//!
//! [`capture`] is this module's compositor-facing entry point, called from
//! `AnvilState`'s `SelectionHandler::new_selection` (`crate::state`) every
//! time any client - ordinary, Xwayland, or an external `wlr-data-control`/
//! `ext-data-control` clipboard tool - sets the `wl_data_device` clipboard
//! selection. Rather than becoming a second clipboard-manager client that
//! watches from the outside (the way a standalone tool like `wl-clipboard`
//! or `cliphist` has to), the compositor is already sitting at the one
//! place every selection change passes through, so it just reads the
//! content itself: it asks the offering client to write one of its offered
//! mime types into a pipe (`request_data_device_client_selection`) and
//! reads the other end asynchronously via the event loop.
//!
//! Plain text and decodable images (`png`/`jpeg`/`webp` - whatever the
//! `image` crate is built with) are captured, in that preference order when
//! a copy offers both - see the general "in-memory only, no persistence"
//! scope this shares with `crate::screencopy`'s grant table (this crate's
//! top-level `CLAUDE.md`): a copy that offers neither a `text/…`-ish mime
//! type nor a decodable image one, whose text content isn't valid UTF-8, or
//! whose image content doesn't actually decode, is silently not captured,
//! same as one exceeding [`MAX_CAPTURE_BYTES`]/[`MAX_IMAGE_CAPTURE_BYTES`]
//! or one that names the `x-kde-password-manager-hint` mime type - a
//! convention several other clipboard managers already respect to avoid
//! recording secrets copied out of a password manager. A copy identical to
//! an already-recorded entry's content moves that entry back to
//! most-recent instead of duplicating it. An image entry additionally
//! carries a small in-memory-only thumbnail (see [`THUMBNAIL_MAX_DIM`]),
//! sent to granted clients alongside the entry itself so they can render a
//! grid of history without fetching each one's full-resolution content.
//!
//! Reading never blocks the compositor: the pipe's read end is registered
//! with the event loop as a normal non-blocking source, and a short timer
//! (see [`CAPTURE_TIMEOUT`]) abandons the read - simply dropping both ends
//! of the pipe - if the offering client never finishes writing, so a
//! misbehaving or hung source can't leak an event-loop source per copy.
//!
//! ## Permission model
//!
//! Deliberately mirrors `crate::screencopy`'s (see that module's doc for
//! the full reasoning): the `ironland_clipboard_history_manager_v1` global
//! is unrestricted - any client may bind it, since binding alone reveals
//! nothing - but [`ClipboardHistoryState`]'s grant table, keyed by
//! requesting executable path (`ClientState::capture_identity`, resolved
//! the same kernel-backed way for both purposes), gates whether a bound
//! client actually receives `entry` events. An undecided executable is
//! prompted (`crate::permission_prompt`, tagged
//! [`crate::permission_prompt::PromptKind::ClipboardHistory`] so it shares
//! the prompt queue without being confused for a screen-capture prompt);
//! the decision, once made, is remembered in memory for the rest of this
//! compositor process's lifetime, exactly like a capture grant.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Duration;

use rustix::io::Errno;
use rustix::pipe::{PipeFlags, pipe_with};
use smithay::input::{Seat, SeatHandler};
use smithay::reexports::calloop::{
    Interest, LoopHandle, Mode, PostAction,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::wayland::Dispatch2;
use smithay::wayland::GlobalDispatch2;
use smithay::wayland::selection::SelectionSource;
use smithay::wayland::selection::data_device::{DataDeviceHandler, request_data_device_client_selection};
use tracing::debug;

use crate::ironland_protocols::clipboard_history::ironland_clipboard_history_manager_v1::{
    self, IronlandClipboardHistoryManagerV1,
};
use crate::permission_prompt::PromptKind;

/// How many entries [`ClipboardHistoryState`] keeps before dropping the
/// oldest - a bound so a very active clipboard doesn't grow this without
/// limit over a long session.
const MAX_ENTRIES: usize = 200;
/// A captured text copy larger than this is discarded rather than stored -
/// clipboard text is normally far smaller, and this keeps one enormous
/// copy from dominating the in-memory history.
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
/// Same as [`MAX_CAPTURE_BYTES`], but for image copies - kept separate and
/// larger since a screenshot or photo routinely exceeds the text cap.
const MAX_IMAGE_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
/// The box an image entry's thumbnail is scaled to fit within (aspect
/// ratio preserved) - see [`EntryContent::Image`]. No ceiling on the
/// encoded PNG's byte size is needed here: unlike the `entry`/`thumbnail`
/// events themselves, `receive_thumbnail` hands the pixels back over a
/// pipe (see `send_thumbnail`), the same way `receive` does for full
/// content, so nothing about this is bounded by the Wayland wire protocol's
/// per-message size limit.
const THUMBNAIL_MAX_DIM: u32 = 160;
/// How long [`capture`] waits for the offering client to finish writing
/// before giving up on that one copy.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
/// How much of a captured text's start is sent as the `entry` event's
/// `preview` - see `protocols/ironland-clipboard-history-v1.xml`'s `receive`
/// request doc for why the full text isn't inlined into the event itself.
const PREVIEW_CHARS: usize = 256;
/// Mime type convention (used by e.g. KDE's Klipper) for "don't record this
/// copy in clipboard history" - respected here for the same reason.
const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";
/// Mime types tried, in order, as the one actually read back from a new
/// selection - the first one the source also offers wins.
const TEXT_MIME_PRIORITY: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];
/// Mime types tried, in order, when a copy offers no capturable text (see
/// [`TEXT_MIME_PRIORITY`]) - restricted to what the `image` crate is built
/// to decode (see `Cargo.toml`'s `image` dependency features).
const IMAGE_MIME_PRIORITY: &[&str] = &["image/png", "image/jpeg", "image/webp"];

/// Implemented by the compositor state so this module can read/edit the
/// history and grant table, and reach the event loop, without depending on
/// `AnvilState` directly.
pub trait ClipboardHistoryHandler: 'static {
    fn clipboard_history_state(&mut self) -> &mut ClipboardHistoryState;

    /// Resolves `client`'s executable identity for the grant table - in
    /// practice `ClientState::capture_identity`, cached at connect time;
    /// see that field's doc for why it's resolved then rather than lazily
    /// here.
    fn clipboard_client_identity(&self, client: &Client) -> String;
}

/// A captured entry's actual content - see the module doc for what decides
/// which variant a given copy becomes.
#[derive(Debug, Clone)]
enum EntryContent {
    /// Trimmed (see [`finish_capture`]) UTF-8 text, sent back verbatim by
    /// `receive`.
    Text(String),
    /// A decoded image. `bytes` is the *original* captured bytes (in
    /// whichever of [`IMAGE_MIME_PRIORITY`] was actually offered) sent back
    /// verbatim by `receive`; `thumbnail` is a small PNG-encoded downscale
    /// of it, `thumb_width`/`thumb_height` its actual pixel size - both
    /// broadcast inline via the `thumbnail` event (see [`broadcast_entry`]).
    Image {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        thumb_width: u32,
        thumb_height: u32,
        thumbnail: Vec<u8>,
    },
}

impl EntryContent {
    /// The bytes two captures are compared against to decide whether one is
    /// a repeat of the other (see [`finish_capture`]) - the original bytes
    /// either way.
    fn dedup_key(&self) -> &[u8] {
        match self {
            EntryContent::Text(text) => text.as_bytes(),
            EntryContent::Image { bytes, .. } => bytes,
        }
    }

    /// What an `entry` event's `preview` carries for this content - see the
    /// protocol doc.
    fn preview(&self) -> String {
        match self {
            EntryContent::Text(text) => preview_of(text),
            EntryContent::Image { width, height, .. } => format!("Image ({width}×{height})"),
        }
    }
}

#[derive(Debug, Clone)]
struct ClipboardEntry {
    id: u32,
    mime_types: Vec<String>,
    content: EntryContent,
}

/// State of the `ironland_clipboard_history_manager_v1` global: the
/// captured history itself, the per-executable grant table gating access to
/// it, and every currently-bound resource, sorted into which of those two
/// buckets it's in.
#[derive(Debug, Default)]
pub struct ClipboardHistoryState {
    global: Option<GlobalId>,
    /// Most-recent-first.
    entries: VecDeque<ClipboardEntry>,
    next_id: u32,
    /// Per-executable-path decisions, in memory only for this compositor
    /// process's lifetime - see the module doc.
    grants: HashMap<String, bool>,
    /// Bound resources whose executable is currently granted - the ones
    /// `entry`/`removed`/`cleared` are broadcast to, and the only ones
    /// whose `remove`/`remove_all`/`receive` requests do anything.
    listeners: Vec<IronlandClipboardHistoryManagerV1>,
    /// Bound resources still waiting on an undecided executable's prompt to
    /// resolve, alongside the subject they're waiting on.
    pending: Vec<(String, IronlandClipboardHistoryManagerV1)>,
}

impl ClipboardHistoryState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<IronlandClipboardHistoryManagerV1, ManagerGlobalData> + 'static,
    {
        let global =
            dh.create_global::<D, IronlandClipboardHistoryManagerV1, _>(3, ManagerGlobalData);
        ClipboardHistoryState {
            global: Some(global),
            ..Default::default()
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> Option<GlobalId> {
        self.global.clone()
    }

    fn grant(&self, subject: &str) -> Option<bool> {
        self.grants.get(subject).copied()
    }

    fn set_grant(&mut self, subject: String, allowed: bool) {
        self.grants.insert(subject, allowed);
    }
}

fn preview_of(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((byte_idx, _)) => format!("{}…", &text[..byte_idx]),
        None => text.to_string(),
    }
}

/// Sends `entry`'s `entry` event, and its `thumbnail` event if it has one,
/// to `resource` - the shared tail end of [`send_initial_burst`] and
/// [`broadcast_entry`].
fn send_entry(entry: &ClipboardEntry, resource: &IronlandClipboardHistoryManagerV1) {
    resource.entry(entry.id, entry.mime_types.join(" "), entry.content.preview());
    if let EntryContent::Image { thumb_width, thumb_height, .. } = &entry.content {
        resource.thumbnail(entry.id, *thumb_width, *thumb_height);
    }
}

fn send_initial_burst(entries: &VecDeque<ClipboardEntry>, resource: &IronlandClipboardHistoryManagerV1) {
    for entry in entries.iter().rev() {
        send_entry(entry, resource);
    }
}

/// Broadcasts `entry`'s current state to every granted listener - a fresh
/// capture, or an existing entry promoted back to most-recent.
fn broadcast_entry<D: ClipboardHistoryHandler>(state: &mut D, entry: &ClipboardEntry) {
    for listener in &state.clipboard_history_state().listeners {
        send_entry(entry, listener);
    }
}

fn broadcast_removed<D: ClipboardHistoryHandler>(state: &mut D, id: u32) {
    for listener in &state.clipboard_history_state().listeners {
        listener.removed(id);
    }
}

fn broadcast_cleared<D: ClipboardHistoryHandler>(state: &mut D) {
    for listener in &state.clipboard_history_state().listeners {
        listener.cleared();
    }
}

/// Decodes a captured image (`mime_type` was one of [`IMAGE_MIME_PRIORITY`])
/// into its [`EntryContent::Image`], thumbnailing it to fit within
/// [`THUMBNAIL_MAX_DIM`]. `None` if `bytes` doesn't actually decode as an
/// image, or the thumbnail fails to encode.
fn decode_image(bytes: Vec<u8>) -> Option<EntryContent> {
    let img = image::load_from_memory(&bytes).ok()?;
    let thumb = img.thumbnail(THUMBNAIL_MAX_DIM, THUMBNAIL_MAX_DIM);
    let mut thumbnail = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut thumbnail), image::ImageFormat::Png)
        .ok()?;
    Some(EntryContent::Image {
        bytes,
        width: img.width(),
        height: img.height(),
        thumb_width: thumb.width(),
        thumb_height: thumb.height(),
        thumbnail,
    })
}

/// Records a freshly-read copy: dedupes against an existing entry with the
/// same content (moving it to most-recent instead of duplicating it), then
/// evicts the oldest entries past [`MAX_ENTRIES`]. Called once the pipe
/// [`capture`] set up has been fully read.
fn finish_capture<D: ClipboardHistoryHandler>(state: &mut D, mime_types: Vec<String>, bytes: Vec<u8>) {
    let is_image = mime_types.first().is_some_and(|m| IMAGE_MIME_PRIORITY.contains(&m.as_str()));
    let cap = if is_image { MAX_IMAGE_CAPTURE_BYTES } else { MAX_CAPTURE_BYTES };
    if bytes.is_empty() || bytes.len() > cap {
        debug!(len = bytes.len(), "clipboard: capture empty or too large, discarding");
        return;
    }

    let content = if is_image {
        let Some(content) = decode_image(bytes) else {
            debug!("clipboard: capture claimed an image mime type but didn't decode, discarding");
            return;
        };
        content
    } else {
        let Ok(text) = String::from_utf8(bytes) else {
            debug!("clipboard: capture wasn't valid UTF-8, discarding");
            return;
        };
        let text = text.trim_end_matches(['\n', '\r', '\0']).to_string();
        if text.is_empty() {
            debug!("clipboard: capture was empty after trimming, discarding");
            return;
        }
        EntryContent::Text(text)
    };

    let history = state.clipboard_history_state();
    let id = if let Some(pos) = history.entries.iter().position(|e| e.content.dedup_key() == content.dedup_key()) {
        let existing = history.entries.remove(pos).unwrap();
        existing.id
    } else {
        let id = history.next_id;
        history.next_id = history.next_id.wrapping_add(1);
        id
    };
    let entry = ClipboardEntry { id, mime_types, content };
    debug!(
        id,
        listeners = history.listeners.len(),
        "clipboard: entry recorded, broadcasting to granted listeners"
    );
    history.entries.push_front(entry.clone());

    let mut evicted = Vec::new();
    while state.clipboard_history_state().entries.len() > MAX_ENTRIES {
        if let Some(dropped) = state.clipboard_history_state().entries.pop_back() {
            evicted.push(dropped.id);
        }
    }

    broadcast_entry(state, &entry);
    for id in evicted {
        broadcast_removed(state, id);
    }
}

/// Picks which of `source`'s offered mime types to actually read back, and
/// whether to bother at all - see the module doc for what's excluded and
/// why.
fn pick_mime_type(mime_types: &[String]) -> Option<String> {
    if mime_types.iter().any(|m| m == PASSWORD_HINT_MIME) {
        return None;
    }
    TEXT_MIME_PRIORITY
        .iter()
        .find(|candidate| mime_types.iter().any(|m| m == *candidate))
        .map(|m| m.to_string())
        .or_else(|| mime_types.iter().find(|m| m.starts_with("text/")).cloned())
        .or_else(|| {
            IMAGE_MIME_PRIORITY
                .iter()
                .find(|candidate| mime_types.iter().any(|m| m == *candidate))
                .map(|m| m.to_string())
        })
}

/// Entry point called from `AnvilState`'s `SelectionHandler::new_selection`
/// for every `SelectionTarget::Clipboard` change - see the module doc.
/// A no-op if `source` offers nothing worth capturing.
///
/// The actual read is deferred to an idle callback rather than started
/// here: `new_selection` runs *before* Smithay records `source` as the
/// seat's current clipboard selection (it does that itself right after this
/// handler returns), and [`request_data_device_client_selection`] reads
/// that seat-recorded selection - calling it synchronously here would win
/// the read against the *previous* selection (or fail with no selection at
/// all, for the very first copy of a session). An idle callback runs once
/// the event loop is about to block again, i.e. strictly after Smithay's
/// own update, so by then the seat reflects `source`.
pub fn capture<D>(handle: &LoopHandle<'static, D>, seat: &Seat<D>, source: &SelectionSource)
where
    D: SeatHandler + DataDeviceHandler + ClipboardHistoryHandler + 'static,
{
    let mime_types = source.mime_types();
    let Some(mime_type) = pick_mime_type(&mime_types) else {
        debug!(?mime_types, "clipboard: new selection offers nothing capturable, skipping");
        return;
    };
    debug!(%mime_type, ?mime_types, "clipboard: new selection captured, will read on idle");
    let seat = seat.clone();
    let handle_for_idle = handle.clone();
    handle.insert_idle(move |state: &mut D| {
        start_read(&handle_for_idle, &seat, mime_type, state);
    });
}

/// Opens the pipe and asks the offering client (now recorded as the seat's
/// current clipboard selection - see [`capture`]) to write `mime_type`'s
/// content into it, then registers the read side with the event loop.
fn start_read<D>(handle: &LoopHandle<'static, D>, seat: &Seat<D>, mime_type: String, _state: &mut D)
where
    D: SeatHandler + DataDeviceHandler + ClipboardHistoryHandler + 'static,
{
    let Ok((read_fd, write_fd)) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK) else {
        return;
    };
    // The entry only ever ends up holding content actually read back in
    // `mime_type`, so that (not `source`'s full offered list) is what it
    // advertises - see `receive`'s doc in the protocol XML for why this
    // needs to stay accurate.
    let stored_mime_types = vec![mime_type.clone()];
    let read_cap = if IMAGE_MIME_PRIORITY.contains(&mime_type.as_str()) {
        MAX_IMAGE_CAPTURE_BYTES
    } else {
        MAX_CAPTURE_BYTES
    };
    if let Err(err) = request_data_device_client_selection(seat, mime_type, write_fd) {
        debug!(?err, "clipboard: request_data_device_client_selection failed, not capturing");
        return;
    }

    let done = Rc::new(Cell::new(false));
    let done_for_read = done.clone();
    let mut buf = Vec::new();
    let insert_result = handle.insert_source(
        Generic::new(read_fd, Interest::READ, Mode::Level),
        move |_readiness, fd, state: &mut D| {
            loop {
                let mut chunk = [0u8; 8192];
                match rustix::io::read(&*fd, &mut chunk) {
                    Ok(0) => {
                        done_for_read.set(true);
                        finish_capture(state, stored_mime_types.clone(), std::mem::take(&mut buf));
                        return Ok(PostAction::Remove);
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > read_cap {
                            done_for_read.set(true);
                            return Ok(PostAction::Remove);
                        }
                    }
                    Err(Errno::WOULDBLOCK) => return Ok(PostAction::Continue),
                    Err(_) => {
                        done_for_read.set(true);
                        return Ok(PostAction::Remove);
                    }
                }
            }
        },
    );
    let Ok(read_token) = insert_result else {
        return;
    };

    let handle_for_timeout = handle.clone();
    let _ = handle.insert_source(Timer::from_duration(CAPTURE_TIMEOUT), move |_, _, _state| {
        if !done.get() {
            handle_for_timeout.remove(read_token);
        }
        TimeoutAction::Drop
    });
}

/// Resolves a queued clipboard-history prompt (see
/// `crate::permission_prompt::PromptKind::ClipboardHistory`): records the
/// decision, then finishes every resource that was waiting on it - sending
/// the initial burst and moving it to `listeners` if allowed, or sending
/// `denied` if not.
pub fn resolve_grant<D: ClipboardHistoryHandler>(state: &mut D, subject: String, allowed: bool) {
    debug!(%subject, allowed, "clipboard: grant decision recorded");
    state.clipboard_history_state().set_grant(subject.clone(), allowed);

    let history = state.clipboard_history_state();
    let mut waiting = Vec::new();
    history.pending.retain(|(s, resource)| {
        if *s == subject {
            waiting.push(resource.clone());
            false
        } else {
            true
        }
    });

    for resource in waiting {
        if allowed {
            send_initial_burst(&state.clipboard_history_state().entries, &resource);
            state.clipboard_history_state().listeners.push(resource);
        } else {
            resource.denied();
        }
    }
}

/// Global data for the `ironland_clipboard_history_manager_v1` global
/// (nothing to carry - it's unrestricted, see the module doc).
#[derive(Debug)]
pub struct ManagerGlobalData;

/// User data attached to a bound `ironland_clipboard_history_manager_v1`
/// resource (nothing to carry - it's tracked in
/// [`ClipboardHistoryState::listeners`]/[`ClipboardHistoryState::pending`]
/// instead).
#[derive(Debug)]
pub struct ManagerToken;

impl<D> GlobalDispatch2<IronlandClipboardHistoryManagerV1, D> for ManagerGlobalData
where
    D: ClipboardHistoryHandler
        + crate::permission_prompt::PermissionPromptHandler
        + Dispatch<IronlandClipboardHistoryManagerV1, ManagerToken>,
{
    fn bind(
        &self,
        state: &mut D,
        _dh: &DisplayHandle,
        client: &Client,
        resource: New<IronlandClipboardHistoryManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let resource = data_init.init(resource, ManagerToken);
        let subject = state.clipboard_client_identity(client);
        match state.clipboard_history_state().grant(&subject) {
            Some(true) => {
                debug!(%subject, "clipboard: bind from already-granted executable, sending history");
                send_initial_burst(&state.clipboard_history_state().entries, &resource);
                state.clipboard_history_state().listeners.push(resource);
            }
            Some(false) => {
                debug!(%subject, "clipboard: bind from already-denied executable");
                resource.denied();
            }
            None => {
                debug!(%subject, "clipboard: bind from undecided executable, queuing prompt");
                state.clipboard_history_state().pending.push((subject.clone(), resource));
                state.permission_prompt_state().queue_internal(
                    PromptKind::ClipboardHistory,
                    subject,
                    "read your clipboard history".to_string(),
                );
            }
        }
    }
}

impl<D: ClipboardHistoryHandler> Dispatch2<IronlandClipboardHistoryManagerV1, D> for ManagerToken {
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        manager: &IronlandClipboardHistoryManagerV1,
        request: ironland_clipboard_history_manager_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // Only a granted (listening) client's requests have any effect -
        // see the protocol doc.
        let is_listener = state.clipboard_history_state().listeners.contains(manager);

        match request {
            ironland_clipboard_history_manager_v1::Request::Remove { id } => {
                if !is_listener {
                    return;
                }
                let history = state.clipboard_history_state();
                if let Some(pos) = history.entries.iter().position(|e| e.id == id) {
                    history.entries.remove(pos);
                    broadcast_removed(state, id);
                }
            }
            ironland_clipboard_history_manager_v1::Request::RemoveAll => {
                if !is_listener {
                    return;
                }
                state.clipboard_history_state().entries.clear();
                broadcast_cleared(state);
            }
            ironland_clipboard_history_manager_v1::Request::Receive { id, mime_type, fd } => {
                if !is_listener {
                    return;
                }
                send_content(state, id, &mime_type, fd);
            }
            ironland_clipboard_history_manager_v1::Request::ReceiveThumbnail { id, fd } => {
                if !is_listener {
                    return;
                }
                send_thumbnail(state, id, fd);
            }
            ironland_clipboard_history_manager_v1::Request::Destroy => {}
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandClipboardHistoryManagerV1) {
        let history = state.clipboard_history_state();
        history.listeners.retain(|l| l != resource);
        history.pending.retain(|(_, r)| r != resource);
    }
}

/// Writes entry `id`'s content to `fd` if `mime_type` names one of the mime
/// types it was captured with - see the protocol's `receive` request doc.
/// Runs the actual write on a short-lived thread since `fd` is normally the
/// write end of a pipe the requesting client is reading from at its own
/// pace, and there's no reason to let a slow reader stall the compositor;
/// the thread touches no compositor state, only a cloned copy of the
/// entry's text, so this needs no synchronization back into the event loop.
fn send_content<D: ClipboardHistoryHandler>(state: &mut D, id: u32, mime_type: &str, fd: OwnedFd) {
    let bytes = state
        .clipboard_history_state()
        .entries
        .iter()
        .find(|e| e.id == id && e.mime_types.iter().any(|m| m == mime_type))
        .map(|e| e.content.dedup_key().to_vec());

    std::thread::spawn(move || {
        if let Some(bytes) = bytes {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(&bytes);
        }
    });
}

/// Writes entry `id`'s thumbnail PNG to `fd` - see the protocol's
/// `receive_thumbnail` request doc. `fd` is simply dropped (closed) if `id`
/// names no currently-known entry or a text one with no thumbnail. Same
/// off-thread rationale as [`send_content`].
fn send_thumbnail<D: ClipboardHistoryHandler>(state: &mut D, id: u32, fd: OwnedFd) {
    let thumbnail = state.clipboard_history_state().entries.iter().find(|e| e.id == id).and_then(|e| {
        match &e.content {
            EntryContent::Image { thumbnail, .. } => Some(thumbnail.clone()),
            EntryContent::Text(_) => None,
        }
    });

    std::thread::spawn(move || {
        if let Some(thumbnail) = thumbnail {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(&thumbnail);
        }
    });
}
