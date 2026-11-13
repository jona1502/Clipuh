pub mod config;

pub use config::{
    AppConfig, ConfigError, ConfigValidationError, QualityPreset, VIDEO_FPS, VIDEO_RESOLUTIONS,
    config_path, default_recording_directory,
};
