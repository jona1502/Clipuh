pub mod config;
pub mod storage_projection;

pub use config::{
    ACCENT_PRESETS, AppConfig, AppearanceSettings, ConfigError, ConfigValidationError,
    DEFAULT_ACCENT, NOTIFICATION_DURATION_SECONDS, NotificationSettings, QualityPreset,
    REPLAY_DISK_LIMIT_MEGABYTES, Theme, UiDensity, VIDEO_FPS, VIDEO_RESOLUTIONS, config_path,
    default_recording_directory, default_replay_cache_directory, format_hex_color, parse_hex_color,
    video_mode_supported,
};
pub use storage_projection::{StorageProjection, StorageProjectionError};
