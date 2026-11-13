pub mod config;

pub use config::{
    AppConfig, ConfigError, ConfigValidationError, NOTIFICATION_DURATION_SECONDS,
    NotificationSettings, QualityPreset, VIDEO_FPS, VIDEO_RESOLUTIONS, config_path,
    default_recording_directory,
};
