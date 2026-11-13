pub mod config;

pub use config::{
    AppConfig, ConfigError, ConfigValidationError, NOTIFICATION_DURATION_SECONDS,
    NotificationSettings, QualityPreset, REPLAY_DISK_LIMIT_MEGABYTES, VIDEO_FPS, VIDEO_RESOLUTIONS,
    config_path, default_recording_directory, default_replay_cache_directory,
};
