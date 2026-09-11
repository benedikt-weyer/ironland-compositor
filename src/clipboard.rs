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
//! Only plain text is captured (**for now** - see the module's top-level
//! doc, i.e. this crate's `CLAUDE.md`, for the general "in-memory only, no
//! persistence" scope this shares with `crate::screencopy`'s grant table):
//! a copy that doesn't offer any `text/…`-ish mime type, or whose content
//! isn't valid UTF-8, is silently not captured, same as one exceeding
//! [`MAX_CAPTURE_BYTES`] or one that names the `x-kde-password-manager-hint`
//! mime type - a convention several other clipboard managers already
//! respect to avoid recording secrets copied out of a password manager. A
//! copy identical to an already-recorded entry's text moves that entry back
//! to most-recent instead of duplicating it.
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

use crate::ironland_protocols::clipboard_history::ironland_clipboard_history_manager_v1::{
    self, IronlandClipboardHistoryManagerV1,
};
use crate::permission_prompt::PromptKind;

/// How many entries [`ClipboardHistoryState`] keeps before dropping the
/// oldest - a bound so a very active clipboard doesn't grow this without
/// limit over a long session.
const MAX_ENTRIES: usize = 200;
/// A captured copy larger than this is discarded rather than stored -
/// clipboard text is normally far smaller, and this keeps one enormous
/// copy from dominating the in-memory history.
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
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

#[derive(Debug, Clone)]
struct ClipboardEntry {
    id: u32,
    mime_types: Vec<String>,
    text: String,
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
            dh.create_global::<D, IronlandClipboardHistoryManagerV1, _>(1, ManagerGlobalData);
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

fn send_initial_burst(entries: &VecDeque<ClipboardEntry>, resource: &IronlandClipboardHistoryManagerV1) {
    for entry in entries.iter().rev() {
        resource.entry(entry.id, entry.mime_types.join(" "), preview_of(&entry.text));
    }
}

/// Broadcasts `entry`'s current state to every granted listener - a fresh
/// capture, or an existing entry promoted back to most-recent.
fn broadcast_entry<D: ClipboardHistoryHandler>(state: &mut D, entry: &ClipboardEntry) {
    for listener in &state.clipboard_history_state().listeners {
        listener.entry(entry.id, entry.mime_types.join(" "), preview_of(&entry.text));
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

/// Records a freshly-read copy: dedupes against an existing entry with the
/// same text (moving it to most-recent instead of duplicating it), then
/// evicts the oldest entries past [`MAX_ENTRIES`]. Called once the pipe
/// [`capture`] set up has been fully read.
fn finish_capture<D: ClipboardHistoryHandler>(state: &mut D, mime_types: Vec<String>, bytes: Vec<u8>) {
    if bytes.is_empty() || bytes.len() > MAX_CAPTURE_BYTES {
        return;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };
    let text = text.trim_end_matches(['\n', '\r', '\0']).to_string();
    if text.is_empty() {
        return;
    }

    let history = state.clipboard_history_state();
    let id = if let Some(pos) = history.entries.iter().position(|e| e.text == text) {
        let existing = history.entries.remove(pos).unwrap();
        existing.id
    } else {
        let id = history.next_id;
        history.next_id = history.next_id.wrapping_add(1);
        id
    };
    let entry = ClipboardEntry { id, mime_types, text };
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
}

/// Entry point called from `AnvilState`'s `SelectionHandler::new_selection`
/// for every `SelectionTarget::Clipboard` change - see the module doc.
/// A no-op if `source` offers nothing worth capturing.
pub fn capture<D>(handle: &LoopHandle<'static, D>, seat: &Seat<D>, source: &SelectionSource)
where
    D: SeatHandler + DataDeviceHandler + ClipboardHistoryHandler + 'static,
{
    let mime_types = source.mime_types();
    let Some(mime_type) = pick_mime_type(&mime_types) else {
        return;
    };
    let Ok((read_fd, write_fd)) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK) else {
        return;
    };
    // The entry only ever ends up holding content actually read back in
    // `mime_type`, so that (not `source`'s full offered list) is what it
    // advertises - see `receive`'s doc in the protocol XML for why this
    // needs to stay accurate.
    let stored_mime_types = vec![mime_type.clone()];
    if request_data_device_client_selection(seat, mime_type, write_fd).is_err() {
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
                        if buf.len() > MAX_CAPTURE_BYTES {
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
                send_initial_burst(&state.clipboard_history_state().entries, &resource);
                state.clipboard_history_state().listeners.push(resource);
            }
            Some(false) => {
                resource.denied();
            }
            None => {
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
    let text = state
        .clipboard_history_state()
        .entries
        .iter()
        .find(|e| e.id == id && e.mime_types.iter().any(|m| m == mime_type))
        .map(|e| e.text.clone());

    std::thread::spawn(move || {
        if let Some(text) = text {
            let mut file = std::fs::File::from(fd);
            let _ = file.write_all(text.as_bytes());
        }
    });
}
