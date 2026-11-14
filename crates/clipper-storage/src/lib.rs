pub mod config;
pub mod library;
pub mod notification_sound;
pub mod storage_projection;

pub use config::{
    ACCENT_PRESETS, AppConfig, AppearanceSettings, AudioSettings, AudioSourceSettings,
    AudioSourceType, ConfigError, ConfigValidationError, DEFAULT_ACCENT, DETAILED_LOGGING_HOURS,
    DetailedLogLevel, DiagnosticsSettings, LibrarySettings, LibrarySort, LibraryView,
    MAX_AUDIO_SOURCES, NOTIFICATION_DURATION_SECONDS, NotificationSettings, QualityPreset,
    REPLAY_DISK_LIMIT_MEGABYTES, Theme, UiDensity, VIDEO_FPS, VIDEO_RESOLUTIONS, config_path,
    default_recording_directory, default_replay_cache_directory, format_hex_color, parse_hex_color,
    video_mode_supported,
};
pub use notification_sound::{
    NotificationSoundError, import_notification_sound, import_notification_sound_to,
    import_notification_wav_bytes, store_notification_wav,
};
pub use storage_projection::{StorageProjection, StorageProjectionError};
