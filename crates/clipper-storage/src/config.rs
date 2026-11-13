use clipper_core::hotkey::{DEFAULT_SAVE_REPLAY, Hotkey};
use clipper_core::recording::{QualityProfile, RecordingOptions};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Output resolutions offered by the recorder: 720p, 1080p, 1440p, 4K.
pub const VIDEO_RESOLUTIONS: [(u32, u32); 4] =
    [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];
/// Frame rates offered by the recorder.
pub const VIDEO_FPS: [u32; 3] = [30, 60, 120];
const MAX_BITRATE_MBPS: u32 = 100;
const MIN_REPLAY_DURATION_SECONDS: u32 = 5;
const MAX_REPLAY_DURATION_SECONDS: u32 = 300;
const MIN_REPLAY_MEMORY_MEGABYTES: u32 = 16;
const MAX_REPLAY_MEMORY_MEGABYTES: u32 = 512;
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
                "resolution must be 1280x720, 1920x1080, 2560x1440 or 3840x2160",
            ));
        }
        if !VIDEO_FPS.contains(&self.video.fps) {
            issues.push(ValidationIssue::new(
                "video.fps",
                "must be 30, 60 or 120 frames per second",
            ));
        }
        if self.video.bitrate_mbps == 0 || self.video.bitrate_mbps > MAX_BITRATE_MBPS {
            issues.push(ValidationIssue::new(
                "video.bitrate_mbps",
                "must be between 1 and 100 Mbps",
            ));
        }

        if !(MIN_REPLAY_DURATION_SECONDS..=MAX_REPLAY_DURATION_SECONDS)
            .contains(&self.replay.duration_seconds)
        {
            issues.push(ValidationIssue::new(
                "replay.duration_seconds",
                "must be between 5 and 300 seconds",
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
        Ok(RecordingOptions {
            width: self.video.width,
            height: self.video.height,
            fps: self.video.fps,
            bitrate_mbps: self.video.bitrate_mbps,
            profile: self.recording.quality.into(),
            output_directory,
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

        let config: Self = toml::from_str(&contents).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        config.validate().map_err(ConfigError::Validation)?;
        Ok(config)
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
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self {
            mode: ReplayMode::Hybrid,
            duration_seconds: 60,
            memory_limit_megabytes: 64,
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
    pub game: bool,
    /// The system output mix, excluding dedicated game and Discord streams.
    /// This avoids recording process audio twice when dedicated tracks are enabled.
    /// This crate stores the preference only; it does not capture or route audio.
    pub desktop: bool,
    pub discord: bool,
    pub microphone: bool,
    pub separate_tracks: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
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
    pub theme: Theme,
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
            theme: Theme::Dark,
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

/// `<Videos>/Clipper` for the current user.
pub fn default_recording_directory() -> Result<PathBuf, ConfigError> {
    let user_dirs = UserDirs::new().ok_or(ConfigError::NoVideoDirectory)?;
    let videos = user_dirs
        .video_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| user_dirs.home_dir().join("Videos"));
    Ok(videos.join("Clipper"))
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

        assert_eq!(config.app.theme, Theme::Dark);
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
        config.app.theme = Theme::Light;

        let toml = toml::to_string(&config).expect("serialize config");
        let loaded: AppConfig = toml::from_str(&toml).expect("deserialize config");

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
                assert!(config.validate().is_ok(), "{width}x{height}@{fps}");
            }
        }
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
}
