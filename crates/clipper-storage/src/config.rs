use clipper_core::hotkey::{DEFAULT_SAVE_REPLAY, Hotkey};
use clipper_core::recording::{
    AudioSourceTarget, MAX_AUDIO_GAIN_PERCENT, MAX_RECORDED_AUDIO_SOURCES, QualityProfile,
    REPLAY_DURATIONS_SECONDS, RecordingAudioOptions, RecordingAudioSource, RecordingOptions,
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
        self.audio.validate_sources(&mut issues);
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
            audio: self.audio.recording_options(),
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
        // An explicit `[[audio.sources]]` beats the legacy audio keys.
        let explicit_sources = toml::from_str::<toml::Table>(&contents)
            .ok()
            .and_then(|table| table.get("audio")?.get("sources").cloned())
            .is_some();
        if explicit_sources {
            config.audio.clear_legacy();
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
        self.audio.migrate();
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

/// Most mixer rows kept in the settings.
pub const MAX_AUDIO_SOURCES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSourceType {
    Game,
    Application,
    Desktop,
    Microphone,
}

fn default_true() -> bool {
    true
}

fn default_gain() -> u16 {
    100
}

/// One `[[audio.sources]]` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSourceSettings {
    /// Stable and unique while the row exists.
    pub id: u32,
    pub kind: AudioSourceType,
    /// Application: executable file name (required). Desktop/Microphone:
    /// endpoint id, absent = Windows default. Game: unused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_gain")]
    pub gain_percent: u16,
}

impl AudioSourceSettings {
    /// `None` when the row is invalid (application without an executable).
    pub fn recording_target(&self) -> Option<AudioSourceTarget> {
        let target = self.target.clone();
        match self.kind {
            AudioSourceType::Game => Some(AudioSourceTarget::Game),
            AudioSourceType::Application => target
                .filter(|exe| !exe.trim().is_empty())
                .map(AudioSourceTarget::Application),
            AudioSourceType::Desktop => Some(AudioSourceTarget::Desktop(target)),
            AudioSourceType::Microphone => Some(AudioSourceTarget::Microphone(target)),
        }
    }
}

fn find_duplicate(
    rows: &[AudioSourceSettings],
    kind: AudioSourceType,
    target: Option<&str>,
) -> Option<u32> {
    rows.iter()
        .find(|row| {
            row.kind == kind
                && match kind {
                    AudioSourceType::Game => true,
                    AudioSourceType::Application => match (row.target.as_deref(), target) {
                        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                        _ => false,
                    },
                    _ => row.target.as_deref() == target,
                }
        })
        .map(|row| row.id)
}

/// Skips rows that fail to parse (unknown kind, missing id) instead of
/// rejecting the whole settings file.
fn lenient_sources<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<AudioSourceSettings>, D::Error> {
    let rows = Vec::<toml::Value>::deserialize(deserializer)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_into().ok())
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub separate_tracks: bool,
    /// Mixer order is vec order.
    #[serde(deserialize_with = "lenient_sources")]
    pub sources: Vec<AudioSourceSettings>,
    // Legacy keys: read for migration, never written. `game` and `discord`
    // were "future preference" flags that never captured anything, so they do
    // not become sources (that would silently start recording new audio).
    #[serde(skip_serializing)]
    input_device: Option<String>,
    #[serde(skip_serializing)]
    output_device: Option<String>,
    #[serde(skip_serializing)]
    microphone_gain_percent: Option<u16>,
    #[serde(skip_serializing)]
    desktop_gain_percent: Option<u16>,
    #[serde(skip_serializing)]
    game: Option<bool>,
    #[serde(skip_serializing)]
    desktop: Option<bool>,
    #[serde(skip_serializing)]
    discord: Option<bool>,
    #[serde(skip_serializing)]
    microphone: Option<bool>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        let source = |id, kind| AudioSourceSettings {
            id,
            kind,
            target: None,
            enabled: true,
            gain_percent: 100,
        };
        Self {
            separate_tracks: true,
            sources: vec![
                source(1, AudioSourceType::Desktop),
                source(2, AudioSourceType::Microphone),
            ],
            input_device: None,
            output_device: None,
            microphone_gain_percent: None,
            desktop_gain_percent: None,
            game: None,
            desktop: None,
            discord: None,
            microphone: None,
        }
    }
}

impl AudioSettings {
    pub fn next_source_id(&self) -> u32 {
        self.sources
            .iter()
            .map(|s| s.id)
            .max()
            .map_or(1, |id| id + 1)
    }

    /// Id of an existing row capturing the same audio.
    pub fn duplicate_of(&self, kind: AudioSourceType, target: Option<&str>) -> Option<u32> {
        find_duplicate(&self.sources, kind, target)
    }

    /// Enabled, valid sources in order.
    pub fn recording_options(&self) -> RecordingAudioOptions {
        RecordingAudioOptions {
            sources: self
                .sources
                .iter()
                .filter(|s| s.enabled)
                .filter_map(|s| {
                    Some(RecordingAudioSource {
                        id: s.id,
                        target: s.recording_target()?,
                        gain_percent: s.gain_percent.min(MAX_AUDIO_GAIN_PERCENT),
                    })
                })
                .collect(),
            separate_tracks: self.separate_tracks,
        }
    }

    fn clear_legacy(&mut self) {
        self.input_device = None;
        self.output_device = None;
        self.microphone_gain_percent = None;
        self.desktop_gain_percent = None;
        self.game = None;
        self.desktop = None;
        self.discord = None;
        self.microphone = None;
    }

    /// Turns legacy keys into sources, then repairs the list so a saved file
    /// never stops startup.
    fn migrate(&mut self) {
        let has_legacy = self.input_device.is_some()
            || self.output_device.is_some()
            || self.microphone_gain_percent.is_some()
            || self.desktop_gain_percent.is_some()
            || self.game.is_some()
            || self.desktop.is_some()
            || self.discord.is_some()
            || self.microphone.is_some();
        if has_legacy {
            let row =
                |id, kind, target, enabled: Option<bool>, gain: Option<u16>| AudioSourceSettings {
                    id,
                    kind,
                    target,
                    enabled: enabled.unwrap_or(true),
                    gain_percent: gain.unwrap_or(100),
                };
            self.sources = vec![
                row(
                    1,
                    AudioSourceType::Desktop,
                    self.output_device.take(),
                    self.desktop,
                    self.desktop_gain_percent,
                ),
                row(
                    2,
                    AudioSourceType::Microphone,
                    self.input_device.take(),
                    self.microphone,
                    self.microphone_gain_percent,
                ),
            ];
            self.clear_legacy();
        }
        let mut kept: Vec<AudioSourceSettings> = Vec::new();
        for mut row in std::mem::take(&mut self.sources) {
            row.gain_percent = row.gain_percent.min(MAX_AUDIO_GAIN_PERCENT);
            let invalid = kept.iter().any(|k| k.id == row.id)
                || row.recording_target().is_none()
                || find_duplicate(&kept, row.kind, row.target.as_deref()).is_some();
            if !invalid && kept.len() < MAX_AUDIO_SOURCES {
                kept.push(row);
            }
        }
        let mut enabled = 0;
        for row in &mut kept {
            if row.enabled {
                enabled += 1;
                row.enabled = enabled <= MAX_RECORDED_AUDIO_SOURCES;
            }
        }
        self.sources = kept;
    }

    fn validate_sources(&self, issues: &mut Vec<ValidationIssue>) {
        let mut problem =
            |message: &'static str| issues.push(ValidationIssue::new("audio.sources", message));
        if self.sources.len() > MAX_AUDIO_SOURCES {
            problem("at most 8 sources");
        }
        if self.sources.iter().filter(|s| s.enabled).count() > MAX_RECORDED_AUDIO_SOURCES {
            problem("at most 4 sources can be enabled");
        }
        for (index, row) in self.sources.iter().enumerate() {
            if self.sources[..index].iter().any(|s| s.id == row.id) {
                problem("source ids must be unique");
            }
            if find_duplicate(&self.sources[..index], row.kind, row.target.as_deref()).is_some() {
                problem("the same audio is listed twice");
            }
            if row.gain_percent > MAX_AUDIO_GAIN_PERCENT {
                problem("gain must be between 0 and 200 percent");
            }
            if row.recording_target().is_none() {
                problem("an application source needs an executable name");
            }
            if row.target.as_ref().is_some_and(|t| {
                t.trim().is_empty() || t.len() > 1024 || t.chars().any(char::is_control)
            }) {
                problem("target must be nonempty, at most 1024 bytes, without control characters");
            }
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
        config.audio.sources[1].enabled = false;
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
    fn storage_projection_audio_tracks_follow_enabled_sources() {
        for (desktop, microphone, separate_tracks, tracks, audio_bps, sixty_second_bytes) in [
            (false, false, true, 0, 0, 120_000_000),
            (true, true, false, 1, 192_000, 121_440_000),
            (false, true, true, 2, 384_000, 122_880_000),
            (true, true, true, 3, 576_000, 124_320_000),
        ] {
            let mut config = AppConfig::default();
            config.audio.sources[0].enabled = desktop;
            config.audio.sources[1].enabled = microphone;
            config.audio.separate_tracks = separate_tracks;

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
    fn source(id: u32, kind: AudioSourceType, target: Option<&str>) -> AudioSourceSettings {
        AudioSourceSettings {
            id,
            kind,
            target: target.map(str::to_owned),
            enabled: true,
            gain_percent: 100,
        }
    }

    fn load_toml(contents: &str) -> AppConfig {
        let temp = TestDir::new();
        let path = temp.path("settings.toml");
        fs::write(&path, contents).unwrap();
        AppConfig::load_from(&path).unwrap()
    }

    #[test]
    fn default_audio_sources_match_the_pre_mixer_recording() {
        let audio = AudioSettings::default();
        assert_eq!(
            audio.sources,
            [
                source(1, AudioSourceType::Desktop, None),
                source(2, AudioSourceType::Microphone, None)
            ]
        );
        assert!(audio.separate_tracks);
        let options = audio.recording_options();
        assert_eq!(options.sources.len(), 2);
        assert_eq!(options.track_count(), 3);
    }

    #[test]
    fn legacy_audio_keys_migrate_to_sources_and_are_never_written() {
        let temp = TestDir::new();
        let path = temp.path("legacy.toml");
        fs::write(
            &path,
            "[audio]\ndesktop = false\nmicrophone = true\ninput_device = \"X\"\nmicrophone_gain_percent = 150\ngame = true\ndiscord = true\n",
        )
        .unwrap();
        let config = AppConfig::load_from(&path).unwrap();
        let mut desktop = source(1, AudioSourceType::Desktop, None);
        desktop.enabled = false;
        let mut mic = source(2, AudioSourceType::Microphone, Some("X"));
        mic.gain_percent = 150;
        assert_eq!(config.audio.sources, [desktop, mic]);
        let options = config.audio.recording_options();
        assert_eq!(options.sources.len(), 1);
        assert_eq!(
            options.sources[0].target,
            AudioSourceTarget::Microphone(Some("X".into()))
        );

        config.save_to(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("[[audio.sources]]"));
        for key in [
            "input_device",
            "output_device",
            "microphone_gain_percent",
            "desktop_gain_percent",
            "game",
            "discord",
            "desktop =",
            "microphone =",
        ] {
            assert!(!written.contains(key), "{key} was written:\n{written}");
        }
        assert_eq!(AppConfig::load_from(&path).unwrap(), config);
    }

    #[test]
    fn legacy_game_and_discord_never_become_sources() {
        let config = load_toml("[audio]\ngame = true\ndiscord = true\n");
        assert_eq!(config.audio.sources, AudioSettings::default().sources);
    }

    #[test]
    fn explicit_sources_win_over_legacy_keys() {
        let config = load_toml(
            "[audio]\nmicrophone = false\ninput_device = \"X\"\n[[audio.sources]]\nid = 7\nkind = \"game\"\n",
        );
        assert_eq!(
            config.audio.sources,
            [source(7, AudioSourceType::Game, None)]
        );
    }

    #[test]
    fn broken_audio_sources_are_repaired_on_load() {
        let config = load_toml(
            r#"
[[audio.sources]]
id = 1
kind = "desktop"
gain_percent = 999
[[audio.sources]]
id = 1
kind = "microphone"
[[audio.sources]]
id = 2
kind = "application"
[[audio.sources]]
id = 3
kind = "teleporter"
[[audio.sources]]
id = 4
kind = "application"
target = "Discord.exe"
[[audio.sources]]
id = 5
kind = "application"
target = "discord.EXE"
[[audio.sources]]
id = 6
kind = "game"
[[audio.sources]]
id = 7
kind = "microphone"
[[audio.sources]]
id = 8
kind = "microphone"
target = "a"
[[audio.sources]]
id = 9
kind = "microphone"
target = "b"
"#,
        );
        let ids: Vec<_> = config.audio.sources.iter().map(|s| s.id).collect();
        assert_eq!(ids, [1, 4, 6, 7, 8, 9]);
        assert_eq!(config.audio.sources[0].gain_percent, 200);
        let enabled: Vec<_> = config.audio.sources.iter().map(|s| s.enabled).collect();
        assert_eq!(enabled, [true, true, true, true, false, false]);
    }

    #[test]
    fn too_many_rows_are_truncated_on_load() {
        let rows: String = (1..=10)
            .map(|id| {
                format!(
                    "[[audio.sources]]\nid = {id}\nkind = \"application\"\ntarget = \"a{id}.exe\"\nenabled = false\n"
                )
            })
            .collect();
        assert_eq!(load_toml(&rows).audio.sources.len(), MAX_AUDIO_SOURCES);
    }

    #[test]
    fn programmatic_invalid_audio_sources_are_rejected() {
        let mut config = AppConfig::default();
        assert!(config.validate().is_ok());
        let bad: [fn(&mut AudioSettings); 6] = [
            |a| a.sources[1].id = 1,
            |a| a.sources[0].gain_percent = 201,
            |a| {
                a.sources
                    .push(source(3, AudioSourceType::Application, None))
            },
            |a| a.sources.push(source(3, AudioSourceType::Desktop, None)),
            |a| {
                for id in 3..=9 {
                    let exe = format!("{id}.exe");
                    a.sources
                        .push(source(id, AudioSourceType::Application, Some(&exe)));
                }
            },
            |a| a.sources[0].target = Some("bad\nid".into()),
        ];
        for change in bad {
            config.audio = AudioSettings::default();
            change(&mut config.audio);
            let error = config.validate().unwrap_err();
            assert!(error.issues().iter().all(|i| i.field() == "audio.sources"));
        }
        // 5 enabled sources
        config.audio = AudioSettings::default();
        for id in 3..=5 {
            let exe = format!("{id}.exe");
            config
                .audio
                .sources
                .push(source(id, AudioSourceType::Application, Some(&exe)));
        }
        assert!(config.validate().is_err());
    }

    #[test]
    fn duplicate_of_and_next_source_id() {
        let mut audio = AudioSettings::default();
        assert_eq!(audio.next_source_id(), 3);
        audio.sources.clear();
        assert_eq!(audio.next_source_id(), 1);
        let mut audio = AudioSettings::default();
        assert_eq!(audio.duplicate_of(AudioSourceType::Desktop, None), Some(1));
        assert_eq!(
            audio.duplicate_of(AudioSourceType::Desktop, Some("x")),
            None
        );
        assert_eq!(
            audio.duplicate_of(AudioSourceType::Microphone, None),
            Some(2)
        );
        assert_eq!(audio.duplicate_of(AudioSourceType::Game, None), None);
        audio
            .sources
            .push(source(3, AudioSourceType::Application, Some("Discord.exe")));
        audio.sources.push(source(4, AudioSourceType::Game, None));
        assert_eq!(
            audio.duplicate_of(AudioSourceType::Application, Some("discord.EXE")),
            Some(3)
        );
        assert_eq!(
            audio.duplicate_of(AudioSourceType::Application, Some("o.exe")),
            None
        );
        assert_eq!(audio.duplicate_of(AudioSourceType::Game, None), Some(4));
        assert_eq!(audio.next_source_id(), 5);
    }

    #[test]
    fn recording_options_skip_disabled_sources_and_keep_order() {
        let mut audio = AudioSettings::default();
        audio
            .sources
            .insert(0, source(9, AudioSourceType::Game, None));
        audio.sources[1].enabled = false;
        audio.sources[2].gain_percent = 150;
        let options = audio.recording_options();
        let ids: Vec<_> = options.sources.iter().map(|s| s.id).collect();
        assert_eq!(ids, [9, 2]);
        assert_eq!(options.sources[0].target, AudioSourceTarget::Game);
        assert_eq!(options.sources[1].gain_percent, 150);
    }

    #[test]
    fn audio_sources_survive_save_and_reload() {
        let temp = TestDir::new();
        let path = temp.path("audio.toml");
        let mut config = AppConfig::default();
        config.audio.sources[0].target = Some("render-endpoint-id".into());
        config.audio.sources[0].enabled = false;
        config.audio.sources[1].gain_percent = 50;
        config
            .audio
            .sources
            .push(source(3, AudioSourceType::Application, Some("Game.exe")));
        config.save_to(&path).unwrap();
        let restored = AppConfig::load_from(&path).unwrap();
        assert_eq!(restored, config);
        let options = restored.recording_options().unwrap().audio;
        assert_eq!(options.sources.len(), 2);
        assert_eq!(options.sources[0].gain_percent, 50);
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
