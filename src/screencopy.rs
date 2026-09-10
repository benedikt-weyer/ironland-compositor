//! Real-time output capture backing `ext-image-capture-source-v1`/
//! `ext-image-copy-capture-v1` (both implemented by Smithay itself - see
//! `smithay::wayland::image_capture_source`/`image_copy_capture` - this
//! module only supplies the parts Smithay leaves to the compositor: the
//! actual pixel copy, and this compositor's own permission model).
//!
//! ## Permission model
//!
//! Screen content is sensitive, so access is gated at two layers:
//!
//! 1. **Protocol-level**: the `ext_output_image_capture_source_manager_v1`
//!    and `ext_image_copy_capture_manager_v1` globals are only advertised to
//!    clients connected through a second, privileged Wayland socket (see
//!    [`ScreencopyState`]'s doc and `AnvilState::init`'s
//!    `capture_socket_name`) - not the regular one every app connects
//!    through. In practice the only client that ever connects there is this
//!    compositor's own `ironland-portal-screenshot` binary (the
//!    `org.freedesktop.impl.portal.Screenshot` xdg-desktop-portal backend),
//!    so ordinary applications can never reach these globals at all, no
//!    matter what they ask xdg-desktop-portal for.
//! 2. **Per-app grant**: within that privileged client, `Screenshot()`
//!    requests are further gated per requesting `app_id` - see the module
//!    doc on `src/bin/ironland_portal_screenshot.rs` for the prompt/persist
//!    flow, which uses `crate::permission_prompt` to ask the user before
//!    ever creating a source/session for an app it hasn't already got a
//!    decision on file for.
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
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::{ExportMem, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::utils::{Buffer as BufferCoord, Rectangle, Size, Transform};
use smithay::wayland::image_copy_capture::{CaptureFailureReason, Frame, Session, SessionRef};
use smithay::wayland::shm::with_buffer_contents_mut;

/// Compositor-side state for the image-copy-capture pipeline: sessions kept
/// alive for their duration, and capture requests waiting on the next
/// rendered frame of their output.
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
}

impl ScreencopyState {
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
