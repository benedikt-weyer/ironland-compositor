//! Server side of the `ironland-frame-capture-v1` protocol (see
//! `crate::ironland_protocols::frame_capture` for the generated bindings
//! and `protocols/ironland-frame-capture-v1.xml` for the wire format).
//!
//! A debugging aid, not a permanent user-facing feature: lets a client
//! (in practice the shell's Nexus settings) ask for a detailed per-stage
//! timing breakdown of the next N frames on every live output, to compare
//! against a suspected scheduling issue without guessing from aggregate
//! FPS numbers alone (see `crate::perf_overlay` for those).
//!
//! [`record_stage`] and [`record_frame`] are the two entry points -
//! `udev.rs`/`winit.rs` call them from the render path, but only while
//! [`is_capturing`] says a session is actually open, so this has zero cost
//! the rest of the time.

use std::{collections::HashMap, time::{Duration, Instant}};

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    backend::{ClientId, GlobalId},
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::ironland_protocols::frame_capture::{
    ironland_frame_capture_manager_v1::{self, IronlandFrameCaptureManagerV1},
    ironland_frame_capture_v1::{self, IronlandFrameCaptureV1},
};

/// How long a session is allowed to wait for every live output to deliver
/// `frame_count` frames before it's force-finished anyway - covers an
/// output that's asleep/disconnected for the whole session, so a client
/// waiting on `finished` never hangs forever.
const GRACE_PERIOD: Duration = Duration::from_secs(2);

/// Implemented by the compositor state so this module can record timing
/// without depending on `AnvilState` directly.
pub trait FrameCaptureHandler: 'static {
    fn frame_capture_state(&mut self) -> &mut FrameCaptureManagerState;
}

/// The pure bookkeeping for one capture session - how many frames per
/// output it still wants, how many each output has delivered so far, and
/// when it started (for [`GRACE_PERIOD`]) - kept separate from
/// [`CaptureSession`] so it's constructible and testable without a live
/// protocol resource.
#[derive(Debug)]
struct SessionProgress {
    frame_count: u32,
    per_output: HashMap<String, u32>,
    started_at: Instant,
}

impl SessionProgress {
    fn new(frame_count: u32) -> Self {
        SessionProgress {
            frame_count,
            per_output: HashMap::new(),
            started_at: Instant::now(),
        }
    }

    /// Whether every output that has reported *any* frame under this
    /// session has now reported `frame_count` of them, and at least
    /// `live_output_count` outputs have reported - i.e. every output live
    /// at completion time is accounted for, not just a subset that happens
    /// to have rendered first.
    fn complete(&self, live_output_count: usize) -> bool {
        self.per_output.len() >= live_output_count
            && self.per_output.values().all(|&count| count >= self.frame_count)
    }

    fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) > GRACE_PERIOD
    }
}

/// One in-progress capture session: the protocol object to notify, plus
/// its [`SessionProgress`].
#[derive(Debug)]
struct CaptureSession {
    resource: IronlandFrameCaptureV1,
    progress: SessionProgress,
}

/// State of the `ironland_frame_capture_manager_v1` global: every capture
/// session currently in progress, across every client.
#[derive(Debug)]
pub struct FrameCaptureManagerState {
    global: GlobalId,
    sessions: Vec<CaptureSession>,
}

impl FrameCaptureManagerState {
    pub fn new<D>(dh: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<IronlandFrameCaptureManagerV1, ManagerGlobalData> + 'static,
    {
        let global = dh.create_global::<D, IronlandFrameCaptureManagerV1, _>(1, ManagerGlobalData);
        FrameCaptureManagerState {
            global,
            sessions: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

/// Global data for the `ironland_frame_capture_manager_v1` global (nothing
/// to carry).
#[derive(Debug)]
pub struct ManagerGlobalData;

/// User data attached to a bound `ironland_frame_capture_manager_v1`
/// resource (nothing to carry - every started session lives in
/// [`FrameCaptureManagerState::sessions`]).
#[derive(Debug)]
pub struct ManagerToken;

/// User data attached to an `ironland_frame_capture_v1` resource (nothing
/// to carry beyond what's already in its [`CaptureSession`]).
#[derive(Debug)]
pub struct SessionToken;

/// Whether any capture session is currently open - callers on the render
/// hot path should check this before doing any per-stage timing work at
/// all, so a normal (non-capturing) frame pays nothing.
pub fn is_capturing<D: FrameCaptureHandler>(state: &mut D) -> bool {
    !state.frame_capture_state().sessions.is_empty()
}

fn as_micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

/// Records one named stage of one frame currently being captured on
/// `output`, for every session that's still open. `start`/`duration` are
/// offsets/spans from that frame's own start, converted to microseconds
/// (saturating - no captured stage is anywhere near `u32::MAX` us long).
pub fn record_stage<D: FrameCaptureHandler>(
    state: &mut D,
    output: &str,
    frame_index: u32,
    name: &str,
    start: Duration,
    duration: Duration,
) {
    let sessions = &state.frame_capture_state().sessions;
    if sessions.is_empty() {
        return;
    }
    for session in sessions {
        session.resource.stage(
            output.to_string(),
            frame_index,
            name.to_string(),
            as_micros(start),
            as_micros(duration),
        );
    }
}

/// Records that `output` just finished frame `frame_index` (the
/// [`crate::perf_overlay::FrameStats::record_frame`] result feeds
/// `frame_time`/`stutter` directly). Closes and removes any session that's
/// now complete (see [`CaptureSession::complete`]) or has run past
/// [`GRACE_PERIOD`], sending `finished` first.
///
/// `live_output_count` should be the number of outputs the compositor
/// currently knows about (e.g. `self.space.outputs().count()`), used to
/// tell "every output has reported" from "only the outputs that happen to
/// have rendered so far have reported".
pub fn record_frame<D: FrameCaptureHandler>(
    state: &mut D,
    output: &str,
    frame_index: u32,
    frame_time: Duration,
    stutter: bool,
    live_output_count: usize,
) {
    let state = state.frame_capture_state();
    if state.sessions.is_empty() {
        return;
    }
    let now = Instant::now();
    for session in &mut state.sessions {
        session.resource.frame(
            output.to_string(),
            frame_index,
            as_micros(frame_time),
            u32::from(stutter),
        );
        *session.progress.per_output.entry(output.to_string()).or_insert(0) += 1;
    }
    state.sessions.retain(|session| {
        if session.progress.complete(live_output_count) || session.progress.timed_out(now) {
            session.resource.finished();
            false
        } else {
            true
        }
    });
}

impl<D> GlobalDispatch2<IronlandFrameCaptureManagerV1, D> for ManagerGlobalData
where
    D: FrameCaptureHandler
        + Dispatch<IronlandFrameCaptureManagerV1, ManagerToken>
        + Dispatch<IronlandFrameCaptureV1, SessionToken>,
{
    fn bind(
        &self,
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<IronlandFrameCaptureManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ManagerToken);
    }
}

impl<D> Dispatch2<IronlandFrameCaptureManagerV1, D> for ManagerToken
where
    D: FrameCaptureHandler + Dispatch<IronlandFrameCaptureV1, SessionToken>,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _manager: &IronlandFrameCaptureManagerV1,
        request: ironland_frame_capture_manager_v1::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ironland_frame_capture_manager_v1::Request::Capture { id, frame_count } => {
                let resource = data_init.init(id, SessionToken);
                state.frame_capture_state().sessions.push(CaptureSession {
                    resource,
                    progress: SessionProgress::new(frame_count),
                });
            }
            ironland_frame_capture_manager_v1::Request::Destroy => {}
        }
    }
}

impl<D: FrameCaptureHandler> Dispatch2<IronlandFrameCaptureV1, D> for SessionToken {
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &IronlandFrameCaptureV1,
        request: ironland_frame_capture_v1::Request,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let ironland_frame_capture_v1::Request::Destroy = request;
        state
            .frame_capture_state()
            .sessions
            .retain(|session| &session.resource != resource);
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &IronlandFrameCaptureV1) {
        state
            .frame_capture_state()
            .sessions
            .retain(|session| &session.resource != resource);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_until_every_known_output_hits_frame_count() {
        let mut s = SessionProgress::new(3);
        s.per_output.insert("eDP-1".into(), 3);
        assert!(!s.complete(2), "second output hasn't reported at all yet");
        s.per_output.insert("HDMI-A-1".into(), 2);
        assert!(!s.complete(2), "second output hasn't hit frame_count yet");
        s.per_output.insert("HDMI-A-1".into(), 3);
        assert!(s.complete(2));
    }

    #[test]
    fn not_timed_out_immediately() {
        let s = SessionProgress::new(3);
        assert!(!s.timed_out(Instant::now()));
    }

    #[test]
    fn timed_out_past_grace_period() {
        let s = SessionProgress::new(3);
        assert!(s.timed_out(s.started_at + GRACE_PERIOD + Duration::from_millis(1)));
    }
}
