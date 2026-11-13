//! Allocation-free helpers for the capture -> encode hot path.
//!
//! Timestamps are in 100 ns units (Windows `TimeSpan` / Media Foundation
//! sample time), monotonic, from an arbitrary origin.

use std::time::Duration;

pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// Converts a capture timestamp into a zero-based sample time and decides
/// whether a frame is due at the target frame rate.
///
/// Capture APIs deliver frames at display refresh rate (or only on change).
/// The pacer keeps at most one frame per output interval and never invents
/// frames, so output is variable frame rate capped at `fps`.
#[derive(Clone, Copy, Debug)]
pub struct FramePacer {
    interval: i64,
    origin: Option<i64>,
    next_due: i64,
}

impl FramePacer {
    /// `fps` of 0 is treated as 1.
    pub fn new(fps: u32) -> Self {
        Self {
            interval: TICKS_PER_SECOND / i64::from(fps.max(1)),
            origin: None,
            next_due: 0,
        }
    }

    /// Shares the audio recorder QPC origin instead of rebasing the first frame.
    pub fn with_origin(fps: u32, origin: i64) -> Self {
        Self {
            origin: Some(origin),
            ..Self::new(fps)
        }
    }

    /// Nominal frame duration in 100 ns ticks.
    pub fn interval(&self) -> i64 {
        self.interval
    }

    /// Returns the zero-based sample time if the frame should be encoded,
    /// `None` if it arrives too early and should be skipped (not a drop).
    pub fn accept(&mut self, timestamp: i64) -> Option<i64> {
        let origin = *self.origin.get_or_insert(timestamp);
        let relative = timestamp - origin;
        if relative < 0 {
            return None;
        }
        // Quarter-interval tolerance absorbs capture jitter around refresh
        // edges without letting a 2x source through.
        let tolerance = self.interval / 4;
        if relative + tolerance < self.next_due {
            return None;
        }
        // Advance the deadline on the fixed grid. Re-anchor only after a real
        // stall (frame later than the following slot) instead of bursting to
        // catch up. Re-anchoring on every late-in-window frame pushed the grid
        // forward and lost frames from non-60 Hz sources (75 Hz gave 56 fps).
        self.next_due = if relative < self.next_due + self.interval {
            self.next_due + self.interval
        } else {
            relative + self.interval - tolerance
        };
        Some(relative)
    }
}

/// Counts events over fixed windows to report a current rate.
#[derive(Clone, Copy, Debug)]
pub struct RateMeter {
    window_start: Duration,
    count: u64,
    rate: f32,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateMeter {
    pub const fn new() -> Self {
        Self {
            window_start: Duration::ZERO,
            count: 0,
            rate: 0.0,
        }
    }

    pub fn record(&mut self) {
        self.count += 1;
    }

    /// Closes the window if at least `window` elapsed since it started.
    /// Returns `true` when a new rate was computed.
    pub fn update(&mut self, now: Duration, window: Duration) -> bool {
        let elapsed = now.saturating_sub(self.window_start);
        if elapsed < window || elapsed.is_zero() {
            return false;
        }
        self.rate = self.count as f32 / elapsed.as_secs_f32();
        self.count = 0;
        self.window_start = now;
        true
    }

    pub fn rate(&self) -> f32 {
        self.rate
    }
}

/// `Clipper_YYYY-MM-DD_HH-MM-SS.mp4` from local time components.
pub fn recording_file_name(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> String {
    format!("Clipper_{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}.mp4")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 10_000;

    #[test]
    fn pacer_starts_at_zero_and_halves_a_120hz_source_to_60fps() {
        let mut pacer = FramePacer::new(60);
        let base = 987_654_321;
        let accepted = (0..120)
            .filter_map(|i| pacer.accept(base + i * TICKS_PER_SECOND / 120))
            .collect::<Vec<_>>();
        assert_eq!(accepted.first(), Some(&0));
        assert!((59..=61).contains(&accepted.len()), "{}", accepted.len());
    }

    #[test]
    fn pacer_keeps_every_frame_when_source_matches_target() {
        let mut pacer = FramePacer::new(30);
        let accepted = (0..30)
            .filter(|i| pacer.accept(i * TICKS_PER_SECOND / 30).is_some())
            .count();
        assert_eq!(accepted, 30);
    }

    #[test]
    fn pacer_does_not_burst_after_a_stall() {
        let mut pacer = FramePacer::new(60);
        assert_eq!(pacer.accept(0), Some(0));
        // Window minimized for one second, then frames resume at 60 Hz.
        assert_eq!(pacer.accept(1000 * MS), Some(1000 * MS));
        assert_eq!(pacer.accept(1001 * MS), None);
        assert!(pacer.accept(1000 * MS + pacer.interval()).is_some());
    }

    /// Frames accepted over `seconds` from a source at `hz` with a
    /// deterministic +-`jitter` ticks wobble.
    fn accepted(hz: i64, jitter: i64, seconds: i64) -> usize {
        let mut pacer = FramePacer::new(60);
        let period = 10_000_000 / hz;
        (0..hz * seconds)
            .filter(|&i| {
                let wobble = if i % 2 == 0 { jitter } else { -jitter };
                pacer.accept(1_000_000 + i * period + wobble).is_some()
            })
            .count()
    }

    #[test]
    fn pacer_keeps_60fps_from_non_multiple_refresh_rates() {
        // 120 s: 7200 frames expected; regression for 75 Hz giving 56.25 fps.
        for hz in [75, 90, 100, 120, 144, 165] {
            for jitter in [0, 20_000] {
                let frames = accepted(hz, jitter, 120);
                assert!(
                    (7190..=7201).contains(&frames),
                    "{hz} Hz jitter {jitter}: {frames} frames"
                );
            }
        }
        // Slower sources keep every frame.
        assert_eq!(accepted(50, 0, 10), 500);
    }

    #[test]
    fn pacer_rejects_timestamps_before_origin() {
        let mut pacer = FramePacer::new(60);
        assert_eq!(pacer.accept(100 * MS), Some(0));
        assert_eq!(pacer.accept(50 * MS), None);
    }

    #[test]
    fn shared_audio_origin_preserves_real_video_start_delay() {
        let mut pacer = FramePacer::with_origin(60, 5_000_000);
        assert_eq!(pacer.accept(5_200_000), Some(200_000));
        assert_eq!(pacer.accept(4_900_000), None);
    }

    #[test]
    fn rate_meter_reports_per_window() {
        let mut meter = RateMeter::new();
        for _ in 0..60 {
            meter.record();
        }
        assert!(!meter.update(Duration::from_millis(500), Duration::from_secs(1)));
        assert!(meter.update(Duration::from_secs(1), Duration::from_secs(1)));
        assert!((meter.rate() - 60.0).abs() < 0.01);
    }

    #[test]
    fn file_name_is_sortable_and_filesystem_safe() {
        assert_eq!(
            recording_file_name(2026, 1, 2, 3, 4, 5),
            "Clipper_2026-01-02_03-04-05.mp4"
        );
    }
}
