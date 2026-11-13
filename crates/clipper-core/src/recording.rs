//! Platform-independent contract between the UI and the recorder worker.
//!
//! The UI sends [`RecorderCommand`]s through a bounded std channel and never
//! blocks on it (`try_send`). The worker reports [`RecorderEvent`]s through a
//! bounded async channel that a GPUI task awaits, so the UI re-renders only
//! when something actually changed.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

/// What kind of surface a [`CaptureSource`] refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Monitor,
    Window,
}

/// A capturable monitor or window.
///
/// `id` is an opaque platform handle (HMONITOR / HWND on Windows) and is only
/// valid until the source disappears; backends must re-validate it on start.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CaptureSource {
    pub kind: SourceKind,
    pub id: u64,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

/// Encoder tuning preset. Mapping to concrete encoder options lives in the
/// encoder backend; only options the active encoder accepts are applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum QualityProfile {
    /// Speed-oriented encoder preset, configured bitrate (CBR).
    Performance,
    /// Balanced preset, configured bitrate (peak-constrained VBR).
    #[default]
    Balanced,
    /// Quality preset, 1.5x configured bitrate (capped), VBR.
    Quality,
}

impl QualityProfile {
    pub const ALL: [Self; 3] = [Self::Performance, Self::Balanced, Self::Quality];

    pub fn label(self) -> &'static str {
        match self {
            Self::Performance => "Performance",
            Self::Balanced => "Balanced",
            Self::Quality => "Quality",
        }
    }

    /// Effective target bitrate in bits per second for a configured base bitrate.
    pub fn bitrate_bps(self, base_mbps: u32) -> u32 {
        const MAX_BPS: u32 = 150_000_000;
        let base = base_mbps.saturating_mul(1_000_000);
        match self {
            Self::Performance | Self::Balanced => base.min(MAX_BPS),
            Self::Quality => (base / 2).saturating_mul(3).min(MAX_BPS),
        }
    }
}

/// Everything the recorder needs to start one recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingOptions {
    /// Output video size. The source is scaled on the GPU to this size.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub profile: QualityProfile,
    pub output_directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecorderCommand {
    RefreshSources,
    StartRecording {
        source: CaptureSource,
        options: RecordingOptions,
    },
    StopRecording,
    Shutdown,
}

/// Snapshot of pipeline counters, sent about once per second while recording.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RecordingStats {
    pub elapsed: Duration,
    /// Frames delivered by the capture API.
    pub frames_captured: u64,
    /// Frames submitted to the encoder.
    pub frames_encoded: u64,
    /// Frames lost because the encoder could not keep up (backpressure).
    pub frames_dropped: u64,
    /// Encoded frames per second over the last measurement window.
    pub current_fps: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    User,
    SourceClosed,
    Shutdown,
    /// Capture or encoder failed mid-recording; frames up to the failure
    /// were finalized successfully.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecorderEvent {
    SourcesListed(Vec<CaptureSource>),
    /// Encoder and capture are running; `encoder` is the hardware encoder name.
    RecordingStarted {
        path: PathBuf,
        encoder: String,
    },
    Stats(RecordingStats),
    /// The file was finalized successfully and is playable.
    RecordingFinished {
        path: PathBuf,
        stats: RecordingStats,
        reason: StopReason,
    },
    /// Recording failed. No successful file is reported; a partial file is
    /// deleted when possible.
    RecordingFailed {
        message: String,
    },
    /// Non-fatal error (e.g. source enumeration failed, invalid command).
    Error {
        message: String,
    },
    /// The worker thread has released all resources and exited.
    Stopped,
}

/// Why a command could not be queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendCommandError {
    /// The bounded command queue is full; the worker is busy.
    Busy,
    /// The worker has exited.
    Disconnected,
}

impl std::fmt::Display for SendCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("recorder is busy, try again"),
            Self::Disconnected => f.write_str("recorder is not running"),
        }
    }
}

impl std::error::Error for SendCommandError {}

/// UI-side endpoint. Cheap to clone the parts; never blocks.
#[derive(Clone, Debug)]
pub struct RecorderHandle {
    commands: SyncSender<RecorderCommand>,
    events: async_channel::Receiver<RecorderEvent>,
}

impl RecorderHandle {
    pub fn send(&self, command: RecorderCommand) -> Result<(), SendCommandError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => SendCommandError::Busy,
                TrySendError::Disconnected(_) => SendCommandError::Disconnected,
            })
    }

    pub fn events(&self) -> async_channel::Receiver<RecorderEvent> {
        self.events.clone()
    }
}

/// Worker-side endpoint.
#[derive(Debug)]
pub struct RecorderEndpoint {
    pub commands: Receiver<RecorderCommand>,
    pub events: async_channel::Sender<RecorderEvent>,
}

/// Create the bounded UI <-> worker channel pair. Capacities must be >= 1.
pub fn recorder_channels(
    command_capacity: usize,
    event_capacity: usize,
) -> (RecorderHandle, RecorderEndpoint) {
    let (command_tx, command_rx) = sync_channel(command_capacity.max(1));
    let (event_tx, event_rx) = async_channel::bounded(event_capacity.max(1));
    (
        RecorderHandle {
            commands: command_tx,
            events: event_rx,
        },
        RecorderEndpoint {
            commands: command_rx,
            events: event_tx,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_profile_scales_bitrate_and_caps_it() {
        assert_eq!(QualityProfile::Performance.bitrate_bps(16), 16_000_000);
        assert_eq!(QualityProfile::Balanced.bitrate_bps(16), 16_000_000);
        assert_eq!(QualityProfile::Quality.bitrate_bps(16), 24_000_000);
        assert_eq!(QualityProfile::Quality.bitrate_bps(u32::MAX), 150_000_000);
    }

    #[test]
    fn full_command_queue_reports_busy_without_blocking() {
        let (handle, endpoint) = recorder_channels(1, 1);
        handle.send(RecorderCommand::RefreshSources).unwrap();
        assert_eq!(
            handle.send(RecorderCommand::StopRecording),
            Err(SendCommandError::Busy)
        );
        drop(endpoint);
        assert_eq!(
            handle.send(RecorderCommand::StopRecording),
            Err(SendCommandError::Disconnected)
        );
    }

    #[test]
    fn events_reach_the_ui_receiver() {
        let (handle, endpoint) = recorder_channels(1, 2);
        endpoint
            .events
            .try_send(RecorderEvent::SourcesListed(Vec::new()))
            .unwrap();
        assert_eq!(
            handle.events().try_recv(),
            Ok(RecorderEvent::SourcesListed(Vec::new()))
        );
    }
}
