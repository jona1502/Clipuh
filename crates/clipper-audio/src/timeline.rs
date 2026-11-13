//! Places timestamped packets on a gap-free 48 kHz sample timeline anchored
//! at the recording start (shared QPC timebase with video).
//!
//! Drift handling: device clocks differ slightly from QPC (typ. < 100 ppm).
//! Such drift is not corrected continuously; it accumulates in the
//! difference between the packet timestamp position and the written position
//! until it crosses [`TOLERANCE_FRAMES`], then one pad/skip of that size is
//! applied. A downstream muxer may later replace this with resampling driven
//! by [`TimelineStats::drift_ppm`]; the API stays the same.

use crate::types::{SAMPLE_RATE, TICKS_PER_SECOND};

/// Offsets within this many frames (10 ms at 48 kHz) are treated as timestamp
/// jitter and left uncorrected, so jitter never causes clicks.
pub const TOLERANCE_FRAMES: i64 = 480;

/// Offsets beyond this many frames (100 ms) are discontinuities (gap or
/// seek), not drift; they reset the drift estimator.
const DISCONTINUITY_FRAMES: i64 = 4_800;

/// Minimum span of audio (1 s) before `drift_ppm` is reported.
const MIN_DRIFT_SPAN_FRAMES: f64 = SAMPLE_RATE as f64;

/// What to do with one packet so the track stays aligned to its timestamps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Alignment {
    /// Silent frames to write before the packet (gap, e.g. loopback idle).
    pub pad_frames: u64,
    /// Leading frames of the packet to discard (packet overlaps the past).
    pub skip_frames: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimelineStats {
    pub padded_frames: u64,
    pub skipped_frames: u64,
    /// Largest |timestamp position - written position| seen, in frames.
    pub max_drift_frames: u64,
    /// Estimated device clock drift vs. QPC in parts per million.
    pub drift_ppm: f64,
}

/// Running least-squares fit of uncorrected offset (frames) vs. expected
/// position (frames). Allocation-free; x is relative to the first sample.
#[derive(Clone, Copy, Debug, Default)]
struct DriftFit {
    x0: Option<i64>,
    n: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    sxy: f64,
    span: f64,
    /// Net small corrections (skips - pads) since the last reset.
    corrected: i64,
}

impl DriftFit {
    fn add(&mut self, expected: i64, offset: i64) {
        let x0 = *self.x0.get_or_insert(expected);
        let x = (expected - x0) as f64;
        let y = (offset + self.corrected) as f64;
        self.n += 1.0;
        self.sx += x;
        self.sy += y;
        self.sxx += x * x;
        self.sxy += x * y;
        self.span = x;
    }

    fn ppm(&self) -> f64 {
        if self.span < MIN_DRIFT_SPAN_FRAMES {
            return 0.0;
        }
        let den = self.n * self.sxx - self.sx * self.sx;
        if den <= 0.0 {
            return 0.0;
        }
        (self.n * self.sxy - self.sx * self.sy) / den * 1e6
    }
}

pub struct TrackTimeline {
    origin: i64,
    position: u64,
    padded: u64,
    skipped: u64,
    max_drift: u64,
    fit: DriftFit,
    ppm: f64,
}

impl TrackTimeline {
    /// `origin` is the QPC time (100 ns) of timeline frame 0.
    pub fn new(origin: i64) -> Self {
        Self {
            origin,
            position: 0,
            padded: 0,
            skipped: 0,
            max_drift: 0,
            fit: DriftFit::default(),
            ppm: 0.0,
        }
    }

    /// Aligns a packet of `frames` frames stamped `timestamp`.
    ///
    /// Expected position is `ticks_to_frames(timestamp - origin)`; the packet
    /// is then padded, skipped or left alone per [`TOLERANCE_FRAMES`], and
    /// the written position advances by `pad + frames - skip`.
    pub fn align(&mut self, timestamp: i64, frames: u64) -> Alignment {
        let delta = i128::from(timestamp) - i128::from(self.origin);
        let expected = ticks_to_frames_wide(delta);
        let written = i64::try_from(self.position).unwrap_or(i64::MAX);
        let diff = expected.saturating_sub(written); // >0: gap, <0: overlap
        self.max_drift = self.max_drift.max(diff.unsigned_abs());

        let mut a = Alignment::default();
        if diff > TOLERANCE_FRAMES {
            a.pad_frames = diff as u64;
        } else if diff < -TOLERANCE_FRAMES {
            a.skip_frames = diff.unsigned_abs().min(frames);
        }

        if diff.abs() > DISCONTINUITY_FRAMES {
            self.fit = DriftFit::default();
        } else if expected >= 0 {
            // Offset as written minus expected, before this correction.
            self.fit.add(expected, -diff);
            self.fit.corrected += a.skip_frames as i64 - a.pad_frames as i64;
            self.ppm = self.fit.ppm();
        }

        self.padded += a.pad_frames;
        self.skipped += a.skip_frames;
        self.position += a.pad_frames + frames - a.skip_frames;
        a
    }

    /// Frames written so far (after padding/skipping).
    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn stats(&self) -> TimelineStats {
        TimelineStats {
            padded_frames: self.padded,
            skipped_frames: self.skipped,
            max_drift_frames: self.max_drift,
            drift_ppm: self.ppm,
        }
    }
}

/// Rounds `n / d` to nearest, halves away from zero (`d > 0`).
fn round_div(n: i128, d: i128) -> i128 {
    let half = d / 2;
    if n >= 0 {
        (n + half) / d
    } else {
        (n - half) / d
    }
}

fn saturate(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn ticks_to_frames_wide(ticks: i128) -> i64 {
    saturate(round_div(
        ticks * i128::from(SAMPLE_RATE),
        i128::from(TICKS_PER_SECOND),
    ))
}

/// Converts a QPC duration (100 ns) to 48 kHz frames, rounding to nearest.
pub fn ticks_to_frames(ticks: i64) -> i64 {
    ticks_to_frames_wide(i128::from(ticks))
}

/// Converts 48 kHz frames to a QPC duration (100 ns), rounding to nearest.
pub fn frames_to_ticks(frames: i64) -> i64 {
    saturate(round_div(
        i128::from(frames) * i128::from(TICKS_PER_SECOND),
        i128::from(SAMPLE_RATE),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: i64 = 123_456_789_000;
    const PKT: u64 = 480; // 10 ms

    fn ts(frames: i64) -> i64 {
        ORIGIN + frames_to_ticks(frames)
    }

    #[test]
    fn conversions() {
        assert_eq!(ticks_to_frames(TICKS_PER_SECOND), 48_000);
        assert_eq!(frames_to_ticks(48_000), TICKS_PER_SECOND);
        assert_eq!(ticks_to_frames(0), 0);
        assert_eq!(ticks_to_frames(-TICKS_PER_SECOND), -48_000);
        for f in [-1_000_000i64, -1, 0, 1, 479, 480, 48_000, 172_800_000 * 4] {
            assert_eq!(ticks_to_frames(frames_to_ticks(f)), f);
        }
        // One tick is 0.0048 frames: 105 ticks = 0.504 -> 1, 104 -> 0.
        assert_eq!(ticks_to_frames(105), 1);
        assert_eq!(ticks_to_frames(104), 0);
        assert_eq!(ticks_to_frames(-105), -1);
        // Hours of audio, no overflow.
        let day = 24 * 3600 * TICKS_PER_SECOND;
        assert_eq!(ticks_to_frames(day), 24 * 3600 * 48_000);
        assert!(ticks_to_frames(i64::MAX) > 0);
    }

    #[test]
    fn contiguous_needs_no_correction() {
        let mut t = TrackTimeline::new(ORIGIN);
        for k in 0..1000 {
            let a = t.align(ts(k * PKT as i64), PKT);
            assert_eq!(a, Alignment::default());
        }
        assert_eq!(t.position(), 1000 * PKT);
        let s = t.stats();
        assert_eq!(
            (s.padded_frames, s.skipped_frames, s.max_drift_frames),
            (0, 0, 0)
        );
    }

    #[test]
    fn first_packet_after_origin_pads_from_zero() {
        let mut t = TrackTimeline::new(ORIGIN);
        let a = t.align(ts(24_000), PKT);
        assert_eq!(a.pad_frames, 24_000);
        assert_eq!(t.position(), 24_000 + PKT);
    }

    #[test]
    fn gap_200ms_padded_exactly() {
        let mut t = TrackTimeline::new(ORIGIN);
        let mut pos = 0i64;
        for _ in 0..10 {
            t.align(ts(pos), PKT);
            pos += PKT as i64;
        }
        pos += 9_600; // 200 ms of silence, no packets
        let a = t.align(ts(pos), PKT);
        assert_eq!(
            a,
            Alignment {
                pad_frames: 9_600,
                skip_frames: 0
            }
        );
        assert_eq!(t.position(), (pos as u64) + PKT);
        assert_eq!(t.stats().padded_frames, 9_600);
    }

    #[test]
    fn jitter_within_tolerance_never_corrects() {
        let mut t = TrackTimeline::new(ORIGIN);
        let jit = [-2i64, 2, 1, -1, 0, 2, -2, 1];
        for k in 0..2000i64 {
            let j = jit[(k % 8) as usize] * 10_000; // +-2 ms in ticks
            let a = t.align(ts(k * PKT as i64) + j, PKT);
            assert_eq!(a, Alignment::default(), "packet {k}");
        }
        assert_eq!(t.position(), 2000 * PKT);
        assert!(t.stats().max_drift_frames <= 97);
    }

    #[test]
    fn overlap_50ms_skips_2400() {
        let mut t = TrackTimeline::new(ORIGIN);
        t.align(ts(0), 48_000);
        // Next packet stamped 50 ms before the written end.
        let a = t.align(ts(48_000 - 2_400), 4_800);
        assert_eq!(
            a,
            Alignment {
                pad_frames: 0,
                skip_frames: 2_400
            }
        );
        assert_eq!(t.position(), 48_000 + 4_800 - 2_400);
        assert_eq!(t.stats().skipped_frames, 2_400);
    }

    #[test]
    fn skip_is_capped_to_packet_length() {
        let mut t = TrackTimeline::new(ORIGIN);
        t.align(ts(0), 48_000);
        let a = t.align(ts(0), 480);
        assert_eq!(a.skip_frames, 480);
        assert_eq!(t.position(), 48_000);
    }

    #[test]
    fn pre_origin_packet_skipped() {
        let mut t = TrackTimeline::new(ORIGIN);
        let a = t.align(ts(-48_000), 480);
        assert_eq!(
            a,
            Alignment {
                pad_frames: 0,
                skip_frames: 480
            }
        );
        assert_eq!(t.position(), 0);
        // Subsequent in-time packet is unaffected.
        let a = t.align(ts(0), 480);
        assert_eq!(a, Alignment::default());
        assert_eq!(t.position(), 480);
    }

    /// Device clock runs `ppm` fast: it delivers more frames per QPC second.
    fn simulate(ppm: f64, seconds: i64) -> TrackTimeline {
        let mut t = TrackTimeline::new(ORIGIN);
        let n = seconds * 100; // 10 ms packets
        for k in 0..n {
            let qpc_ticks = (k as f64) * 100_000.0 / (1.0 + ppm * 1e-6);
            t.align(ORIGIN + qpc_ticks.round() as i64, PKT);
        }
        t
    }

    #[test]
    fn drift_80ppm_60s() {
        let t = simulate(80.0, 60);
        let s = t.stats();
        let drift = (60.0 * 48_000.0 * 80e-6) as i64; // ~230 frames
        assert_eq!(drift, 230);
        let net = s.skipped_frames as i64 - s.padded_frames as i64;
        // Drift (230) is below tolerance, so at most one correction occurs.
        assert!((net - drift).abs() <= TOLERANCE_FRAMES, "net {net}");
        assert!((s.drift_ppm - 80.0).abs() <= 10.0, "ppm {}", s.drift_ppm);
        assert!(s.max_drift_frames <= TOLERANCE_FRAMES as u64);
    }

    #[test]
    fn drift_80ppm_long_run_is_corrected() {
        let secs = 300;
        let t = simulate(80.0, secs);
        let s = t.stats();
        let drift = (secs as f64 * 48_000.0 * 80e-6) as i64; // 1152
        let net = s.skipped_frames as i64 - s.padded_frames as i64;
        assert!(s.skipped_frames > 0 && s.padded_frames == 0);
        assert!((net - drift).abs() <= TOLERANCE_FRAMES, "net {net}");
        assert!((s.drift_ppm - 80.0).abs() <= 10.0, "ppm {}", s.drift_ppm);
    }

    #[test]
    fn slow_clock_negative_drift() {
        let t = simulate(-80.0, 300);
        let s = t.stats();
        assert!(s.padded_frames > 0 && s.skipped_frames == 0);
        assert!((s.drift_ppm + 80.0).abs() <= 10.0, "ppm {}", s.drift_ppm);
    }

    #[test]
    fn drift_ppm_zero_before_one_second() {
        let mut t = TrackTimeline::new(ORIGIN);
        for k in 0..50 {
            t.align(ts(k * PKT as i64), PKT);
        }
        assert_eq!(t.stats().drift_ppm, 0.0);
    }
}
