//! Places timestamped packets on a gap-free 48 kHz sample timeline anchored
//! at the recording start (shared QPC timebase with video). CONTRACT STUB.

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

pub struct TrackTimeline {}

impl TrackTimeline {
    /// `origin` is the QPC time (100 ns) of timeline frame 0.
    pub fn new(_origin: i64) -> Self {
        todo!("sync agent")
    }

    /// Aligns a packet of `frames` frames stamped `timestamp`.
    pub fn align(&mut self, _timestamp: i64, _frames: u64) -> Alignment {
        todo!("sync agent")
    }

    /// Frames written so far (after padding/skipping).
    pub fn position(&self) -> u64 {
        todo!("sync agent")
    }

    pub fn stats(&self) -> TimelineStats {
        todo!("sync agent")
    }
}

/// Converts a QPC duration (100 ns) to 48 kHz frames, rounding to nearest.
pub fn ticks_to_frames(_ticks: i64) -> i64 {
    todo!("sync agent")
}

/// Converts 48 kHz frames to a QPC duration (100 ns), rounding to nearest.
pub fn frames_to_ticks(_frames: i64) -> i64 {
    todo!("sync agent")
}
