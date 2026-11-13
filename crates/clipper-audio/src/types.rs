//! Platform-independent audio API types.

/// Internal sample rate of every track. Backends request it from the OS
/// (WASAPI converts in shared mode), so no resampling happens in Rust.
pub const SAMPLE_RATE: u32 = 48_000;

/// 100 ns ticks per second: the QPC-based timebase shared with video capture
/// (WGC `SystemRelativeTime`) and WASAPI `GetBuffer` QPC positions.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// A process to capture, identified so it can be found again after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProcessTarget {
    /// Exact process id. Lost for good when the process exits.
    Pid(u32),
    /// Executable file name, case-insensitive (e.g. `"Discord.exe"`). The
    /// root of the process tree is used and re-resolved after restarts.
    Executable(String),
}

impl ProcessTarget {
    pub fn discord() -> Self {
        Self::Executable("Discord.exe".to_owned())
    }

    /// Whether both targets certainly name the same process. Executable names
    /// compare ASCII case-insensitively, pids exactly. A `Pid` and an
    /// `Executable` are never equal here: the platform-independent planner
    /// cannot resolve an exe name to a pid, so the same process selected both
    /// ways is not detected.
    pub fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pid(a), Self::Pid(b)) => a == b,
            (Self::Executable(a), Self::Executable(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum MicrophoneChoice {
    /// Follows the Windows default communications/console capture device.
    #[default]
    Default,
    /// A specific endpoint id from [`AudioDevice::id`].
    Device(String),
}

/// What the user wants to record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioSelection {
    pub game: Option<ProcessTarget>,
    pub discord: Option<ProcessTarget>,
    /// Other desktop playback. Routed so it never contains a separately
    /// captured game or Discord signal (see [`crate::routing`]).
    pub desktop: bool,
    pub microphone: Option<MicrophoneChoice>,
}

/// What the running Windows version supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioCapabilities {
    /// WASAPI process loopback (`ActivateAudioInterfaceAsync` with
    /// `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`), Windows 10 build
    /// 20348+ / Windows 11.
    pub process_loopback: bool,
}

/// Exact content of a track. Labels are honest: a full desktop mix is never
/// presented as a cleaned remainder.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrackContent {
    Game,
    Discord,
    /// Everything the default render device plays (includes game/Discord;
    /// only planned when those are not captured separately).
    DesktopFull,
    /// Desktop playback without the process tree of `excluded`.
    DesktopExcluding(AudioSourceKind),
    Microphone,
}

impl TrackContent {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Game => "Game",
            Self::Discord => "Discord",
            Self::DesktopFull => "Desktop (full mix)",
            Self::DesktopExcluding(AudioSourceKind::Game) => "Desktop (without game)",
            Self::DesktopExcluding(AudioSourceKind::Discord) => "Desktop (without Discord)",
            Self::DesktopExcluding(_) => "Desktop",
            Self::Microphone => "Microphone",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioSourceKind {
    Game,
    Discord,
    Desktop,
    Microphone,
}

/// A running output track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
    pub index: usize,
    pub content: TrackContent,
    /// 2 for playback sources; device-dependent (1 or 2) for microphones.
    pub channels: u16,
    pub sample_rate: u32,
}

/// Per-packet flags reported by the backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameFlags {
    /// The OS marked the packet as silence; samples are zero.
    pub silent: bool,
    /// The OS reported a glitch/gap before this packet.
    pub discontinuity: bool,
}

/// One captured packet, borrowed from the engine (no allocation).
#[derive(Clone, Copy, Debug)]
pub struct AudioFrameRef<'a> {
    pub track: usize,
    /// QPC time of the first sample, 100 ns ticks.
    pub timestamp: i64,
    pub channels: u16,
    /// Interleaved f32 samples, `frames * channels` long.
    pub samples: &'a [f32],
    pub flags: FrameFlags,
}

impl AudioFrameRef<'_> {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLevels {
    /// Linear peak of the last packet, 0.0..=1.0 (can exceed 1.0 for float).
    pub peak: f32,
    /// Linear RMS of the last packet.
    pub rms: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceFlow {
    Render,
    Capture,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable endpoint id string.
    pub id: String,
    pub name: String,
    pub flow: DeviceFlow,
    pub is_default: bool,
    /// Native channel count of the shared-mode mix format.
    pub channels: u16,
    /// Native sample rate of the shared-mode mix format.
    pub sample_rate: u32,
}

/// Asynchronous notifications from capture threads (bounded channel; if the
/// consumer does not drain it, further events are dropped, never blocking).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioEvent {
    /// The track's stream is running (also after a restore).
    TrackStarted { track: usize },
    /// The source vanished (process exited, device removed). The thread keeps
    /// trying to reopen it.
    SourceLost { track: usize, reason: String },
    /// The default device changed and the track switched to it.
    DeviceChanged { track: usize, device: String },
    /// The ring was full; `frames` were discarded (consumer too slow).
    Overflow { track: usize, frames: u64 },
    /// Unrecoverable for this track; the thread exited.
    Failed { track: usize, message: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum AudioError {
    /// Requested feature needs a newer Windows version.
    Unsupported(String),
    /// Device or process not found / not capturable right now.
    SourceUnavailable(String),
    /// The routing plan contains no track.
    NothingToCapture,
    /// OS API failure with context.
    Os(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "not supported on this system: {what}"),
            Self::SourceUnavailable(what) => write!(f, "audio source unavailable: {what}"),
            Self::NothingToCapture => f.write_str("no audio source selected"),
            Self::Os(message) => write!(f, "audio error: {message}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Engine tuning. Defaults keep RAM small: 500 ms of stereo f32 per track is
/// 192 KB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioConfig {
    /// Ring capacity per track in milliseconds.
    pub ring_ms: u32,
    /// Capacity of the [`AudioEvent`] channel.
    pub event_capacity: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            ring_ms: 500,
            event_capacity: 64,
        }
    }
}
