//! Server side of the `ironland-permission-prompt-v1` protocol (see
//! `crate::ironland_protocols::permission_prompt` for the generated bindings
//! and `protocols/ironland-permission-prompt-v1.xml` for the wire format).
//!
//! Two kinds of prompt share this one queue, distinguished by
//! [`PromptTarget`]:
//!
//! - **Wire-requested**: a privileged *requester* client (in practice,
//!   `ironland-portal-screenshot`) calls the protocol's `prompt` request and
//!   waits on the returned object's `allowed`/`denied` event.
//! - **Internal**: the compositor itself queues one via
//!   [`PermissionPromptManagerState::queue_internal`], with no requester
//!   object at all - tagged with a [`PromptKind`] so [`PermissionPromptHandler::internal_prompt_resolved`]
//!   knows which in-memory grant table (`crate::screencopy`'s, gating
//!   direct non-portal screen capture, or `crate::clipboard`'s, gating
//!   clipboard-history access) the answer belongs to. Resolving one of
//!   these calls back via [`PermissionPromptHandler::internal_prompt_resolved`]
//!   instead of firing a protocol event.
//!
//! Separately, a privileged *renderer* client (in practice, this
//! compositor's companion shell, caelestia-shell-iron) calls `set_renderer`
//! once at startup to take over showing every prompt - of either kind - as
//! `show`/`cancel` events, themed and laid out however it likes, and
//! answers them via `answer`.
//!
//! If no renderer is registered - the shell isn't running yet, or is an
//! older build without this protocol - [`PermissionPromptManagerState`]
//! falls back to drawing a minimal prompt of its own (reusing the bitmap
//! font from `crate::font`/`crate::drawing`, the same way the launcher
//! does) and reading the user's Enter/Escape answer directly in
//! `crate::input_handler`, so a prompt is never simply lost. This fallback
//! is fully inert once a renderer registers - see [`PermissionPromptManagerState::is_visible`]/
//! [`PermissionPromptManagerState::ensure_buffer`]/[`answer`].

use std::collections::VecDeque;

use smithay::backend::allocator::Fourcc;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::utils::Transform;
use smithay::wayland::Dispatch2;
use smithay::wayland::GlobalDispatch2;
use smithay::{backend::renderer::element::memory::MemoryRenderBuffer, utils::Size};

use crate::font::Canvas;
use crate::ironland_protocols::permission_prompt::{
    ironland_permission_prompt_manager_v1::{self, IronlandPermissionPromptManagerV1},
    ironland_permission_prompt_v1::{self, IronlandPermissionPromptV1},
};

/// Implemented by the compositor state so this module can queue/answer
/// prompts without depending on `AnvilState` directly.
pub trait PermissionPromptHandler: 'static {
    fn permission_prompt_state(&mut self) -> &mut PermissionPromptManagerState;

    /// Called when an internally-queued prompt (see
    /// [`PermissionPromptManagerState::queue_internal`]) is resolved, with
    /// the same `kind`/`subject` it was queued under. There's no requester
    /// object to notify for these, unlike wire-requested prompts, so this is
    /// the only way to learn the answer.
    fn internal_prompt_resolved(&mut self, kind: PromptKind, subject: String, allowed: bool) {
        let _ = (kind, subject, allowed);
    }
}

/// Which in-memory grant table an internally-queued prompt (see
/// [`PermissionPromptManagerState::queue_internal`]) belongs to - carried
/// through to [`PermissionPromptHandler::internal_prompt_resolved`] so the
/// one shared prompt queue can back more than one such table without them
/// interfering (in particular, so a still-pending prompt for one table
/// doesn't dedupe-suppress a same-named subject's prompt for another).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Gates `crate::screencopy`'s direct, non-portal screen-capture grant
    /// table.
    Capture,
    /// Gates `crate::clipboard`'s clipboard-history access grant table.
    ClipboardHistory,
}

/// Who resolving a [`PendingPrompt`] notifies - see the module doc.
#[derive(Debug)]
enum PromptTarget {
    Requester(IronlandPermissionPromptV1),
    Internal(PromptKind),
}

#[derive(Debug)]
struct PendingPrompt {
    /// Opaque id handed to the renderer (see the protocol doc for why -
    /// renderer and requester are different client connections, so their
    /// own object ids for this prompt don't correspond).
    id: u32,
    /// For a wire-requested prompt, the requesting `app_id` as chosen by
    /// the (trusted) requester. For an internal one (see
    /// [`PromptTarget::Internal`]), doubles as the subject
    /// [`PermissionPromptHandler::internal_prompt_resolved`] is called back
    /// with - see [`PermissionPromptManagerState::queue_internal`].
    app_id: String,
    reason: String,
    target: PromptTarget,
}

const PROMPT_WIDTH: i32 = 520;
const PROMPT_PADDING: i32 = 18;
const FONT_SCALE: i32 = 2;
const LINE_HEIGHT: i32 = crate::font::GLYPH_HEIGHT as i32 * FONT_SCALE + 10;

const COLOR_BACKGROUND: [u8; 4] = [40, 30, 30, 245];
const COLOR_TITLE: [u8; 4] = [235, 230, 225, 255];
const COLOR_HINT: [u8; 4] = [200, 170, 120, 255];

/// State of the `ironland_permission_prompt_manager_v1` global.
#[derive(Debug)]
pub struct PermissionPromptManagerState {
    global: GlobalId,
    next_id: u32,
    queue: VecDeque<PendingPrompt>,
    /// The client that called `set_renderer`, if any - see the module doc.
    /// While set, every prompt is described to it via `show`/`cancel`
    /// instead of the fallback bitmap-font overlay, and only its `answer`
    /// requests resolve them.
    renderer: Option<IronlandPermissionPromptManagerV1>,
    /// Fallback overlay buffer, only ever populated while `renderer` is
    /// `None`.
    buffer: Option<MemoryRenderBuffer>,
    dirty: bool,
}

impl PermissionPromptManagerState {
    /// Registers the global, visible only to clients for which `filter`
    /// returns `true` - in practice, clients connected through the
    /// privileged capture socket (see `crate::screencopy`'s module doc),
    /// the same gate `ext-image-capture-source-v1`/
    /// `ext-image-copy-capture-v1` use.
    pub fn new<D, F>(dh: &DisplayHandle, filter: F) -> Self
    where
        D: GlobalDispatch<IronlandPermissionPromptManagerV1, ManagerGlobalData> + 'static,
        F: Fn(&Client) -> bool + Send + Sync + 'static,
    {
        let global = dh.create_global::<D, IronlandPermissionPromptManagerV1, _>(
            2,
            ManagerGlobalData {
                filter: Box::new(filter),
            },
        );
        PermissionPromptManagerState {
            global,
            next_id: 1,
            queue: VecDeque::new(),
            renderer: None,
            buffer: None,
            dirty: false,
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    /// Queues a prompt with no wire requester behind it - see
    /// [`PromptTarget::Internal`] and the module doc. A no-op if `subject`
    /// already has an internal prompt of the same `kind` in flight, so a
    /// storm of requests from the same still-undecided caller (e.g.
    /// repeated capture attempts - see `crate::screencopy`) doesn't queue a
    /// prompt per attempt.
    pub fn queue_internal(&mut self, kind: PromptKind, subject: String, reason: String) {
        let already_queued = self.queue.iter().any(
            |p| matches!(p.target, PromptTarget::Internal(k) if k == kind) && p.app_id == subject,
        );
        if already_queued {
            return;
        }

        let prompt_id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        match &self.renderer {
            Some(renderer) => renderer.show(prompt_id, subject.clone(), reason.clone()),
            None => self.dirty = true,
        }
        self.queue.push_back(PendingPrompt {
            id: prompt_id,
            app_id: subject,
            reason,
            target: PromptTarget::Internal(kind),
        });
    }

    /// Whether the fallback overlay should be showing (queue non-empty and
    /// no renderer has taken over).
    pub fn is_visible(&self) -> bool {
        self.renderer.is_none() && !self.queue.is_empty()
    }

    /// The logical size of the fallback overlay, for centering by callers.
    pub fn logical_size(&self) -> Size<i32, smithay::utils::Logical> {
        Size::from((PROMPT_WIDTH, PROMPT_PADDING * 2 + LINE_HEIGHT * 2))
    }

    fn rasterize(front: &PendingPrompt) -> Canvas {
        let width = PROMPT_WIDTH;
        let height = PROMPT_PADDING * 2 + LINE_HEIGHT * 2;
        let mut canvas = Canvas::new(width as usize, height as usize, COLOR_BACKGROUND);

        let title = format!("{} wants to {}", front.app_id, front.reason);
        canvas.draw_text(
            PROMPT_PADDING,
            PROMPT_PADDING,
            &title,
            FONT_SCALE,
            COLOR_TITLE,
        );
        canvas.draw_text(
            PROMPT_PADDING,
            PROMPT_PADDING + LINE_HEIGHT,
            "[Enter] Allow      [Esc] Deny",
            FONT_SCALE,
            COLOR_HINT,
        );

        canvas
    }

    /// Returns the fallback overlay buffer, rebuilding it if the showing
    /// prompt changed since the last frame. `None` whenever there's
    /// nothing to fall back for (a renderer is registered, or the queue is
    /// empty).
    pub fn ensure_buffer(&mut self) -> Option<&MemoryRenderBuffer> {
        if self.renderer.is_some() {
            return None;
        }
        let front = self.queue.front()?;
        if self.dirty || self.buffer.is_none() {
            let canvas = Self::rasterize(front);
            self.buffer = Some(MemoryRenderBuffer::from_slice(
                &canvas.pixels,
                Fourcc::Argb8888,
                (canvas.width as i32, canvas.height as i32),
                1,
                Transform::Normal,
                None,
            ));
            self.dirty = false;
        }
        self.buffer.as_ref()
    }
}

/// Notifies `prompt`'s target of `allow` - firing a protocol event for a
/// wire-requested prompt, or calling back into `state` for an internal one.
/// A free function (rather than a method) so it can reach
/// [`PermissionPromptHandler::internal_prompt_resolved`] on `state`.
fn send_answer<D: PermissionPromptHandler>(state: &mut D, prompt: PendingPrompt, allow: bool) {
    match prompt.target {
        PromptTarget::Requester(resource) => {
            if allow {
                resource.allowed();
            } else {
                resource.denied();
            }
        }
        PromptTarget::Internal(kind) => state.internal_prompt_resolved(kind, prompt.app_id, allow),
    }
}

/// Answers the currently-showing prompt via the fallback overlay (if any)
/// and shows the next queued one, if any. A no-op (returns `false`) once a
/// renderer has registered - it answers via the protocol's `answer` request
/// instead (see the `Dispatch2` impl below). Returns whether a prompt was
/// actually answered, so callers (`input_handler`) know whether to consume
/// the key.
pub fn answer<D: PermissionPromptHandler>(state: &mut D, allow: bool) -> bool {
    let prompt_state = state.permission_prompt_state();
    if prompt_state.renderer.is_some() {
        return false;
    }
    let Some(prompt) = prompt_state.queue.pop_front() else {
        return false;
    };
    prompt_state.dirty = true;
    prompt_state.buffer = None;
    send_answer(state, prompt, allow);
    true
}

/// Resolves the prompt named `id` (the protocol's `answer` request's job).
/// A no-op if `id` doesn't name a pending prompt.
fn resolve<D: PermissionPromptHandler>(state: &mut D, id: u32, allow: bool) {
    let prompt_state = state.permission_prompt_state();
    let Some(pos) = prompt_state.queue.iter().position(|p| p.id == id) else {
        return;
    };
    let prompt = prompt_state.queue.remove(pos).unwrap();
    send_answer(state, prompt, allow);
}

/// Global data for the `ironland_permission_prompt_manager_v1` global: the
/// client filter it's gated behind.
#[allow(missing_debug_implementations)]
pub struct ManagerGlobalData {
    filter: Box<dyn Fn(&Client) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ManagerGlobalData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerGlobalData").finish_non_exhaustive()
    }
}

/// User data attached to a bound `ironland_permission_prompt_manager_v1`
/// resource (nothing to carry).
#[derive(Debug)]
pub struct ManagerToken;

/// User data attached to an `ironland_permission_prompt_v1` resource
/// (nothing to carry beyond what's already queued).
#[derive(Debug)]
pub struct PromptToken;

impl<D> GlobalDispatch2<IronlandPermissionPromptManagerV1, D> for ManagerGlobalData
where
    D: PermissionPromptHandler
        + Dispatch<IronlandPermissionPromptManagerV1, ManagerToken>
        + Dispatch<IronlandPermissionPromptV1, PromptToken>,
{
    fn bind(
        &self,
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandPermissionPromptManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ManagerToken);
    }

    fn can_view(&self, client: &Client) -> bool {
        (self.filter)(client)
    }
}

impl<D> Dispatch2<IronlandPermissionPromptManagerV1, D> for ManagerToken
where
    D: PermissionPromptHandler + Dispatch<IronlandPermissionPromptV1, PromptToken>,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        manager: &IronlandPermissionPromptManagerV1,
        request: ironland_permission_prompt_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_permission_prompt_manager_v1::Request::Prompt { id, app_id, reason } => {
                let resource = data_init.init(id, PromptToken);
                let prompt_state = state.permission_prompt_state();
                let prompt_id = prompt_state.next_id;
                prompt_state.next_id = prompt_state.next_id.wrapping_add(1);
                match &prompt_state.renderer {
                    Some(renderer) => renderer.show(prompt_id, app_id.clone(), reason.clone()),
                    None => prompt_state.dirty = true,
                }
                prompt_state.queue.push_back(PendingPrompt {
                    id: prompt_id,
                    app_id,
                    reason,
                    target: PromptTarget::Requester(resource),
                });
            }
            ironland_permission_prompt_manager_v1::Request::SetRenderer => {
                let prompt_state = state.permission_prompt_state();
                prompt_state.renderer = Some(manager.clone());
                // Flush every already-queued prompt (including any the
                // fallback overlay was mid-showing) to the new renderer,
                // and retire the fallback since it's no longer relevant.
                for prompt in &prompt_state.queue {
                    manager.show(prompt.id, prompt.app_id.clone(), prompt.reason.clone());
                }
                prompt_state.buffer = None;
                prompt_state.dirty = false;
            }
            ironland_permission_prompt_manager_v1::Request::Answer { prompt_id, allowed } => {
                if state.permission_prompt_state().renderer.as_ref() != Some(manager) {
                    return;
                }
                resolve(state, prompt_id, allowed != 0);
            }
            ironland_permission_prompt_manager_v1::Request::Destroy => {}
        }
    }
}

impl<D: PermissionPromptHandler> Dispatch2<IronlandPermissionPromptV1, D> for PromptToken {
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &IronlandPermissionPromptV1,
        request: ironland_permission_prompt_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let ironland_permission_prompt_v1::Request::Destroy = request;
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandPermissionPromptV1) {
        let prompt_state = state.permission_prompt_state();
        let Some(pos) = prompt_state.queue.iter().position(
            |p| matches!(&p.target, PromptTarget::Requester(r) if r == resource),
        ) else {
            return;
        };
        let was_front = pos == 0;
        let prompt = prompt_state.queue.remove(pos).unwrap();

        match &prompt_state.renderer {
            Some(renderer) => renderer.cancel(prompt.id),
            None if was_front => {
                prompt_state.dirty = true;
                prompt_state.buffer = None;
            }
            None => {}
        }
    }
}
