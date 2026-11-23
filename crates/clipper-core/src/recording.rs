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
#[derive(Clone, Debug, PartialEq)]
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

/// What one mixer source captures. Platform-neutral; the recorder resolves it
/// to a concrete capture stream when a session starts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AudioSourceTarget {
    /// The application behind the capture window; follows the capture source.
    /// Unavailable while a whole display is captured.
    Game,
    /// A running application by executable file name, e.g. `Discord.exe`.
    /// Survives restarts and pid changes.
    Application(String),
    /// Loopback of an output device (endpoint id); `None` is the Windows default.
    Desktop(Option<String>),
    /// An input device (endpoint id); `None` is the Windows default.
    Microphone(Option<String>),
}

/// Highest per-source gain, in percent. Above 100 % the recorder limits the
/// output instead of letting it clip.
pub const MAX_AUDIO_GAIN_PERCENT: u16 = 200;

/// Most sources recorded at once: the MP4 writer takes one mix plus four
/// isolated AAC tracks.
pub const MAX_RECORDED_AUDIO_SOURCES: usize = 4;

/// Per-source gate parameters. Threshold is measured before source gain.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NoiseGateSettings {
    pub enabled: bool,
    pub threshold_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for NoiseGateSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -45.0,
            attack_ms: 5.0,
            hold_ms: 100.0,
            release_ms: 150.0,
        }
    }
}

impl NoiseGateSettings {
    /// Repair malformed persisted or external parameters before entering DSP.
    pub fn sanitized(self) -> Self {
        let bound = |value: f32, low: f32, high: f32, default: f32| {
            if value.is_finite() {
                value.clamp(low, high)
            } else {
                default
            }
        };
        Self {
            enabled: self.enabled,
            threshold_db: bound(self.threshold_db, -80.0, 0.0, -45.0),
            attack_ms: bound(self.attack_ms, 1.0, 100.0, 5.0),
            hold_ms: bound(self.hold_ms, 0.0, 1000.0, 100.0),
            release_ms: bound(self.release_ms, 5.0, 2000.0, 150.0),
        }
    }
}

/// One enabled mixer source handed to the recorder.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingAudioSource {
    /// Stable id from the settings; levels, status and gain changes use it.
    pub id: u32,
    pub target: AudioSourceTarget,
    /// 0..=[`MAX_AUDIO_GAIN_PERCENT`]; 0 is silence.
    pub gain_percent: u16,
    pub noise_gate: NoiseGateSettings,
}

/// Audio of a recording or replay: the enabled mixer sources in mixer order.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingAudioOptions {
    pub sources: Vec<RecordingAudioSource>,
    /// Also write one isolated track per source next to the mix.
    pub separate_tracks: bool,
}

impl RecordingAudioOptions {
    /// Upper bound of audio tracks in the file: the mix, plus one isolated
    /// track per source when `separate_tracks` is on; none without sources.
    /// The recorder may drop a source it cannot capture (unsupported Windows
    /// version, overlapping desktop audio), so the real count can be lower.
    pub fn track_count(&self) -> usize {
        if self.sources.is_empty() {
            return 0;
        }
        1 + if self.separate_tracks {
            self.sources.len()
        } else {
            0
        }
    }

    /// AAC tracks used for storage estimates (see [`Self::track_count`]).
    pub fn encoded_track_count(&self) -> u32 {
        self.track_count() as u32
    }
}

impl Default for RecordingAudioOptions {
    /// Desktop playback and the default microphone, as before the mixer.
    fn default() -> Self {
        Self {
            sources: vec![
                RecordingAudioSource {
                    id: 1,
                    target: AudioSourceTarget::Desktop(None),
                    gain_percent: 100,
                    noise_gate: NoiseGateSettings::default(),
                },
                RecordingAudioSource {
                    id: 2,
                    target: AudioSourceTarget::Microphone(None),
                    gain_percent: 100,
                    noise_gate: NoiseGateSettings::default(),
                },
            ],
            separate_tracks: true,
        }
    }
}

/// Live peak of one mixer source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceLevel {
    pub source: u32,
    /// Linear peak after gain, 0.0..=1.0.
    pub peak: f32,
}

/// What the recorder is doing with one mixer source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioSourceStatus {
    Starting,
    /// Captured into the session; `track` is its isolated track, if any.
    /// `note` explains a limitation of what is captured (e.g. applications a
    /// desktop track cannot contain without duplicating a separated app).
    Capturing {
        track: Option<usize>,
        note: Option<String>,
    },
    /// Kept in the session but silent right now (application not running,
    /// device gone); capture resumes on its own when it comes back.
    Waiting(String),
    /// Not part of this session, with the reason (unsupported on this
    /// Windows version, would duplicate other audio, no game window).
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSourceState {
    pub source: u32,
    pub status: AudioSourceStatus,
}

/// Allowed replay lengths offered in settings, in seconds.
pub const REPLAY_DURATIONS_SECONDS: [u32; 5] = [15, 30, 60, 120, 300];

/// Everything the recorder needs to run the instant replay buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayOptions {
    /// Video/audio encoding settings; `output_directory` receives saved clips.
    pub recording: RecordingOptions,
    pub duration: Duration,
    /// RAM budget for encoded packets in bytes (not the process size).
    pub memory_limit_bytes: u64,
    /// App-owned cache root for temporary SSD segments. `None` = RAM only.
    pub cache_directory: Option<PathBuf>,
    /// Upper bound for temporary SSD segment bytes.
    pub disk_limit_bytes: u64,
}

/// Where buffered replay footage lives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplayTier {
    #[default]
    Ram,
    /// Older footage spilled to temporary SSD segments.
    Hybrid,
    /// SSD unavailable: RAM only, the replay may be shorter than configured.
    RamOnlyDegraded,
}

/// Replay buffer status, sent about once per second while it runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayStatus {
    /// Footage currently available for a save.
    pub buffered: Duration,
    pub configured: Duration,
    pub ram_bytes: u64,
    pub disk_bytes: u64,
    pub tier: ReplayTier,
    pub exports_in_flight: usize,
    pub stats: RecordingStats,
    /// Human readable warning (e.g. SSD tier failed), if any.
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
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
    /// Start the instant replay buffer. Mutually exclusive with a manual
    /// recording (one capture/encoder session at a time).
    StartReplay {
        source: CaptureSource,
        options: ReplayOptions,
    },
    /// Stop the replay buffer. Running exports still finish.
    StopReplay,
    /// Change the replay length live, without restarting capture.
    SetReplayDuration(Duration),
    /// Change one source's recorded gain live (ramped, no restart). Ignored
    /// when no session records that source.
    SetAudioGain {
        source: u32,
        gain_percent: u16,
    },
    /// The enabled audio sources changed (added, removed, enabled, disabled,
    /// retargeted). A running instant replay applies the change live,
    /// without restarting capture, encoder or buffer; a manual recording or
    /// audio test keeps its sources until it ends.
    SetAudioSources(RecordingAudioOptions),
    /// Update only this source's gate in the active recording, replay or test.
    SetAudioNoiseGate {
        source: u32,
        settings: NoiseGateSettings,
    },
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
    /// Peaks of the captured sources, about 15 times per second while a
    /// session or audio test runs; an empty list when capture stops.
    AudioLevels(Vec<SourceLevel>),
    /// Per-source capture state at session start and whenever it changes;
    /// an empty list when the session ends.
    AudioSources(Vec<AudioSourceState>),
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
    /// The replay buffer is capturing; `encoder` is the hardware encoder name.
    ReplayStarted {
        encoder: String,
    },
    ReplayStatus(ReplayStatus),
    /// The replay buffer stopped; `error` is set when it stopped on a failure
    /// (capture lost, encoder failure, audio device removed...).
    ReplayStopped {
        error: Option<String>,
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
    fn noise_gate_settings_bound_nonfinite_and_overflow() {
        let settings = NoiseGateSettings {
            enabled: true,
            threshold_db: f32::NAN,
            attack_ms: f32::INFINITY,
            hold_ms: -20.0,
            release_ms: f32::MAX,
        }
        .sanitized();
        assert_eq!(
            settings,
            NoiseGateSettings {
                enabled: true,
                threshold_db: -45.0,
                attack_ms: 5.0,
                hold_ms: 0.0,
                release_ms: 2000.0,
            }
        );
        assert_eq!(settings.sanitized(), settings);
    }

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
        audio.sources.clear();
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
    fn encoded_track_count_is_one_mix_plus_one_per_source() {
        let source = |id| RecordingAudioSource {
            id,
            target: AudioSourceTarget::Application(format!("app{id}.exe")),
            gain_percent: 100,
            noise_gate: NoiseGateSettings::default(),
        };
        for (sources, separate_tracks, expected) in [
            (0, false, 0),
            (0, true, 0),
            (1, false, 1),
            (3, false, 1),
            (1, true, 2),
            (4, true, 5),
        ] {
            let options = RecordingAudioOptions {
                sources: (1..=sources).map(source).collect(),
                separate_tracks,
            };
            assert_eq!(
                options.encoded_track_count(),
                expected,
                "{sources} {separate_tracks}"
            );
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
