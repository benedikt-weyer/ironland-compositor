//! Real-time output capture backing `ext-image-capture-source-v1`/
//! `ext-image-copy-capture-v1` (both implemented by Smithay itself - see
//! `smithay::wayland::image_capture_source`/`image_copy_capture` - this
//! module only supplies the parts Smithay leaves to the compositor: the
//! actual pixel copy, and this compositor's own permission model).
//!
//! ## Permission model
//!
//! The `ext_output_image_capture_source_manager_v1`/
//! `ext_image_copy_capture_manager_v1` globals themselves are unrestricted
//! (any client can bind them and negotiate a session - session setup alone
//! never reads screen content), *unlike* most other wlroots-style
//! compositors' `wlr-screencopy-v1`, which usually stops there: this
//! compositor gates the one point that actually matters instead, moving
//! consent from the protocol layer to the pixel layer.
//!
//! Every time a client's `ext_image_copy_capture_frame_v1.capture` request
//! would actually read screen content
//! ([`ImageCopyCaptureHandler::frame`][state] in `crate::state`), the
//! requesting client is identified by [`client_identity`] - the executable
//! path behind its Wayland connection, resolved via the kernel (`SO_
//! PEERCRED` through [`Client::get_credentials`], then `/proc/<pid>/exe`),
//! never anything the client itself asserts. That path is looked up in
//! [`ScreencopyState`]'s persisted grant table:
//!
//! - A prior **Allow** completes the capture immediately.
//! - A prior **Deny**, or no decision yet, fails the capture
//!   ([`CaptureFailureReason::Unknown`]) without reading any pixels. With
//!   no decision on file, it additionally queues an on-screen prompt (see
//!   `crate::permission_prompt`) naming that executable path; the client's
//!   *next* capture attempt (after the user answers - most callers,
//!   including this compositor's own shell's screenshot tool and
//!   `ironland-portal-screenshot`, simply retry on failure) succeeds or
//!   fails accordingly, and the decision is remembered from then on.
//!
//! This means every capturer - this compositor's own shell included - goes
//! through the exact same gate; nothing is special-cased. The grant table
//! is keyed by resolved executable path (`$XDG_CONFIG_HOME/ironland-
//! compositor/capture-permissions.json`), which - unlike a client-supplied
//! `app_id` string - cannot be spoofed by the client itself.
//!
//! [state]: crate::state
//!
//! `ironland-portal-screenshot` (the `org.freedesktop.impl.portal.
//! Screenshot` xdg-desktop-portal backend used by sandboxed/third-party
//! apps) is just another caller of this same gate: it runs through its own
//! executable path like anyone else, with its *own*, separate per-`app_id`
//! prompt/grant layer on top (see that binary's module doc) for
//! distinguishing between the different apps it captures on behalf of.
//!
//! ## Pixel capture
//!
//! [`fulfill`] is the actual capture: given pixels already sitting in a
//! readable framebuffer, it reads them back via
//! [`ExportMem::copy_framebuffer`] and copies them into every pending
//! capture frame's client-provided shm buffer. Both backends call it
//! immediately after rendering an output's frame, but get there
//! differently:
//!
//! - `crate::winit`'s bound framebuffer is directly readable, so it calls
//!   [`fulfill`] straight away.
//! - `crate::udev`'s DRM/KMS compositor may instead have scanned a client's
//!   buffer out directly (direct scanout) without ever compositing it into
//!   a readable framebuffer, so `crate::udev::capture_udev_frame` first
//!   re-composites the same `RenderFrameResult` into an offscreen
//!   renderbuffer via `RenderFrameResult::blit_frame_result` (which blits
//!   the scanout plane's dmabuf in like any other element), then calls
//!   [`fulfill`] on that.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::{ExportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::utils::{Buffer as BufferCoord, Rectangle, Size, Transform};
use smithay::wayland::image_copy_capture::{CaptureFailureReason, Frame, Session, SessionRef};
use smithay::wayland::shm::with_buffer_contents_mut;

/// Compositor-side state for the image-copy-capture pipeline: sessions kept
/// alive for their duration, capture requests waiting on the next rendered
/// frame of their output, and the persisted per-executable capture grants
/// gating them - see the module doc.
#[derive(Debug, Default)]
pub struct ScreencopyState {
    /// Owned sessions, kept alive as long as the client's session object is;
    /// see `ImageCopyCaptureHandler::new_session`/`session_destroyed` in
    /// `crate::state`.
    pub sessions: Vec<Session>,
    /// Frame captures requested (`ext_image_copy_capture_frame_v1.capture`)
    /// but not yet fulfilled, keyed by output name. Drained by [`fulfill`]
    /// after the next successful render of that output.
    pending: HashMap<String, Vec<Frame>>,
    /// Per-executable-path capture decisions; see [`Self::grant`]/
    /// [`Self::set_grant`].
    grants: HashMap<String, bool>,
}

impl ScreencopyState {
    /// Loads persisted grants from
    /// `$XDG_CONFIG_HOME/ironland-compositor/capture-permissions.json`. A
    /// missing or unreadable file is treated as "no decisions yet", not an
    /// error - matches this compositor's other config/store loaders (see
    /// e.g. `crate::config::Config::load`).
    pub fn load() -> Self {
        let grants = std::fs::read_to_string(grants_path())
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default();
        ScreencopyState {
            grants,
            ..Default::default()
        }
    }

    pub fn queue_frame(&mut self, output_name: String, frame: Frame) {
        self.pending.entry(output_name).or_default().push(frame);
    }

    /// Takes every capture request still waiting on `output`'s next
    /// rendered frame, for [`fulfill`] to complete.
    pub fn take_pending(&mut self, output: &Output) -> Vec<Frame> {
        self.pending.remove(&output.name()).unwrap_or_default()
    }

    pub fn has_pending(&self, output: &Output) -> bool {
        self.pending
            .get(&output.name())
            .is_some_and(|frames| !frames.is_empty())
    }

    /// Drops the `Session` matching `destroyed` (if any is currently owned),
    /// letting its `Drop` impl stop it and fail any frames still active on
    /// it. Called from `ImageCopyCaptureHandler::session_destroyed`.
    pub fn forget_session(&mut self, destroyed: &SessionRef) {
        self.sessions.retain(|session| session != destroyed);
    }

    /// The persisted decision for `subject` (an executable path from
    /// [`client_identity`]), if any.
    pub fn grant(&self, subject: &str) -> Option<bool> {
        self.grants.get(subject).copied()
    }

    /// Records and persists a decision for `subject`. Called back via
    /// `PermissionPromptHandler::capture_grant_resolved` once the user
    /// answers a prompt [`PermissionPromptManagerState::queue_internal`]
    /// queued (see `crate::state`'s impl of that trait).
    ///
    /// [`PermissionPromptManagerState::queue_internal`]: crate::permission_prompt::PermissionPromptManagerState::queue_internal
    pub fn set_grant(&mut self, subject: String, allowed: bool) {
        self.grants.insert(subject, allowed);
        let path = grants_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.grants) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn grants_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string())).join(".config")
        });
    config_home
        .join("ironland-compositor")
        .join("capture-permissions.json")
}

/// Identifies the executable behind `client`'s Wayland connection, for
/// [`ScreencopyState`]'s grant table - see the module doc for why this
/// (rather than any client-supplied string) is what capture decisions are
/// keyed and prompted on. Resolved via the kernel (`SO_PEERCRED`, wrapped by
/// [`Client::get_credentials`]) and `/proc/<pid>/exe`, so it can't be
/// spoofed by the client itself. Falls back to a placeholder (still safe to
/// prompt/gate on, just not a meaningful path) if either step fails - e.g.
/// the process has already exited, or `/proc` isn't available.
pub fn client_identity(dh: &DisplayHandle, client: &Client) -> String {
    client
        .get_credentials(dh)
        .ok()
        .and_then(|creds| std::fs::read_link(format!("/proc/{}/exe", creds.pid)).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "an unidentified application".to_string())
}

/// The size (in buffer/physical pixels) `ext-image-copy-capture-v1` buffer
/// constraints and captures use for `output` - its current mode's size,
/// unaffected by logical scale.
pub fn output_buffer_size(output: &Output) -> Option<Size<i32, BufferCoord>> {
    let mode = output.current_mode()?;
    Some(
        mode.size
            .to_logical(1)
            .to_buffer(1, smithay::utils::Transform::Normal),
    )
}

/// Completes every frame in `pending` using the pixels currently in
/// `framebuffer`, which must already hold `size`'s worth of freshly
/// rendered, readable content (a backend calls this immediately after its
/// normal per-output render, while its renderer is still bound to that
/// output). No-ops (skips the readback entirely) if `pending` is empty.
pub fn fulfill<R>(
    pending: Vec<Frame>,
    renderer: &mut R,
    framebuffer: &R::Framebuffer<'_>,
    size: Size<i32, BufferCoord>,
    presented: Duration,
) where
    R: Renderer + ExportMem,
{
    if pending.is_empty() {
        return;
    }

    let region = Rectangle::from_size(size);
    let mapping = match renderer.copy_framebuffer(framebuffer, region, Fourcc::Argb8888) {
        Ok(mapping) => mapping,
        Err(_) => {
            for frame in pending {
                frame.fail(CaptureFailureReason::Unknown);
            }
            return;
        }
    };
    let bytes = match renderer.map_texture(&mapping) {
        Ok(bytes) => bytes,
        Err(_) => {
            for frame in pending {
                frame.fail(CaptureFailureReason::Unknown);
            }
            return;
        }
    };

    for frame in pending {
        if write_shm_argb8888(&frame.buffer(), bytes, size.w as u32, size.h as u32) {
            frame.success(Transform::Normal, None, presented);
        } else {
            frame.fail(CaptureFailureReason::BufferConstraints);
        }
    }
}

/// Copies a tightly-packed `Argb8888` source (as produced by
/// [`ExportMem::map_texture`]) into a client's shm-backed `wl_buffer`, row
/// by row to account for the two potentially differing strides. Returns
/// `false` (leaving the buffer untouched) if either is too small for
/// `width`x`height`.
fn write_shm_argb8888(buffer: &WlBuffer, src: &[u8], width: u32, height: u32) -> bool {
    let src_stride = width as usize * 4;
    let rows = height as usize;
    if src.len() < src_stride * rows {
        return false;
    }

    with_buffer_contents_mut(buffer, |ptr, len, data| {
        let dst_stride = data.stride as usize;
        let offset = data.offset as usize;
        if data.width as u32 != width
            || data.height as u32 != height
            || len < offset + dst_stride * rows
        {
            return false;
        }

        // Safety: `with_buffer_contents_mut` guarantees `ptr` is valid for
        // `len` bytes for the duration of this closure.
        let dst = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset), len - offset) };
        for row in 0..rows {
            let src_row = &src[row * src_stride..row * src_stride + src_stride];
            let dst_row = &mut dst[row * dst_stride..row * dst_stride + src_stride];
            dst_row.copy_from_slice(src_row);
        }
        true
    })
    .unwrap_or(false)
}
