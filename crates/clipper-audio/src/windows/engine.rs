//! Public capture engine. CONTRACT STUB — owned by the engine agent.

use std::sync::mpsc::Receiver;

use crate::routing::RoutingWarning;
use crate::types::{
    AudioConfig, AudioError, AudioEvent, AudioFrameRef, AudioLevels, AudioSelection, TrackInfo,
};

/// Final counters per track after `stop`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSummary {
    pub frames: Vec<u64>,
    pub dropped_frames: Vec<u64>,
}

/// Running audio capture: one dedicated thread per track.
pub struct AudioCapture {}

impl AudioCapture {
    /// Plans routing for `selection`, opens rings and starts every track.
    pub fn start(_selection: &AudioSelection, _config: AudioConfig) -> Result<Self, AudioError> {
        todo!("engine agent")
    }

    pub fn tracks(&self) -> &[TrackInfo] {
        todo!("engine agent")
    }

    pub fn warnings(&self) -> &[RoutingWarning] {
        todo!("engine agent")
    }

    /// Drains all available packets of all tracks, non-blocking. Returns the
    /// number of packets delivered.
    pub fn read_frames(&mut self, _f: impl FnMut(AudioFrameRef<'_>)) -> usize {
        todo!("engine agent")
    }

    /// Latest peak/RMS of `track` (lock-free).
    pub fn levels(&self, _track: usize) -> AudioLevels {
        todo!("engine agent")
    }

    pub fn events(&self) -> &Receiver<AudioEvent> {
        todo!("engine agent")
    }

    /// Stops all threads and returns counters. Unread packets are discarded.
    pub fn stop(self) -> CaptureSummary {
        todo!("engine agent")
    }

    /// QPC time (100 ns) at which `start` returned: timeline origin.
    pub fn started_at(&self) -> i64 {
        todo!("engine agent")
    }
}
