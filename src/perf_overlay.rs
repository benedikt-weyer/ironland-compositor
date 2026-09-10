//! Per-output FPS overlay and frame stutter analytics (see
//! [`crate::config::PerformanceSettings`]). Both backends keep one
//! [`FrameStats`] per output (see `AnvilState::perf_stats`), call
//! [`FrameStats::record_frame`] once per successfully rendered/presented
//! frame, and - when `performance.fps_overlay` is on - draw the buffer from
//! [`overlay_buffer`] the same way the launcher/workspace-switcher overlays
//! are drawn (a [`MemoryRenderBuffer`] pushed as `CustomRenderElements::Overlay`).
//!
//! This intentionally doesn't reuse the old anvil-derived `FpsElement`/
//! `debug` cargo feature in `drawing.rs`/`render.rs`: that one is a
//! compile-time-only digit texture with no notion of frame time or
//! stutters, and isn't built into normal (non-`debug`-feature) binaries at
//! all, so it can't be toggled from `config.toml`/Nexus/`ironlandctl` like
//! every other user-facing setting.

use std::{collections::VecDeque, time::{Duration, Instant}};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Point, Size, Transform},
};

use crate::{
    config::{OverlayPosition, PerformanceSettings},
    font::Canvas,
};

/// How many past frame intervals are kept for the rolling average FPS/last
/// frame time shown in the overlay. At 60Hz this is ~4 seconds of history;
/// at higher refresh rates it's proportionally shorter, which is fine since
/// it only feeds a live readout, not long-term analytics.
const HISTORY_LEN: usize = 240;

/// How long a stutter counts towards the overlay's "recent" severity color,
/// independent of `HISTORY_LEN` (which is frame-count-based and would
/// otherwise cover a wildly different wall-clock span at 30Hz vs. 240Hz).
const RECENT_WINDOW: Duration = Duration::from_secs(5);

/// Frame-timing history and stutter counters for one output. Not tied to any
/// particular backend: both `udev.rs` (one per CRTC) and `winit.rs` (one,
/// for the single nested output) drive it the same way.
#[derive(Debug, Default)]
pub struct FrameStats {
    frame_times: VecDeque<Duration>,
    last_frame_at: Option<Instant>,
    /// Total stutters (frame time over threshold) since this output appeared.
    stutter_count: u64,
    /// Timestamps of stutters within `RECENT_WINDOW`, oldest first.
    recent_stutters: VecDeque<Instant>,
}

/// Result of [`FrameStats::record_frame`]: `Some(frame_time)` if the just-
/// recorded frame counted as a stutter (its interval exceeded the threshold
/// passed in), `None` otherwise. The very first frame recorded (no previous
/// timestamp to measure an interval from) is never a stutter.
pub type Stutter = Option<Duration>;

impl FrameStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a frame that just finished rendering/presenting at `now`,
    /// against `threshold` (see [`stutter_threshold`]). Returns the frame's
    /// interval if it counted as a stutter, for the caller to log.
    pub fn record_frame(&mut self, now: Instant, threshold: Duration) -> Stutter {
        let prev = self.last_frame_at.replace(now)?;
        let frame_time = now.saturating_duration_since(prev);

        if self.frame_times.len() >= HISTORY_LEN {
            self.frame_times.pop_front();
        }
        self.frame_times.push_back(frame_time);

        while let Some(&oldest) = self.recent_stutters.front() {
            if now.saturating_duration_since(oldest) > RECENT_WINDOW {
                self.recent_stutters.pop_front();
            } else {
                break;
            }
        }

        if frame_time > threshold {
            self.stutter_count += 1;
            self.recent_stutters.push_back(now);
            Some(frame_time)
        } else {
            None
        }
    }

    /// Average FPS over the recorded history, or `0.0` before at least two
    /// frames have been recorded.
    pub fn avg_fps(&self) -> f64 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        let total: Duration = self.frame_times.iter().sum();
        let mean = total.as_secs_f64() / self.frame_times.len() as f64;
        if mean > 0.0 { 1.0 / mean } else { 0.0 }
    }

    /// The most recently recorded frame's interval, in milliseconds.
    pub fn last_frame_ms(&self) -> f64 {
        self.frame_times.back().map_or(0.0, Duration::as_secs_f64) * 1000.0
    }

    /// Total stutters since this output appeared.
    pub fn stutter_count(&self) -> u64 {
        self.stutter_count
    }

    /// Stutters within the last [`RECENT_WINDOW`], used to color the
    /// overlay so an ongoing stutter streak is visible at a glance rather
    /// than buried in a lifetime counter.
    pub fn recent_stutter_count(&self) -> usize {
        self.recent_stutters.len()
    }
}

/// The frame-time threshold above which a frame counts as a "stutter":
/// `settings.stutter_threshold_ms` if set, otherwise 1.5x the expected
/// frame time for `refresh_mhz` (falling back to 60Hz if that isn't known).
pub fn stutter_threshold(settings: &PerformanceSettings, refresh_mhz: Option<i32>) -> Duration {
    if settings.stutter_threshold_ms > 0.0 {
        return Duration::from_secs_f32(settings.stutter_threshold_ms / 1000.0);
    }
    let hz = refresh_mhz
        .map(|mhz| mhz as f64 / 1000.0)
        .filter(|hz| *hz > 1.0)
        .unwrap_or(60.0);
    Duration::from_secs_f64(1.5 / hz)
}

/// Caches the FPS overlay's rasterized texture across frames, rebuilding it
/// from [`FrameStats`] only every `config.performance.fps_overlay_interval_ms`
/// (see [`OverlayCache::buffer`]) rather than on every single frame -
/// rebuilding it every frame does real, avoidable work (allocating a canvas
/// and rasterizing bitmap text) purely for a number a human glances at, not
/// something that needs to be legible at the output's own refresh rate.
/// Frame timing itself (and stutter detection) is unaffected - only the
/// overlay's *drawn* text lags behind by up to the configured interval.
#[derive(Debug, Default)]
pub struct OverlayCache {
    built: Option<(MemoryRenderBuffer, Instant)>,
}

impl OverlayCache {
    /// Returns the cached overlay texture, rebuilding it from `stats` first
    /// if there's nothing cached yet, `interval` is zero (throttling
    /// disabled), or the cached texture is older than `interval`.
    pub fn buffer(&mut self, stats: &FrameStats, interval: Duration) -> &MemoryRenderBuffer {
        let stale = self
            .built
            .as_ref()
            .is_none_or(|(_, built_at)| interval.is_zero() || built_at.elapsed() >= interval);
        if stale {
            self.built = Some((overlay_buffer(stats), Instant::now()));
        }
        &self.built.as_ref().expect("just set above if it was missing").0
    }
}

const FONT_SCALE: i32 = 2;
const PADDING: i32 = 10;
const LINE_GAP: i32 = 4;
const COLOR_BACKGROUND: [u8; 4] = [20, 18, 16, 210];
const COLOR_GOOD: [u8; 4] = [140, 230, 140, 255];
const COLOR_WARN: [u8; 4] = [235, 200, 90, 255];
const COLOR_BAD: [u8; 4] = [235, 100, 100, 255];
const COLOR_LABEL: [u8; 4] = [200, 195, 190, 255];

fn severity_color(recent_stutters: usize) -> [u8; 4] {
    match recent_stutters {
        0 => COLOR_GOOD,
        1..=2 => COLOR_WARN,
        _ => COLOR_BAD,
    }
}

fn line_height() -> i32 {
    (crate::font::GLYPH_HEIGHT as i32) * FONT_SCALE + LINE_GAP
}

/// Logical size of the overlay, so callers can position it without first
/// rasterizing it (mirrors `drawing::workspace_overlay_size`).
pub fn overlay_size() -> Size<i32, Logical> {
    let lines = ["999 FPS", "999.9 MS", "STUTTERS 9999"];
    let width = lines
        .iter()
        .map(|line| Canvas::text_width(line, FONT_SCALE))
        .max()
        .unwrap_or(0)
        + PADDING * 2;
    let height = line_height() * lines.len() as i32 - LINE_GAP + PADDING * 2;
    Size::from((width, height))
}

/// Rasterizes the current stats into a small semi-transparent panel: FPS,
/// last frame time, and a stutter counter colored by how recently stutters
/// have been happening (green/amber/red).
pub fn overlay_buffer(stats: &FrameStats) -> MemoryRenderBuffer {
    let size = overlay_size();
    let mut canvas = Canvas::new(size.w as usize, size.h as usize, COLOR_BACKGROUND);

    let fps = stats.avg_fps().round() as i64;
    let frame_ms = stats.last_frame_ms();
    let color = severity_color(stats.recent_stutter_count());

    let mut y = PADDING;
    canvas.draw_text(PADDING, y, &format!("{fps} FPS"), FONT_SCALE, color);
    y += line_height();
    canvas.draw_text(
        PADDING,
        y,
        &format!("{frame_ms:.1} MS"),
        FONT_SCALE,
        COLOR_LABEL,
    );
    y += line_height();
    canvas.draw_text(
        PADDING,
        y,
        &format!("STUTTERS {}", stats.stutter_count()),
        FONT_SCALE,
        color,
    );

    MemoryRenderBuffer::from_slice(
        &canvas.pixels,
        Fourcc::Argb8888,
        (canvas.width as i32, canvas.height as i32),
        1,
        Transform::Normal,
        None,
    )
}

/// Top-left logical position to place the overlay at within an output of
/// `output_size`, anchored to `position` with a fixed screen-edge margin.
pub fn overlay_location(
    position: OverlayPosition,
    output_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    const MARGIN: i32 = 16;
    let size = overlay_size();
    let (x, y) = match position {
        OverlayPosition::TopLeft => (MARGIN, MARGIN),
        OverlayPosition::TopRight => (output_size.w - size.w - MARGIN, MARGIN),
        OverlayPosition::BottomLeft => (MARGIN, output_size.h - size.h - MARGIN),
        OverlayPosition::BottomRight => {
            (output_size.w - size.w - MARGIN, output_size.h - size.h - MARGIN)
        }
    };
    Point::from((x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_is_never_a_stutter() {
        let mut stats = FrameStats::new();
        let now = Instant::now();
        assert_eq!(stats.record_frame(now, Duration::from_millis(16)), None);
        assert_eq!(stats.stutter_count(), 0);
    }

    #[test]
    fn slow_frame_counts_as_a_stutter() {
        let mut stats = FrameStats::new();
        let t0 = Instant::now();
        stats.record_frame(t0, Duration::from_millis(16));
        let t1 = t0 + Duration::from_millis(50);
        let stutter = stats.record_frame(t1, Duration::from_millis(16));
        assert_eq!(stutter, Some(Duration::from_millis(50)));
        assert_eq!(stats.stutter_count(), 1);
        assert_eq!(stats.recent_stutter_count(), 1);
    }

    #[test]
    fn fast_frame_is_not_a_stutter() {
        let mut stats = FrameStats::new();
        let t0 = Instant::now();
        stats.record_frame(t0, Duration::from_millis(16));
        let t1 = t0 + Duration::from_millis(16);
        assert_eq!(stats.record_frame(t1, Duration::from_millis(20)), None);
        assert_eq!(stats.stutter_count(), 0);
    }

    #[test]
    fn avg_fps_matches_steady_interval() {
        let mut stats = FrameStats::new();
        let mut t = Instant::now();
        for _ in 0..10 {
            stats.record_frame(t, Duration::from_millis(1000));
            t += Duration::from_millis(20);
        }
        let fps = stats.avg_fps();
        assert!((fps - 50.0).abs() < 0.5, "expected ~50fps, got {fps}");
    }

    #[test]
    fn stutter_threshold_defaults_to_auto_1_5x_refresh() {
        let settings = PerformanceSettings {
            stutter_threshold_ms: 0.0,
            ..PerformanceSettings::default()
        };
        let threshold = stutter_threshold(&settings, Some(60_000));
        assert!((threshold.as_secs_f64() - 1.5 / 60.0).abs() < 1e-6);

        let threshold_no_refresh = stutter_threshold(&settings, None);
        assert!((threshold_no_refresh.as_secs_f64() - 1.5 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn stutter_threshold_honors_explicit_override() {
        let settings = PerformanceSettings {
            stutter_threshold_ms: 33.0,
            ..PerformanceSettings::default()
        };
        let threshold = stutter_threshold(&settings, Some(144_000));
        assert!((threshold.as_secs_f64() - 0.033).abs() < 1e-6);
    }

    #[test]
    fn overlay_cache_rebuilds_only_after_interval_elapses() {
        let stats = FrameStats::new();
        let mut cache = OverlayCache::default();

        cache.buffer(&stats, Duration::from_secs(10));
        let first_built_at = cache.built.as_ref().unwrap().1;

        cache.buffer(&stats, Duration::from_secs(10));
        let second_built_at = cache.built.as_ref().unwrap().1;

        assert_eq!(first_built_at, second_built_at, "shouldn't rebuild before the interval elapses");
    }

    #[test]
    fn overlay_cache_always_rebuilds_when_interval_is_zero() {
        let stats = FrameStats::new();
        let mut cache = OverlayCache::default();

        cache.buffer(&stats, Duration::ZERO);
        let first_built_at = cache.built.as_ref().unwrap().1;

        std::thread::sleep(Duration::from_millis(1));
        cache.buffer(&stats, Duration::ZERO);
        let second_built_at = cache.built.as_ref().unwrap().1;

        assert!(second_built_at > first_built_at, "interval 0 should disable the throttle");
    }
}
