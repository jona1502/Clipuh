//! `[diagnostics]` settings: defaults, expiry and backward compatibility.

use clipper_storage::{AppConfig, DETAILED_LOGGING_HOURS, DetailedLogLevel, DiagnosticsSettings};

const NOW: u64 = 1_800_000_000;

#[test]
fn files_without_the_section_keep_default_info_logging() {
    let config: AppConfig = toml::from_str("[video]\nfps = 60\n").expect("old file parses");
    assert_eq!(config.diagnostics, DiagnosticsSettings::default());
    assert_eq!(config.diagnostics.active_detailed_level(NOW), None);
}

#[test]
fn malformed_values_fall_back_instead_of_failing_the_file() {
    let config: AppConfig =
        toml::from_str("[diagnostics]\ndetailed_logging = true\ndetailed_level = \"loud\"\n")
            .expect("unknown level is tolerated");
    assert_eq!(config.diagnostics.detailed_level, DetailedLogLevel::Debug);
    // No start time: treated as expired, never "on forever".
    assert_eq!(config.diagnostics.active_detailed_level(NOW), None);
}

#[test]
fn detailed_logging_expires_after_the_limit() {
    let mut d = DiagnosticsSettings {
        detailed_level: DetailedLogLevel::Trace,
        ..Default::default()
    };
    d.set_detailed(true, NOW);
    assert_eq!(d.active_detailed_level(NOW), Some(DetailedLogLevel::Trace));
    let limit = DETAILED_LOGGING_HOURS * 3600;
    assert_eq!(
        d.active_detailed_level(NOW + limit - 1),
        Some(DetailedLogLevel::Trace)
    );
    assert_eq!(d.active_detailed_level(NOW + limit), None);
    // A clock set backwards does not extend it either.
    assert_eq!(d.active_detailed_level(NOW - 10), None);
    d.set_detailed(false, NOW);
    assert_eq!(d.active_detailed_level(NOW), None);
    assert_eq!(d.detailed_since, None);
}

#[test]
fn diagnostics_settings_round_trip_through_toml() {
    let mut config = AppConfig::default();
    config.diagnostics.set_detailed(true, NOW);
    config.diagnostics.detailed_level = DetailedLogLevel::Trace;
    let text = toml::to_string(&config).expect("serialize");
    assert!(text.contains("[diagnostics]"));
    let loaded: AppConfig = toml::from_str(&text).expect("parse");
    assert_eq!(loaded.diagnostics, config.diagnostics);
}
