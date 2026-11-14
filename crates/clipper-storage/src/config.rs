use clipper_core::hotkey::{DEFAULT_SAVE_REPLAY, Hotkey};
use clipper_core::recording::{
    QualityProfile, REPLAY_DURATIONS_SECONDS, RecordingAudioOptions, RecordingOptions,
    ReplayOptions, h264_supports,
};
use directories::{BaseDirs, ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Output resolutions offered by the recorder: 720p, 900p, 1080p, 1440p, 4K.
pub const VIDEO_RESOLUTIONS: [(u32, u32); 5] = [
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
];
/// Frame rates offered by the recorder.
pub const VIDEO_FPS: [u32; 3] = [30, 60, 120];

/// True when the size and frame rate are both offered and H.264 can encode the combination.
pub fn video_mode_supported(width: u32, height: u32, fps: u32) -> bool {
    VIDEO_RESOLUTIONS.contains(&(width, height))
        && VIDEO_FPS.contains(&fps)
        && h264_supports(width, height, fps)
}
const MAX_BITRATE_MBPS: u32 = 100;
const MIN_REPLAY_MEMORY_MEGABYTES: u32 = 16;
const MAX_REPLAY_MEMORY_MEGABYTES: u32 = 512;
/// Allowed replay disk cache limit, in MiB.
pub const REPLAY_DISK_LIMIT_MEGABYTES: std::ops::RangeInclusive<u32> = 256..=65536;
/// Allowed notification display time, in seconds.
pub const NOTIFICATION_DURATION_SECONDS: std::ops::RangeInclusive<u32> = 2..=10;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Per-user settings for the Clipper application.
///
/// TOML input may omit fields; missing values are supplied from the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub video: VideoSettings,
    pub replay: ReplaySettings,
    pub audio: AudioSettings,
    pub performance: PerformanceSettings,
    pub recording: RecordingSettings,
    pub notifications: NotificationSettings,
    pub app: AppSettings,
    pub appearance: AppearanceSettings,
}

impl AppConfig {
    /// Returns a default config when no file exists, otherwise loads and validates it.
    ///
    /// Parse, I/O, and validation failures are returned so startup can report them.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&config_path()?)
    }

    /// Validates all bounded settings before they are used or saved.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let mut issues = Vec::new();

        if !VIDEO_RESOLUTIONS.contains(&(self.video.width, self.video.height)) {
            issues.push(ValidationIssue::new(
                "video.width",
                "resolution must be 1280x720, 1600x900, 1920x1080, 2560x1440 or 3840x2160",
            ));
        }
        if !VIDEO_FPS.contains(&self.video.fps) {
            issues.push(ValidationIssue::new(
                "video.fps",
                "must be 30, 60 or 120 frames per second",
            ));
        }
        if VIDEO_RESOLUTIONS.contains(&(self.video.width, self.video.height))
            && VIDEO_FPS.contains(&self.video.fps)
            && !h264_supports(self.video.width, self.video.height, self.video.fps)
        {
            issues.push(ValidationIssue::new(
                "video.fps",
                "4K recording is limited to 60 frames per second",
            ));
        }
        if self.video.bitrate_mbps == 0 || self.video.bitrate_mbps > MAX_BITRATE_MBPS {
            issues.push(ValidationIssue::new(
                "video.bitrate_mbps",
                "must be between 1 and 100 Mbps",
            ));
        }

        if !REPLAY_DURATIONS_SECONDS.contains(&self.replay.duration_seconds) {
            issues.push(ValidationIssue::new(
                "replay.duration_seconds",
                "must be 15, 30, 60, 120 or 300 seconds",
            ));
        }
        if !REPLAY_DISK_LIMIT_MEGABYTES.contains(&self.replay.disk_limit_megabytes) {
            issues.push(ValidationIssue::new(
                "replay.disk_limit_megabytes",
                "must be between 256 and 65536 MiB",
            ));
        }
        if let Some(directory) = &self.replay.cache_directory
            && !directory.is_absolute()
        {
            issues.push(ValidationIssue::new(
                "replay.cache_directory",
                "must be an absolute path",
            ));
        }
        if !(MIN_REPLAY_MEMORY_MEGABYTES..=MAX_REPLAY_MEMORY_MEGABYTES)
            .contains(&self.replay.memory_limit_megabytes)
        {
            issues.push(ValidationIssue::new(
                "replay.memory_limit_megabytes",
                "must be between 16 and 512 MiB",
            ));
        }
        for (field, gain) in [
            (
                "audio.microphone_gain_percent",
                self.audio.microphone_gain_percent,
            ),
            (
                "audio.desktop_gain_percent",
                self.audio.desktop_gain_percent,
            ),
        ] {
            if gain > 200 {
                issues.push(ValidationIssue::new(
                    field,
                    "gain must be between 0 and 200 percent",
                ));
            }
        }
        for (field, device) in [
            ("audio.input_device", &self.audio.input_device),
            ("audio.output_device", &self.audio.output_device),
        ] {
            if device.as_ref().is_some_and(|id| {
                id.trim().is_empty() || id.len() > 1024 || id.chars().any(char::is_control)
            }) {
                issues.push(ValidationIssue::new(
                    field,
                    "device id must be nonempty and at most 1024 bytes without control characters",
                ));
            }
        }
        if self.replay.hotkey.parse::<Hotkey>().is_err() {
            issues.push(ValidationIssue::new(
                "replay.hotkey",
                "must be a supported shortcut such as Ctrl+Shift+F10",
            ));
        }
        if !NOTIFICATION_DURATION_SECONDS.contains(&self.notifications.duration_seconds) {
            issues.push(ValidationIssue::new(
                "notifications.duration_seconds",
                "must be between 2 and 10 seconds",
            ));
        }

        if !(1..=1024).contains(&self.performance.command_queue_capacity) {
            issues.push(ValidationIssue::new(
                "performance.command_queue_capacity",
                "must be between 1 and 1024 messages",
            ));
        }
        if !(1..=1024).contains(&self.performance.event_queue_capacity) {
            issues.push(ValidationIssue::new(
                "performance.event_queue_capacity",
                "must be between 1 and 1024 messages",
            ));
        }

        if parse_hex_color(&self.appearance.accent_color).is_none() {
            issues.push(ValidationIssue::new(
                "appearance.accent_color",
                "Use a color like #9B5CF6.",
            ));
        }

        if let Some(directory) = &self.recording.output_directory
            && !directory.is_absolute()
        {
            issues.push(ValidationIssue::new(
                "recording.output_directory",
                "must be an absolute path",
            ));
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(ConfigValidationError { issues })
        }
    }

    /// Resolves the settings needed to start one recording.
    ///
    /// Without a configured directory, recordings go to `<Videos>/Clipper`.
    pub fn recording_options(&self) -> Result<RecordingOptions, ConfigError> {
        let output_directory = match &self.recording.output_directory {
            Some(directory) => directory.clone(),
            None => default_recording_directory()?,
        };
        Ok(self.recording_options_in(output_directory))
    }

    /// Estimated recording size in decimal megabytes for `seconds`, using the
    /// same bitrates and audio tracks as a recording. Cheap: it skips the
    /// output-directory lookup, so it is safe to call while rendering.
    pub fn estimated_megabytes(&self, seconds: f64) -> f64 {
        self.recording_options_in(PathBuf::new())
            .estimated_megabytes(seconds)
    }

    fn recording_options_in(&self, output_directory: PathBuf) -> RecordingOptions {
        RecordingOptions {
            audio: RecordingAudioOptions {
                desktop: self.audio.desktop,
                microphone: self.audio.microphone,
                input_device: self.audio.input_device.clone(),
                output_device: self.audio.output_device.clone(),
                microphone_gain_percent: self.audio.microphone_gain_percent,
                desktop_gain_percent: self.audio.desktop_gain_percent,
                separate_tracks: self.audio.separate_tracks,
            },
            width: self.video.width,
            height: self.video.height,
            fps: self.video.fps,
            bitrate_mbps: self.video.bitrate_mbps,
            profile: self.recording.quality.into(),
            output_directory,
        }
    }

    /// Resolves the settings needed to start the instant replay buffer.
    ///
    /// Without a configured cache directory, the default
    /// `%LOCALAPPDATA%\Clipper\replay-cache` is used.
    pub fn replay_options(&self) -> Result<ReplayOptions, ConfigError> {
        let cache_directory = match &self.replay.cache_directory {
            Some(directory) => directory.clone(),
            None => default_replay_cache_directory()?,
        };
        Ok(ReplayOptions {
            recording: self.recording_options()?,
            duration: Duration::from_secs(u64::from(self.replay.duration_seconds)),
            memory_limit_bytes: u64::from(self.replay.memory_limit_megabytes) * 1024 * 1024,
            cache_directory: Some(cache_directory),
            disk_limit_bytes: u64::from(self.replay.disk_limit_megabytes) * 1024 * 1024,
        })
    }

    /// Saves this configuration to the per-user settings path.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&config_path()?)
    }

    fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };

        let mut config: Self = toml::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        // An explicit `[appearance] mode` always beats the legacy `[app] theme`,
        // even when it equals the default.
        let explicit_mode = toml::from_str::<toml::Table>(&contents)
            .ok()
            .and_then(|table| table.get("appearance")?.get("mode").cloned())
            .is_some();
        if explicit_mode {
            config.app.theme = None;
        }
        config.migrate();
        config.validate().map_err(ConfigError::Validation)?;
        Ok(config)
    }

    /// Upgrades values older versions accepted but this one rejects, so a
    /// saved file never stops startup. 4K at 120 fps (beyond H.264 Level 5.2)
    /// drops to the highest supported frame rate. The file is rewritten only
    /// when the user next changes a setting.
    fn migrate(&mut self) {
        let (width, height) = (self.video.width, self.video.height);
        if VIDEO_RESOLUTIONS.contains(&(width, height))
            && VIDEO_FPS.contains(&self.video.fps)
            && !h264_supports(width, height, self.video.fps)
            && let Some(&fps) = VIDEO_FPS
                .iter()
                .rev()
                .find(|&&fps| h264_supports(width, height, fps))
        {
            self.video.fps = fps;
        }
        // Older versions allowed any replay duration in 5..=300 s. Snap such
        // values to the nearest preset instead of failing startup; the
        // normalized value is persisted on the next settings change.
        if (5..=300).contains(&self.replay.duration_seconds) {
            self.replay.duration_seconds = nearest_replay_duration(self.replay.duration_seconds);
        }
        // The legacy `[app] theme` is read but never written again. Adopt it
        // only when `[appearance]` is untouched, so the new section wins.
        let legacy_theme = self.app.theme.take();
        if let Some(theme) = legacy_theme
            && self.appearance == AppearanceSettings::default()
        {
            self.appearance.mode = theme;
        }
        // A bad accent must never block startup: fall back, or normalize case.
        self.appearance.accent_color = match parse_hex_color(&self.appearance.accent_color) {
            Some(rgb) => format_hex_color(rgb),
            None => DEFAULT_ACCENT.to_owned(),
        };
    }

    fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate().map_err(ConfigError::Validation)?;
        let serialized = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        write_if_changed(path, serialized.as_bytes()).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: VideoCodec,
    pub encoder: EncoderPreference,
    pub bitrate_mbps: u32,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            codec: VideoCodec::H264,
            encoder: EncoderPreference::Auto,
            bitrate_mbps: 16,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    H264,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderPreference {
    #[default]
    Auto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplaySettings {
    pub mode: ReplayMode,
    pub duration_seconds: u32,
    /// RAM budget for Hybrid replay mode; older footage must spill to disk.
    pub memory_limit_megabytes: u32,
    pub hotkey: String,
    /// Start the replay buffer automatically when the app starts.
    pub enabled: bool,
    /// Absolute directory for temporary SSD segments; `None` means
    /// `%LOCALAPPDATA%\Clipper\replay-cache`.
    pub cache_directory: Option<PathBuf>,
    /// Upper bound for temporary SSD segments, in MiB.
    pub disk_limit_megabytes: u32,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            mode: ReplayMode::Hybrid,
            duration_seconds: 60,
            memory_limit_megabytes: 64,
            enabled: false,
            cache_directory: None,
            disk_limit_megabytes: 4096,
            hotkey: DEFAULT_SAVE_REPLAY.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub microphone_gain_percent: u16,
    pub desktop_gain_percent: u16,
    /// Future process-selection preference; the recorder currently uses full desktop playback.
    pub game: bool,
    /// The system output mix, excluding dedicated game and Discord streams.
    /// This avoids recording process audio twice when dedicated tracks are enabled.
    /// This crate stores the preference only; it does not capture or route audio.
    /// The exclusion policy above is for future recorder process routing.
    /// Current recording captures the full selected playback endpoint once,
    /// including game/Discord; those two future preferences do not filter it.
    pub desktop: bool,
    /// Future process-selection preference; not an independent recording source yet.
    pub discord: bool,
    pub microphone: bool,
    pub separate_tracks: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            microphone_gain_percent: 100,
            desktop_gain_percent: 100,
            game: true,
            desktop: true,
            discord: true,
            microphone: true,
            separate_tracks: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    /// Maximum pending UI-to-worker commands in a future bounded queue.
    pub command_queue_capacity: u32,
    /// Maximum pending worker-to-UI events in a future bounded queue.
    pub event_queue_capacity: u32,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            command_queue_capacity: 32,
            event_queue_capacity: 64,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    pub quality: QualityPreset,
    /// Absolute directory for recordings; `None` means `<Videos>/Clipper`.
    pub output_directory: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    Performance,
    #[default]
    Balanced,
    Quality,
}

impl From<QualityPreset> for QualityProfile {
    fn from(preset: QualityPreset) -> Self {
        match preset {
            QualityPreset::Performance => Self::Performance,
            QualityPreset::Balanced => Self::Balanced,
            QualityPreset::Quality => Self::Quality,
        }
    }
}

impl From<QualityProfile> for QualityPreset {
    fn from(profile: QualityProfile) -> Self {
        match profile {
            QualityProfile::Performance => Self::Performance,
            QualityProfile::Balanced => Self::Balanced,
            QualityProfile::Quality => Self::Quality,
        }
    }
}

/// Toast notifications shown after a clip is saved or fails to save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    /// Master switch for all notifications.
    pub enabled: bool,
    /// How long a notification stays visible.
    pub duration_seconds: u32,
    /// Play a sound with the notification.
    pub sound: bool,
    /// Offer an action to open the saved clip.
    pub open_clip_action: bool,
    /// Also notify when saving a clip fails.
    pub notify_on_failure: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_seconds: 4,
            sound: true,
            open_clip_action: true,
            notify_on_failure: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Legacy; superseded by `appearance.mode`. Read for migration only.
    #[serde(default, skip_serializing, deserialize_with = "lenient_option")]
    pub theme: Option<Theme>,
    /// Enables automatic game detection when a future backend supports it.
    pub automatic_detection: bool,
    /// Keeps manual game selection available alongside automatic detection.
    pub manual_detection: bool,
    pub start_on_login: bool,
    /// Whether the app should expose a tray icon when tray integration is added.
    pub tray_icon: bool,
    /// Whether closing the window should minimize to the tray when supported.
    pub minimize_to_tray: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: None,
            automatic_detection: true,
            manual_detection: true,
            start_on_login: false,
            tray_icon: true,
            minimize_to_tray: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    System,
}

/// Default accent color, `#RRGGBB`.
pub const DEFAULT_ACCENT: &str = "#9B5CF6";
/// Accent presets offered in Settings, in display order.
pub const ACCENT_PRESETS: [(&str, &str); 7] = [
    ("Purple", "#9B5CF6"),
    ("Blue", "#3B82F6"),
    ("Cyan", "#06B6D4"),
    ("Green", "#22C55E"),
    ("Orange", "#F97316"),
    ("Pink", "#EC4899"),
    ("Red", "#EF4444"),
];

/// `[appearance]` section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    #[serde(deserialize_with = "lenient_default")]
    pub mode: Theme,
    pub accent_color: String,
    #[serde(deserialize_with = "lenient_default")]
    pub density: UiDensity,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            mode: Theme::Dark,
            accent_color: DEFAULT_ACCENT.to_owned(),
            density: UiDensity::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDensity {
    Compact,
    #[default]
    Default,
    Relaxed,
}

/// Parses exactly `#RRGGBB` (trimmed, any hex case) into `0xRRGGBB`.
pub fn parse_hex_color(text: &str) -> Option<u32> {
    let digits = text.trim().strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Canonical stored form: `#RRGGBB`, uppercase.
pub fn format_hex_color(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xFF_FFFF)
}

// Unknown enum strings must not make the whole settings file unparsable.
fn lenient_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    Ok(toml::Value::deserialize(deserializer)?.try_into().ok())
}

fn lenient_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    lenient_option(deserializer).map(Option::unwrap_or_default)
}

/// `<Videos>/Clipper` for the current user.
pub fn default_recording_directory() -> Result<PathBuf, ConfigError> {
    let user_dirs = UserDirs::new().ok_or(ConfigError::NoVideoDirectory)?;
    let videos = user_dirs
        .video_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| user_dirs.home_dir().join("Videos"));
    Ok(videos.join("Clipper"))
}

/// Returns the default replay SSD cache directory
/// (`%LOCALAPPDATA%\Clipper\replay-cache`).
pub fn default_replay_cache_directory() -> Result<PathBuf, ConfigError> {
    let base = BaseDirs::new().ok_or(ConfigError::NoConfigDirectory)?;
    Ok(base.data_local_dir().join("Clipper").join("replay-cache"))
}

/// Returns the allowed replay duration closest to `seconds` (ties go lower).
pub fn nearest_replay_duration(seconds: u32) -> u32 {
    REPLAY_DURATIONS_SECONDS
        .iter()
        .copied()
        .min_by_key(|preset| preset.abs_diff(seconds))
        .unwrap_or(60)
}

/// Returns the settings file path in the current user's config directory.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let project_dirs =
        ProjectDirs::from("com", "Clipper", "ClippingExe").ok_or(ConfigError::NoConfigDirectory)?;
    Ok(project_dirs.config_dir().join("settings.toml"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    field: &'static str,
    message: &'static str,
}

impl ValidationIssue {
    const fn new(field: &'static str, message: &'static str) -> Self {
        Self { field, message }
    }

    pub fn field(&self) -> &'static str {
        self.field
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigValidationError {
    issues: Vec<ValidationIssue>,
}

impl ConfigValidationError {
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.issues
    }
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, issue) in self.issues.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{}: {}", issue.field(), issue.message())?;
        }
        Ok(())
    }
}

impl Error for ConfigValidationError {}

#[derive(Debug)]
pub enum ConfigError {
    NoConfigDirectory,
    NoVideoDirectory,
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    Serialize(toml::ser::Error),
    Validation(ConfigValidationError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoConfigDirectory => {
                formatter.write_str("could not determine the user config directory")
            }
            Self::NoVideoDirectory => {
                formatter.write_str("could not determine the user videos directory")
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "configuration I/O failed for {}: {source}",
                    path.display()
                )
            }
            Self::Parse { path, source } => {
                write!(
                    formatter,
                    "could not parse configuration at {}: {source}",
                    path.display()
                )
            }
            Self::Serialize(source) => {
                write!(formatter, "could not serialize configuration: {source}")
            }
            Self::Validation(source) => write!(formatter, "configuration is invalid: {source}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NoConfigDirectory | Self::NoVideoDirectory => None,
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::Validation(source) => Some(source),
        }
    }
}

/// Writes through a same-directory temporary file, then renames it into place.
/// Saves in this process are serialized; separate processes may race. On
/// Windows filesystems that cannot replace an existing file during rename, the
/// save fails while preserving the previous config.
fn write_if_changed(path: &Path, contents: &[u8]) -> io::Result<()> {
    let _save_guard = SAVE_LOCK
        .lock()
        .map_err(|_| io::Error::other("config save lock is poisoned"))?;

    match fs::read(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let (temp_path, mut temp_file) = create_temp_file(path)?;
    let mut temp_guard = TempFileGuard(Some(temp_path.clone()));
    temp_file.write_all(contents)?;
    temp_file.sync_all()?;
    drop(temp_file);

    fs::rename(&temp_path, path)?;
    temp_guard.remove();
    Ok(())
}

fn create_temp_file(destination: &Path) -> io::Result<(PathBuf, File)> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");

    for _ in 0..100 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique temporary config file",
    ))
}

struct TempFileGuard(Option<PathBuf>);

impl TempFileGuard {
    fn remove(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        self.remove();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let nonce = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "clipper-storage-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_match_the_bounded_phase_one_profile() {
        let config = AppConfig::default();

        assert_eq!(config.video.width, 1920);
        assert_eq!(config.video.height, 1080);
        assert_eq!(config.video.fps, 60);
        assert_eq!(config.video.codec, VideoCodec::H264);
        assert_eq!(config.video.encoder, EncoderPreference::Auto);
        assert_eq!(config.video.bitrate_mbps, 16);

        assert_eq!(config.replay.mode, ReplayMode::Hybrid);
        assert_eq!(config.replay.duration_seconds, 60);
        assert_eq!(config.replay.memory_limit_megabytes, 64);
        assert!(!config.replay.hotkey.trim().is_empty());

        assert!(config.audio.game);
        assert!(config.audio.desktop);
        assert!(config.audio.discord);
        assert!(config.audio.microphone);
        assert!(config.audio.separate_tracks);
        assert_eq!(config.performance.command_queue_capacity, 32);
        assert_eq!(config.performance.event_queue_capacity, 64);

        assert_eq!(config.recording.quality, QualityPreset::Balanced);
        assert_eq!(config.recording.output_directory, None);

        assert_eq!(config.appearance.mode, Theme::Dark);
        assert!(config.app.automatic_detection);
        assert!(config.app.manual_detection);
        assert!(!config.app.start_on_login);
        assert!(config.app.tray_icon);
        assert!(config.app.minimize_to_tray);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn toml_round_trip_preserves_the_complete_config() {
        let mut config = AppConfig::default();
        config.replay.hotkey = "Ctrl+Shift+F9".to_owned();
        config.audio.microphone = false;
        config.appearance.mode = Theme::Light;

        let toml = toml::to_string(&config).expect("serialize config");
        let loaded: AppConfig = toml::from_str(&toml).expect("deserialize config");

        assert_eq!(loaded, config);
    }

    #[test]
    fn parse_hex_color_accepts_only_rrggbb() {
        assert_eq!(parse_hex_color("#9B5CF6"), Some(0x9B5CF6));
        assert_eq!(parse_hex_color("#9b5cf6"), Some(0x9B5CF6));
        assert_eq!(parse_hex_color("  #22C55E \n"), Some(0x22C55E));
        for bad in ["9B5CF6", "#9B5", "#9B5CF6FF", "#GGGGGG", "", "#"] {
            assert_eq!(parse_hex_color(bad), None, "{bad:?}");
        }
        for (_, hex) in ACCENT_PRESETS {
            assert_eq!(format_hex_color(parse_hex_color(hex).unwrap()), hex);
        }
        assert_eq!(format_hex_color(0x0000AB), "#0000AB");
    }

    #[test]
    fn default_config_writes_appearance_and_no_legacy_theme() {
        let toml = toml::to_string(&AppConfig::default()).expect("serialize");
        assert!(toml.contains("[appearance]"));
        assert!(toml.contains("mode = \"dark\""));
        assert!(toml.contains("accent_color = \"#9B5CF6\""));
        assert!(toml.contains("density = \"default\""));
        assert!(!toml.contains("theme"));
    }

    fn load_text(text: &str) -> AppConfig {
        let dir = TestDir::new();
        let path = dir.path("settings.toml");
        fs::write(&path, text).expect("write config");
        AppConfig::load_from(&path).expect("load config")
    }

    #[test]
    fn legacy_app_theme_migrates_to_appearance_mode() {
        let config = load_text("[app]\ntheme = \"light\"\n");
        assert_eq!(config.appearance.mode, Theme::Light);
        assert_eq!(config.app.theme, None);

        let dir = TestDir::new();
        let path = dir.path("out.toml");
        config.save_to(&path).expect("save");
        let written = fs::read_to_string(&path).expect("read");
        assert!(!written.contains("theme"));
        assert!(written.contains("mode = \"light\""));

        // An explicit [appearance] wins over the legacy field.
        let config = load_text("[app]\ntheme = \"light\"\n[appearance]\ndensity = \"compact\"\n");
        assert_eq!(config.appearance.mode, Theme::Dark);
        // Even an explicit mode equal to the default beats the legacy field.
        let config = load_text("[app]\ntheme = \"light\"\n[appearance]\nmode = \"dark\"\n");
        assert_eq!(config.appearance.mode, Theme::Dark);
    }

    #[test]
    fn appearance_values_are_repaired_not_fatal() {
        let config = load_text("[appearance]\naccent_color = \"purple\"\n");
        assert_eq!(config.appearance.accent_color, DEFAULT_ACCENT);
        let config = load_text("[appearance]\naccent_color = \"#9b5cf6\"\n");
        assert_eq!(config.appearance.accent_color, "#9B5CF6");
        let config = load_text("[appearance]\ndensity = \"huge\"\nmode = \"neon\"\n");
        assert_eq!(config.appearance, AppearanceSettings::default());
        let config = load_text("[app]\ntheme = \"neon\"\n");
        assert_eq!(config.appearance.mode, Theme::Dark);

        let mut config = AppConfig::default();
        config.appearance.accent_color = "nope".to_owned();
        let error = config.validate().expect_err("bad accent");
        assert_eq!(error.issues()[0].field(), "appearance.accent_color");
        assert_eq!(error.issues()[0].message(), "Use a color like #9B5CF6.");
    }

    #[test]
    fn appearance_round_trips() {
        let config = AppConfig {
            appearance: AppearanceSettings {
                mode: Theme::Light,
                accent_color: "#22C55E".to_owned(),
                density: UiDensity::Compact,
            },
            ..AppConfig::default()
        };
        let loaded: AppConfig =
            toml::from_str(&toml::to_string(&config).expect("serialize")).expect("deserialize");
        assert_eq!(loaded, config);
    }

    #[test]
    fn validation_rejects_out_of_bounds_and_malformed_values() {
        let mut config = AppConfig::default();
        config.video.width = 1921;
        config.video.fps = 0;
        config.video.bitrate_mbps = 0;
        config.replay.duration_seconds = u32::MAX;
        config.replay.memory_limit_megabytes = 0;
        config.replay.hotkey = "  ".to_owned();
        config.performance.command_queue_capacity = 0;
        config.performance.event_queue_capacity = 1025;

        let error = config.validate().expect_err("invalid config should fail");
        let fields = error
            .issues()
            .iter()
            .map(|issue| issue.field())
            .collect::<Vec<_>>();

        assert!(fields.contains(&"video.width"));
        assert!(fields.contains(&"video.fps"));
        assert!(fields.contains(&"video.bitrate_mbps"));
        assert!(fields.contains(&"replay.duration_seconds"));
        assert!(fields.contains(&"replay.memory_limit_megabytes"));
        assert!(fields.contains(&"replay.hotkey"));
        assert!(fields.contains(&"performance.command_queue_capacity"));
        assert!(fields.contains(&"performance.event_queue_capacity"));
    }

    #[test]
    fn validation_accepts_every_offered_resolution_and_fps() {
        for (width, height) in VIDEO_RESOLUTIONS {
            for fps in VIDEO_FPS {
                let mut config = AppConfig::default();
                config.video.width = width;
                config.video.height = height;
                config.video.fps = fps;
                assert_eq!(
                    config.validate().is_ok(),
                    video_mode_supported(width, height, fps),
                    "{width}x{height}@{fps}"
                );
            }
        }

        let mut config = AppConfig::default();
        config.video.width = 1600;
        config.video.height = 900;
        assert!(config.validate().is_ok(), "1600x900");
    }

    fn with_mode(width: u32, height: u32, fps: u32) -> AppConfig {
        let mut config = AppConfig::default();
        config.video.width = width;
        config.video.height = height;
        config.video.fps = fps;
        config
    }

    #[test]
    fn validation_handles_900p_and_4k_frame_rate_limit() {
        assert!(with_mode(1600, 900, 60).validate().is_ok());
        assert!(with_mode(3840, 2160, 60).validate().is_ok());
        assert!(with_mode(2560, 1440, 120).validate().is_ok());

        let error = with_mode(3840, 2160, 120)
            .validate()
            .expect_err("4K@120 is unsupported");
        assert!(
            error
                .issues()
                .iter()
                .any(|issue| issue.field() == "video.fps")
        );
    }

    #[test]
    fn video_mode_supported_truth_table() {
        assert!(video_mode_supported(1600, 900, 120));
        assert!(video_mode_supported(3840, 2160, 60));
        assert!(!video_mode_supported(3840, 2160, 120));
        assert!(!video_mode_supported(1921, 1080, 60));
        assert!(!video_mode_supported(1920, 1080, 45));
    }

    #[test]
    fn config_with_900p_round_trips() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        let config = with_mode(1600, 900, 60);
        config.save_to(&path).expect("save config");
        assert_eq!(AppConfig::load_from(&path).expect("load config"), config);
    }

    #[test]
    fn validation_rejects_sub_720p_odd_fps_and_relative_output_directory() {
        let mut config = AppConfig::default();
        config.video.width = 640;
        config.video.height = 480;
        config.video.fps = 45;
        config.recording.output_directory = Some(PathBuf::from("relative/clips"));

        let fields = config
            .validate()
            .expect_err("invalid config should fail")
            .issues()
            .iter()
            .map(|issue| issue.field())
            .collect::<Vec<_>>();
        assert!(fields.contains(&"video.width"));
        assert!(fields.contains(&"video.fps"));
        assert!(fields.contains(&"recording.output_directory"));
    }

    #[test]
    fn recording_options_follow_settings() {
        let mut config = AppConfig::default();
        let directory = std::env::temp_dir().join("clipper-recordings");
        config.recording.output_directory = Some(directory.clone());
        config.recording.quality = QualityPreset::Quality;
        config.video.fps = 30;

        let options = config.recording_options().expect("resolve options");
        assert_eq!(options.output_directory, directory);
        assert_eq!(options.profile, QualityProfile::Quality);
        assert_eq!(
            (options.width, options.height, options.fps),
            (1920, 1080, 30)
        );
        assert_eq!(options.bitrate_mbps, 16);
    }

    #[test]
    fn default_storage_projection_estimates_exact_video_audio_and_hybrid_bytes() {
        let projection = AppConfig::default()
            .storage_projection()
            .expect("default config projects");

        assert_eq!(projection.video_bps, 16_000_000);
        assert_eq!(projection.audio_bps, 576_000);
        assert_eq!(projection.audio_tracks, 3);
        assert!(projection.variable_bitrate);
        assert_eq!(
            projection.sizes,
            [62_160_000, 124_320_000, 248_640_000, 621_600_000]
        );
        assert_eq!(projection.replay_duration_seconds, 60);
        assert_eq!(
            projection.replay,
            clipper_core::storage_estimate::HybridEstimate {
                total_bytes: 124_320_000,
                ram_budget_bytes: 67_108_864,
                estimated_ram_bytes: 67_108_864,
                estimated_ssd_bytes: 57_211_136,
            }
        );
    }

    #[test]
    fn storage_projection_uses_encoder_profile_bitrate_and_rate_control() {
        let mut config = AppConfig::default();
        config.recording.quality = QualityPreset::Performance;
        let performance = config.storage_projection().expect("performance projects");
        assert_eq!(performance.video_bps, 16_000_000);
        assert!(!performance.variable_bitrate);

        config.video.bitrate_mbps = 20;
        config.recording.quality = QualityPreset::Quality;
        let quality = config.storage_projection().expect("quality projects");
        assert_eq!(quality.video_bps, 30_000_000);
        assert!(quality.variable_bitrate);
        assert_eq!(quality.sizes[1], 229_320_000);
    }

    #[test]
    fn storage_projection_audio_tracks_follow_current_sources_and_ignore_future_flags() {
        for (desktop, microphone, separate_tracks, tracks, audio_bps, sixty_second_bytes) in [
            (false, false, true, 0, 0, 120_000_000),
            (true, true, false, 1, 192_000, 121_440_000),
            (false, true, true, 2, 384_000, 122_880_000),
            (true, true, true, 3, 576_000, 124_320_000),
        ] {
            let mut config = AppConfig::default();
            config.audio.desktop = desktop;
            config.audio.microphone = microphone;
            config.audio.separate_tracks = separate_tracks;
            // These are future process-capture preferences and do not create tracks.
            config.audio.game = true;
            config.audio.discord = true;

            let projection = config.storage_projection().expect("audio config projects");
            assert_eq!(projection.audio_tracks, tracks);
            assert_eq!(projection.audio_bps, audio_bps);
            assert_eq!(projection.sizes[1], sixty_second_bytes);
        }
    }

    #[test]
    fn storage_projection_keeps_target_bitrate_independent_of_fps_and_resolution() {
        for (width, height) in VIDEO_RESOLUTIONS {
            for fps in VIDEO_FPS {
                if !video_mode_supported(width, height, fps) {
                    continue;
                }

                let mut config = AppConfig::default();
                config.video.width = width;
                config.video.height = height;
                config.video.fps = fps;
                let projection = config.storage_projection().expect("video config projects");

                assert_eq!(projection.video_bps, 16_000_000, "{width}x{height}@{fps}");
                assert_eq!(projection.sizes[1], 124_320_000, "{width}x{height}@{fps}");
            }
        }
    }

    #[test]
    fn storage_projection_rejects_invalid_config() {
        let mut config = AppConfig::default();
        config.video.fps = 0;

        let error = config
            .storage_projection()
            .expect_err("invalid config cannot be projected");
        assert!(error.to_string().contains("video.fps"));
    }

    #[test]
    fn saved_4k_at_120_fps_loads_as_4k_at_60() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        let mut config = AppConfig::default();
        (config.video.width, config.video.height) = (3840, 2160);
        let text = toml::to_string_pretty(&config)
            .expect("serialize")
            .replace("fps = 60", "fps = 120");
        assert!(text.contains("fps = 120"));
        fs::write(&path, text).expect("write legacy file");

        let loaded = AppConfig::load_from(&path).expect("legacy 4K@120 still loads");
        assert_eq!(loaded.video.fps, 60);
        assert_eq!(loaded.video.width, 3840);
    }

    #[test]
    fn estimate_matches_recording_options_without_directory_lookup() {
        let config = AppConfig::default();
        let options = config
            .recording_options_in(PathBuf::new())
            .estimated_megabytes(60.0);
        assert_eq!(config.estimated_megabytes(60.0), options);
        // 16 Mbps video + 3 AAC tracks (0.576 Mbps) for one minute.
        assert!((config.estimated_megabytes(60.0) - 124.32).abs() < 1e-9);
    }

    #[test]
    fn missing_config_loads_defaults_and_save_load_round_trip() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");

        assert_eq!(
            AppConfig::load_from(&path).expect("defaults when missing"),
            AppConfig::default()
        );

        let config = AppConfig::default();
        config.save_to(&path).expect("save config");
        assert_eq!(
            AppConfig::load_from(&path).expect("load saved config"),
            config
        );
    }

    #[test]
    fn saving_identical_settings_does_not_replace_the_existing_file() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        let alias = temp.path("settings-hardlink.toml");
        let config = AppConfig::default();
        config.save_to(&path).expect("initial save");
        fs::hard_link(&path, &alias).expect("hard link saved config");

        config.save_to(&path).expect("repeat save");

        fs::write(&alias, b"changed through hard link").expect("write through alias");
        assert_eq!(
            fs::read(&path).expect("read config after repeat save"),
            b"changed through hard link"
        );
    }

    #[test]
    fn save_replaces_an_existing_config_with_new_valid_settings() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        let original = AppConfig::default();
        original.save_to(&path).expect("initial save");

        let mut updated = original;
        updated.video.bitrate_mbps = 20;
        updated.save_to(&path).expect("replace existing config");

        assert_eq!(
            AppConfig::load_from(&path).expect("load updated config"),
            updated
        );
        assert_eq!(
            fs::read_dir(&temp.0).expect("list temp directory").count(),
            1,
            "successful replacement should not leave temp files"
        );
    }

    #[test]
    fn load_rejects_malformed_or_invalid_existing_config() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");

        fs::write(&path, "[video\nwidth = 1920").expect("write malformed TOML");
        assert!(matches!(
            AppConfig::load_from(&path),
            Err(ConfigError::Parse { .. })
        ));

        fs::write(&path, "[video]\nwidth = 1921\n").expect("write invalid config");
        assert!(matches!(
            AppConfig::load_from(&path),
            Err(ConfigError::Validation(_))
        ));
    }
    #[test]
    fn audio_devices_gains_and_enabled_sources_reach_the_recorder_after_reload() {
        let temp = TestDir::new();
        let path = temp.path("audio.toml");
        let mut config = AppConfig::default();
        config.audio.input_device = Some("capture-endpoint-id".into());
        config.audio.output_device = Some("render-endpoint-id".into());
        config.audio.microphone_gain_percent = 50;
        config.audio.desktop_gain_percent = 150;
        config.audio.desktop = false;
        config.save_to(&path).unwrap();
        let restored = AppConfig::load_from(&path)
            .unwrap()
            .recording_options()
            .unwrap();
        assert_eq!(
            restored.audio.input_device.as_deref(),
            Some("capture-endpoint-id")
        );
        assert_eq!(
            restored.audio.output_device.as_deref(),
            Some("render-endpoint-id")
        );
        assert_eq!(restored.audio.microphone_gain_percent, 50);
        assert_eq!(restored.audio.desktop_gain_percent, 150);
        assert!(!restored.audio.desktop);
        assert!(restored.audio.microphone);
    }

    #[test]
    fn malformed_audio_devices_and_unbounded_gains_are_rejected() {
        let mut config = AppConfig::default();
        config.audio.input_device = Some("".into());
        config.audio.output_device = Some("bad\nid".into());
        config.audio.microphone_gain_percent = 201;
        config.audio.desktop_gain_percent = u16::MAX;
        let error = config.validate().unwrap_err();
        assert_eq!(error.issues().len(), 4);
    }

    #[test]
    fn hotkey_default_legacy_and_invalid_values() {
        let mut config = AppConfig::default();
        assert_eq!(config.replay.hotkey, "Ctrl+Shift+F10");
        assert!(config.validate().is_ok());
        config.replay.hotkey = "F9".to_owned();
        assert!(config.validate().is_ok());
        for bad in ["Ctrl+Space", "A"] {
            config.replay.hotkey = bad.to_owned();
            let err = config.validate().expect_err(bad);
            assert_eq!(err.issues()[0].field(), "replay.hotkey");
        }
    }

    #[test]
    fn notification_duration_bounds() {
        let mut config = AppConfig::default();
        for (secs, ok) in [(1, false), (2, true), (10, true), (11, false)] {
            config.notifications.duration_seconds = secs;
            let result = config.validate();
            assert_eq!(result.is_ok(), ok, "{secs}");
            if let Err(err) = result {
                assert_eq!(err.issues()[0].field(), "notifications.duration_seconds");
            }
        }
    }

    #[test]
    fn legacy_file_without_notifications_loads_and_custom_settings_round_trip() {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        fs::write(
            &path,
            "[replay]
hotkey = \"F9\"
",
        )
        .expect("write legacy");
        let legacy = AppConfig::load_from(&path).expect("load legacy");
        assert_eq!(legacy.notifications, NotificationSettings::default());

        let mut config = AppConfig::default();
        config.replay.hotkey = "Alt+F8".to_owned();
        config.notifications = NotificationSettings {
            enabled: false,
            duration_seconds: 7,
            sound: false,
            open_clip_action: false,
            notify_on_failure: false,
        };
        let path = temp.path("custom.toml");
        config.save_to(&path).expect("save");
        assert_eq!(AppConfig::load_from(&path).expect("load"), config);
    }

    #[test]
    fn replay_defaults_and_legacy_files_load() {
        let config = AppConfig::default();
        assert!(!config.replay.enabled);
        assert_eq!(config.replay.cache_directory, None);
        assert_eq!(config.replay.disk_limit_megabytes, 4096);
        let temp = TestDir::new();
        let path = temp.path("legacy.toml");
        fs::write(&path, "[replay]\nduration_seconds = 60\n").expect("write");
        assert_eq!(
            AppConfig::load_from(&path).expect("load").replay,
            config.replay
        );
    }

    #[test]
    fn replay_settings_round_trip() {
        let temp = TestDir::new();
        let mut config = AppConfig::default();
        config.replay.enabled = true;
        config.replay.duration_seconds = 120;
        config.replay.disk_limit_megabytes = 8192;
        config.replay.cache_directory = Some(temp.path("cache"));
        let path = temp.path("replay.toml");
        config.save_to(&path).expect("save");
        assert_eq!(AppConfig::load_from(&path).expect("load"), config);
    }

    #[test]
    fn legacy_replay_duration_is_normalized_on_load() {
        for (old, new) in [
            (5, 15),
            (22, 15),
            (23, 30),
            (45, 30),
            (46, 60),
            (200, 120),
            (300, 300),
        ] {
            let temp = TestDir::new();
            let path = temp.path("old.toml");
            fs::write(&path, format!("[replay]\nduration_seconds = {old}\n")).expect("write");
            let loaded = AppConfig::load_from(&path).expect("load");
            assert_eq!(loaded.replay.duration_seconds, new, "{old}");
        }
        let temp = TestDir::new();
        let path = temp.path("bad.toml");
        fs::write(&path, "[replay]\nduration_seconds = 4\n").expect("write");
        assert!(AppConfig::load_from(&path).is_err());
    }

    #[test]
    fn replay_validation_rejects_bad_values() {
        let mut config = AppConfig::default();
        config.replay.duration_seconds = 45;
        config.replay.disk_limit_megabytes = 255;
        config.replay.cache_directory = Some(PathBuf::from("relative"));
        let err = config.validate().expect_err("invalid");
        let fields: Vec<_> = err.issues().iter().map(ValidationIssue::field).collect();
        assert!(fields.contains(&"replay.duration_seconds"));
        assert!(fields.contains(&"replay.disk_limit_megabytes"));
        assert!(fields.contains(&"replay.cache_directory"));
        config.replay.duration_seconds = 60;
        config.replay.cache_directory = None;
        config.replay.disk_limit_megabytes = 65537;
        assert!(config.validate().is_err());
        config.replay.disk_limit_megabytes = 65536;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn replay_options_follow_settings() {
        let temp = TestDir::new();
        let mut config = AppConfig::default();
        config.recording.output_directory = Some(temp.path("clips"));
        config.replay.duration_seconds = 30;
        config.replay.memory_limit_megabytes = 128;
        config.replay.disk_limit_megabytes = 1024;
        config.replay.cache_directory = Some(temp.path("cache"));
        let options = config.replay_options().expect("options");
        assert_eq!(options.duration, Duration::from_secs(30));
        assert_eq!(options.memory_limit_bytes, 128 * 1024 * 1024);
        assert_eq!(options.disk_limit_bytes, 1024 * 1024 * 1024);
        assert_eq!(options.cache_directory, Some(temp.path("cache")));
        assert_eq!(options.recording.output_directory, temp.path("clips"));

        config.replay.cache_directory = None;
        let options = config.replay_options().expect("options");
        let cache = options.cache_directory.expect("default cache");
        assert!(cache.ends_with(Path::new("Clipper").join("replay-cache")));
    }
}
