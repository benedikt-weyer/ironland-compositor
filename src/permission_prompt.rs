//! Server side of the `ironland-permission-prompt-v1` protocol (see
//! `crate::ironland_protocols::permission_prompt` for the generated bindings
//! and `protocols/ironland-permission-prompt-v1.xml` for the wire format).
//!
//! This is the compositor half of this compositor's screenshot permission
//! system: `src/bin/ironland_portal_screenshot.rs` (the
//! `org.freedesktop.impl.portal.Screenshot` backend) sends a `prompt`
//! request over the privileged capture socket (see `crate::screencopy`)
//! before it ever creates an `ext-image-copy-capture-v1` session for an app
//! it hasn't already got a persisted decision for; this module renders that
//! as an on-screen overlay (reusing the bitmap font from
//! `crate::font`/`crate::drawing`, the same way the launcher does) and
//! reports the user's Enter/Escape answer back over the protocol.
//!
//! Only one prompt is shown at a time; further `prompt` requests queue.
//! [`PermissionPromptManagerState::answer`] is the entry point
//! `crate::input_handler` calls on Enter/Escape while a prompt is showing.

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
}

#[derive(Debug)]
struct PendingPrompt {
    app_id: String,
    reason: String,
    resource: IronlandPermissionPromptV1,
}

const PROMPT_WIDTH: i32 = 520;
const PROMPT_PADDING: i32 = 18;
const FONT_SCALE: i32 = 2;
const LINE_HEIGHT: i32 = crate::font::GLYPH_HEIGHT as i32 * FONT_SCALE + 10;

const COLOR_BACKGROUND: [u8; 4] = [40, 30, 30, 245];
const COLOR_TITLE: [u8; 4] = [235, 230, 225, 255];
const COLOR_HINT: [u8; 4] = [200, 170, 120, 255];

/// State of the `ironland_permission_prompt_manager_v1` global: the queue of
/// prompts (front = currently showing) and the rasterized overlay for the
/// one currently showing, if any.
#[derive(Debug)]
pub struct PermissionPromptManagerState {
    global: GlobalId,
    queue: VecDeque<PendingPrompt>,
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
            1,
            ManagerGlobalData {
                filter: Box::new(filter),
            },
        );
        PermissionPromptManagerState {
            global,
            queue: VecDeque::new(),
            buffer: None,
            dirty: false,
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    /// Whether a prompt is currently showing (queue non-empty).
    pub fn is_visible(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Answers the currently-showing prompt (if any) and shows the next
    /// queued one, if any. Returns whether a prompt was actually answered,
    /// so callers (`input_handler`) know whether to consume the key.
    pub fn answer(&mut self, allow: bool) -> bool {
        let Some(prompt) = self.queue.pop_front() else {
            return false;
        };
        if allow {
            prompt.resource.allowed();
        } else {
            prompt.resource.denied();
        }
        self.dirty = true;
        self.buffer = None;
        true
    }

    /// The logical size of the overlay, for centering by callers.
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

    /// Returns the buffer to render, rebuilding it if the showing prompt
    /// changed since the last frame. `None` when nothing is showing.
    pub fn ensure_buffer(&mut self) -> Option<&MemoryRenderBuffer> {
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
        _manager: &IronlandPermissionPromptManagerV1,
        request: ironland_permission_prompt_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_permission_prompt_manager_v1::Request::Prompt { id, app_id, reason } => {
                let resource = data_init.init(id, PromptToken);
                let prompt_state = state.permission_prompt_state();
                prompt_state.queue.push_back(PendingPrompt {
                    app_id,
                    reason,
                    resource,
                });
                prompt_state.dirty = true;
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
        let was_front = prompt_state
            .queue
            .front()
            .is_some_and(|p| &p.resource == resource);
        prompt_state.queue.retain(|p| &p.resource != resource);
        if was_front {
            prompt_state.dirty = true;
            prompt_state.buffer = None;
        }
    }
}
