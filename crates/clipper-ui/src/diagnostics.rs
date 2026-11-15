//! Glue between the app and `clipper-diagnostics`: applies the logging
//! preference, logs recorder events, and builds the debug-log export.
//!
//! Privacy: log lines name event kinds, counts, encoder names and file
//! *names* only. Full paths, capture-source titles and device names are not
//! logged; the export additionally redacts the user's profile path and name.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clipper_core::recording::{RecorderEvent, ReplayStatus};
use clipper_diagnostics::{ExportSummary, SystemInfo, Verbosity, log};
use clipper_storage::{AppConfig, DetailedLogLevel, DiagnosticsSettings};

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Level the logger should run at for `settings` at `now`.
pub(crate) fn verbosity_for(settings: &DiagnosticsSettings, now: u64) -> Verbosity {
    match settings.active_detailed_level(now) {
        Some(DetailedLogLevel::Debug) => Verbosity::Debug,
        Some(DetailedLogLevel::Trace) => Verbosity::Trace,
        None => Verbosity::Info,
    }
}

/// Applies the saved logging preference (at startup and when it changes).
/// Returns the level in effect, or `None` when file logging is unavailable.
pub fn apply_log_settings(settings: &DiagnosticsSettings) -> Option<Verbosity> {
    let verbosity = verbosity_for(settings, now_unix());
    match clipper_diagnostics::set_verbosity(verbosity) {
        Ok(()) => {
            log::info!("log level {}", verbosity.label());
            Some(verbosity)
        }
        Err(_) => None,
    }
}

/// File name without its folders (a full path names the user).
fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "?".into(), |name| name.to_string_lossy().into_owned())
}

#[derive(Default)]
struct EventLog {
    dropped: u64,
    warning: Option<String>,
    statuses: u64,
}

thread_local! {
    static EVENT_LOG: RefCell<EventLog> = RefCell::new(EventLog::default());
}

/// Logs one recorder event. Cheap when the level is filtered out; replay
/// status (1 Hz) is summarised at Debug every 10 s, and on every change of
/// dropped frames or warnings.
pub(crate) fn log_event(event: &RecorderEvent) {
    match event {
        RecorderEvent::ReplayStarted { encoder } => {
            log::info!("replay started (encoder {encoder})")
        }
        RecorderEvent::ReplayStopped { error: None } => log::info!("replay stopped"),
        RecorderEvent::ReplayStopped { error: Some(error) } => {
            log::error!("replay stopped with an error: {error}")
        }
        RecorderEvent::ReplaySaving => log::info!("save clip: request accepted, saving"),
        RecorderEvent::ReplaySaved { path, duration } => log::info!(
            "save clip: completed {} ({:.1} s)",
            file_name(path),
            duration.as_secs_f64()
        ),
        RecorderEvent::ReplaySaveFailed { message } => log::error!("save clip: failed: {message}"),
        RecorderEvent::RecordingStarted { path, encoder } => {
            log::info!("recording started {} (encoder {encoder})", file_name(path))
        }
        RecorderEvent::RecordingFinished {
            path,
            stats,
            reason,
        } => log::info!(
            "recording finished {} ({reason:?}; {} captured, {} encoded, {} dropped)",
            file_name(path),
            stats.frames_captured,
            stats.frames_encoded,
            stats.frames_dropped
        ),
        RecorderEvent::RecordingFailed { message } => log::error!("recording failed: {message}"),
        RecorderEvent::Error { message } => log::error!("recorder error: {message}"),
        RecorderEvent::AudioTestFinished {
            error: Some(error), ..
        } => log::warn!("audio test failed: {error}"),
        RecorderEvent::AudioSources(states) => {
            log::debug!("audio sources: {} reported", states.len())
        }
        RecorderEvent::SourcesListed(sources) => {
            log::debug!("capture sources listed: {}", sources.len())
        }
        RecorderEvent::ReplayStatus(status) => log_replay_status(status),
        RecorderEvent::Stopped => log::warn!("recorder stopped"),
        RecorderEvent::AudioLevels(_)
        | RecorderEvent::AudioTestStarted
        | RecorderEvent::AudioTestFinished { error: None, .. }
        | RecorderEvent::Stats(_) => {}
    }
}

fn log_replay_status(status: &ReplayStatus) {
    EVENT_LOG.with(|state| {
        let mut state = state.borrow_mut();
        state.statuses += 1;
        let stats = &status.stats;
        if stats.frames_dropped > state.dropped {
            log::warn!(
                "replay dropped {} frame(s) (total {})",
                stats.frames_dropped - state.dropped,
                stats.frames_dropped
            );
        }
        state.dropped = stats.frames_dropped;
        if status.warning != state.warning {
            if let Some(warning) = &status.warning {
                log::warn!("replay warning: {warning}");
            }
            state.warning = status.warning.clone();
        }
        if state.statuses % 10 == 1 {
            log::debug!(
                "replay status: buffered {:.1}/{:.0} s, tier {:?}, ram {} KiB, disk {} KiB, \
                 exports {}, fps {:.1}, captured {}, encoded {}, dropped {}",
                status.buffered.as_secs_f64(),
                status.configured.as_secs_f64(),
                status.tier,
                status.ram_bytes / 1024,
                status.disk_bytes / 1024,
                status.exports_in_flight,
                stats.current_fps,
                stats.frames_captured,
                stats.frames_encoded,
                stats.frames_dropped
            );
        }
    });
}

/// Facts for `system-info.txt`. Only values the app actually knows; capture
/// settings are numbers only (no folders, device or window names).
pub(crate) fn system_info(
    config: &AppConfig,
    encoder: Option<&str>,
    recent_error: Option<&str>,
) -> SystemInfo {
    let gpus = clipper_platform::windows::system_info::gpu_adapters();
    let video = &config.video;
    let replay = &config.replay;
    let enabled_sources = config.audio.sources.iter().filter(|s| s.enabled).count();
    SystemInfo {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        windows_version: clipper_platform::windows::system_info::windows_version(),
        gpu: (!gpus.is_empty()).then(|| gpus.join("; ")),
        capture_backend: Some("Windows Graphics Capture".into()),
        encoder: encoder.map(|name| format!("Media Foundation H.264 ({name})")),
        capture_settings: vec![
            (
                "resolution".into(),
                format!("{}x{}", video.width, video.height),
            ),
            ("fps".into(), video.fps.to_string()),
            ("bitrate".into(), format!("{} Mbps", video.bitrate_mbps)),
            ("codec".into(), format!("{:?}", video.codec)),
            ("encoder preference".into(), format!("{:?}", video.encoder)),
            ("quality".into(), format!("{:?}", config.recording.quality)),
            ("replay".into(), format!("{} s", replay.duration_seconds)),
            ("replay enabled".into(), replay.enabled.to_string()),
            ("audio sources enabled".into(), enabled_sources.to_string()),
            (
                "log level".into(),
                clipper_diagnostics::current_verbosity()
                    .map_or("unavailable", Verbosity::label)
                    .to_owned(),
            ),
        ],
        recent_errors: recent_error.map(str::to_owned).into_iter().collect(),
    }
}

/// `clipuh-debug-logs-YYYYMMDD-HHMMSS.zip` (UTC).
pub(crate) fn export_file_name(now: u64) -> String {
    let (days, seconds) = (now / 86_400, now % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "clipuh-debug-logs-{year:04}{month:02}{day:02}-{:02}{:02}{:02}.zip",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Where the save dialog opens: the Desktop if it exists, else the profile.
pub(crate) fn export_directory() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    home.as_ref()
        .map(|home| home.join("Desktop"))
        .filter(|desktop| desktop.is_dir())
        .or(home)
        .unwrap_or_else(std::env::temp_dir)
}

/// Blocking: flushes the logger and writes the redacted archive.
pub(crate) fn export(destination: &Path, info: &SystemInfo) -> Result<ExportSummary, String> {
    let directory =
        clipper_diagnostics::log_dir().unwrap_or_else(clipper_diagnostics::default_log_dir);
    clipper_diagnostics::flush();
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let user = std::env::var("USERNAME").ok();
    clipper_diagnostics::export_zip(
        &directory,
        destination,
        info,
        home.as_deref(),
        user.as_deref(),
    )
    .map_err(|error| error.to_string())
}

/// Log folder shown and opened from Settings.
pub(crate) fn log_directory() -> PathBuf {
    clipper_diagnostics::log_dir().unwrap_or_else(clipper_diagnostics::default_log_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_follows_the_detailed_setting_and_its_expiry() {
        let now = 1_800_000_000;
        let mut settings = DiagnosticsSettings::default();
        assert_eq!(verbosity_for(&settings, now), Verbosity::Info);
        settings.set_detailed(true, now);
        assert_eq!(verbosity_for(&settings, now + 60), Verbosity::Debug);
        settings.detailed_level = DetailedLogLevel::Trace;
        assert_eq!(verbosity_for(&settings, now + 60), Verbosity::Trace);
        assert_eq!(verbosity_for(&settings, now + 25 * 3600), Verbosity::Info);
    }

    #[test]
    fn export_names_are_dated_and_zip() {
        assert_eq!(export_file_name(0), "clipuh-debug-logs-19700101-000000.zip");
        // 2026-11-14 18:03:11 UTC.
        assert_eq!(
            export_file_name(1_794_679_391),
            "clipuh-debug-logs-20261114-180311.zip"
        );
        assert_eq!(
            export_file_name(951_782_400),
            "clipuh-debug-logs-20000229-000000.zip"
        );
    }

    #[test]
    fn logged_file_names_drop_their_folders() {
        let path = Path::new(r"C:\Users\someone\Videos\clipuh\Clip 1.mp4");
        assert_eq!(file_name(path), "Clip 1.mp4");
    }

    #[test]
    fn system_info_carries_settings_but_no_paths() {
        let config = AppConfig::default();
        let info = system_info(&config, Some("AMDh264Encoder"), None);
        assert_eq!(info.app_version, env!("CARGO_PKG_VERSION"));
        assert!(info.encoder.as_deref().unwrap().contains("AMDh264Encoder"));
        for (key, value) in &info.capture_settings {
            assert!(
                !value.contains('\\') && !value.contains('/'),
                "{key}: {value}"
            );
        }
    }
}
