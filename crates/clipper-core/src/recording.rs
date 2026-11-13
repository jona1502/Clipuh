//! Platform-independent contract between the UI and the recorder worker.
//!
//! The UI sends [`RecorderCommand`]s through a bounded std channel and never
//! blocks on it (`try_send`). The worker reports [`RecorderEvent`]s through a
//! bounded async channel that a GPUI task awaits, so the UI re-renders only
//! when something actually changed.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

/// AAC audio bitrate in bits per second for each encoded track (stereo, 48 kHz).
pub const AAC_TRACK_BITRATE_BPS: u32 = 192_000;
/// Compatibility alias for [`AAC_TRACK_BITRATE_BPS`].
pub const AAC_BITRATE_BPS: u32 = AAC_TRACK_BITRATE_BPS;

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
    pub audio: RecordingAudioOptions,
    /// Output video size. The source is scaled on the GPU to this size.
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub profile: QualityProfile,
    pub output_directory: PathBuf,
}

impl RecordingOptions {
    /// Video bitrate the encoder targets for these options (quality preset applied).
    pub fn video_bitrate_bps(&self) -> u32 {
        self.profile.bitrate_bps(self.bitrate_mbps)
    }

    /// Total audio bitrate written to the file.
    pub fn audio_bitrate_bps(&self) -> u32 {
        AAC_TRACK_BITRATE_BPS * self.audio.track_count() as u32
    }

    /// Estimated file size in decimal megabytes for `seconds` of recording.
    /// An estimate: VBR presets vary around their average bitrate.
    pub fn estimated_megabytes(&self, seconds: f64) -> f64 {
        estimated_megabytes(
            f64::from(self.video_bitrate_bps()) / 1e6,
            f64::from(self.audio_bitrate_bps()) / 1e6,
            seconds,
        )
    }
}

/// `(video Mbps + audio Mbps) * seconds / 8`, in decimal megabytes.
pub fn estimated_megabytes(video_mbps: f64, audio_mbps: f64, seconds: f64) -> f64 {
    (video_mbps + audio_mbps) * seconds / 8.0
}

/// H.264 Level 5.2 limits (the highest common hardware level): at most
/// 36 864 macroblocks per frame and 2 073 600 per second. 4K at 120 fps
/// exceeds them, so hardware encoders reject it.
pub fn h264_supports(width: u32, height: u32, fps: u32) -> bool {
    let macroblocks = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
    macroblocks <= 36_864 && macroblocks * u64::from(fps) <= 2_073_600
}

/// Recording uses one endpoint playback mix plus an optional microphone.
/// Process-isolated game/Discord capture is not wired into the recorder yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingAudioOptions {
    pub desktop: bool,
    pub microphone: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub microphone_gain_percent: u16,
    pub desktop_gain_percent: u16,
    pub separate_tracks: bool,
}

impl RecordingAudioOptions {
    /// Audio tracks in the file: the mix, plus one isolated track per enabled
    /// source when `separate_tracks` is on; none when no source is enabled.
    pub fn track_count(&self) -> usize {
        if !(self.desktop || self.microphone) {
            return 0;
        }
        let isolated = if self.separate_tracks {
            usize::from(self.desktop) + usize::from(self.microphone)
        } else {
            0
        };
        1 + isolated
    }

    /// Number of AAC tracks the current desktop/microphone recorder creates.
    pub fn encoded_track_count(&self) -> u32 {
        self.track_count() as u32
    }
}

impl Default for RecordingAudioOptions {
    fn default() -> Self {
        Self {
            desktop: true,
            microphone: true,
            input_device: None,
            output_device: None,
            microphone_gain_percent: 100,
            desktop_gain_percent: 100,
            separate_tracks: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecorderCommand {
    RefreshSources,
    StartAudioTest {
        options: RecordingAudioOptions,
        output_directory: PathBuf,
    },
    StopAudioTest,
    StartRecording {
        source: CaptureSource,
        options: RecordingOptions,
    },
    StopRecording,
    /// Save the last seconds of the replay buffer as a clip. Never blocks the
    /// sender; the outcome arrives as a `Replay*` event.
    SaveReplay,
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
    AudioLevels {
        desktop: f32,
        microphone: f32,
    },
    AudioTestStarted,
    AudioTestFinished {
        path: Option<PathBuf>,
        error: Option<String>,
    },
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
    /// A replay save was accepted and the clip is being written.
    ReplaySaving,
    /// The replay clip was written and is playable.
    ReplaySaved {
        path: PathBuf,
        duration: Duration,
    },
    /// The replay could not be saved, e.g. [`REPLAY_NOT_RUNNING`].
    ReplaySaveFailed {
        message: String,
    },
    /// The worker thread has released all resources and exited.
    Stopped,
}

/// Reported for `SaveReplay` while no replay buffer is capturing.
pub const REPLAY_NOT_RUNNING: &str = "Replay buffer is not running.";

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
    fn storage_estimate_uses_decimal_megabytes() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(estimated_megabytes(16.0, 0.0, 60.0), 120.0));
        assert!(close(estimated_megabytes(16.0, 0.384, 60.0), 122.88));
        assert!(close(estimated_megabytes(8.0, 0.256, 30.0), 30.96));
    }

    #[test]
    fn audio_track_count_matches_the_encoder_layout() {
        let mut audio = RecordingAudioOptions::default();
        assert_eq!(audio.track_count(), 3);
        audio.separate_tracks = false;
        assert_eq!(audio.track_count(), 1);
        audio.desktop = false;
        audio.microphone = false;
        assert_eq!(audio.track_count(), 0);
    }

    #[test]
    fn h264_level_limits_reject_4k_at_120() {
        assert!(h264_supports(3840, 2160, 60));
        assert!(!h264_supports(3840, 2160, 120));
        assert!(h264_supports(2560, 1440, 120));
        assert!(h264_supports(1600, 900, 120));
    }

    #[test]
    fn quality_profile_scales_bitrate_and_caps_it() {
        assert_eq!(QualityProfile::Performance.bitrate_bps(16), 16_000_000);
        assert_eq!(QualityProfile::Balanced.bitrate_bps(16), 16_000_000);
        assert_eq!(QualityProfile::Quality.bitrate_bps(16), 24_000_000);
        assert_eq!(QualityProfile::Quality.bitrate_bps(u32::MAX), 150_000_000);
    }

    #[test]
    fn encoded_track_count_matches_enabled_sources_and_separation() {
        for (desktop, microphone, separate_tracks, expected) in [
            (false, false, false, 0),
            (false, false, true, 0),
            (true, false, false, 1),
            (false, true, false, 1),
            (true, true, false, 1),
            (true, false, true, 2),
            (false, true, true, 2),
            (true, true, true, 3),
        ] {
            let options = RecordingAudioOptions {
                desktop,
                microphone,
                separate_tracks,
                ..Default::default()
            };

            assert_eq!(
                options.encoded_track_count(),
                expected,
                "desktop={desktop}, microphone={microphone}, separate_tracks={separate_tracks}"
            );
            assert_eq!(options.track_count(), expected as usize);
        }
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
