//! GPUI-free view state for the recording panel.

use std::path::PathBuf;
use std::time::Duration;

use clipper_core::recording::{
    CaptureSource, RecorderEvent, RecordingStats, SourceKind, StopReason,
};

/// Orders background saves by UI intent, including when a newer save fails.
#[derive(Debug, Default)]
pub struct SettingsSaveOrder {
    requested: std::sync::atomic::AtomicU64,
    writer: std::sync::Mutex<()>,
}

impl SettingsSaveOrder {
    pub fn request(&self) -> u64 {
        self.requested
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    pub fn save(
        &self,
        revision: u64,
        write: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| "Settings save lock poisoned".to_owned())?;
        if revision != self.requested.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        write()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    /// Stop was requested; the encoder is finalizing the file.
    Stopping,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    pub path: PathBuf,
    pub stats: RecordingStats,
    pub reason: StopReason,
}

#[derive(Debug, Default)]
pub struct RecordingState {
    pub sources: Vec<CaptureSource>,
    pub selected: Option<(SourceKind, u64)>,
    pub phase: Phase,
    pub encoder: Option<String>,
    pub path: Option<PathBuf>,
    pub stats: RecordingStats,
    pub finished: Option<Finished>,
    pub error: Option<String>,
}

impl RecordingState {
    pub fn apply(&mut self, event: RecorderEvent) {
        match event {
            RecorderEvent::AudioLevels { .. }
            | RecorderEvent::AudioTestStarted
            | RecorderEvent::AudioTestFinished { .. } => {}
            RecorderEvent::SourcesListed(sources) => {
                self.sources = sources;
                if let Some((kind, id)) = self.selected
                    && !self.sources.iter().any(|s| s.kind == kind && s.id == id)
                {
                    self.selected = None;
                }
            }
            RecorderEvent::RecordingStarted { path, encoder } => {
                self.phase = Phase::Recording;
                self.path = Some(path);
                self.encoder = Some(encoder);
                self.stats = RecordingStats::default();
                self.finished = None;
                self.error = None;
            }
            RecorderEvent::Stats(stats) => self.stats = stats,
            RecorderEvent::RecordingFinished {
                path,
                stats,
                reason,
            } => {
                self.reset_run();
                self.finished = Some(Finished {
                    path,
                    stats,
                    reason,
                });
            }
            RecorderEvent::RecordingFailed { message } => {
                self.reset_run();
                self.error = Some(message);
            }
            // Non-fatal errors (e.g. a failed source refresh) never change the
            // run phase: only RecordingStarted/Finished/Failed/Stopped do.
            RecorderEvent::Error { message } => self.error = Some(message),
            // Replay outcomes surface as toasts, not recording state.
            RecorderEvent::ReplaySaving
            | RecorderEvent::ReplaySaved { .. }
            | RecorderEvent::ReplaySaveFailed { .. } => {}
            RecorderEvent::Stopped => {
                self.reset_run();
                self.error = Some("The recorder stopped. Restart Clipper to record again.".into());
            }
        }
    }

    fn reset_run(&mut self) {
        self.phase = Phase::Idle;
        self.encoder = None;
        self.path = None;
    }

    pub fn select(&mut self, kind: SourceKind, id: u64) {
        self.selected = Some((kind, id));
    }

    pub fn selected_source(&self) -> Option<&CaptureSource> {
        let (kind, id) = self.selected?;
        self.sources.iter().find(|s| s.kind == kind && s.id == id)
    }

    pub fn can_start(&self) -> bool {
        self.phase == Phase::Idle && self.selected_source().is_some()
    }

    pub fn can_stop(&self) -> bool {
        self.phase == Phase::Recording
    }

    /// Settings and source selection are locked while a run is active.
    pub fn settings_enabled(&self) -> bool {
        self.phase == Phase::Idle
    }

    pub fn begin_stop(&mut self) {
        if self.phase == Phase::Recording {
            self.phase = Phase::Stopping;
        }
    }

    pub fn begin_start(&mut self) {
        self.phase = Phase::Starting;
        self.finished = None;
        self.error = None;
    }

    pub fn fail(&mut self, message: String) {
        self.error = Some(message);
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
    }

    pub fn start_hint(&self) -> Option<&'static str> {
        match self.phase {
            Phase::Idle if self.selected_source().is_none() => Some("Select a source to record."),
            _ => None,
        }
    }

    pub fn status_label(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "Idle",
            Phase::Starting => "Starting\u{2026}",
            Phase::Recording => "Recording",
            Phase::Stopping => "Saving...",
        }
    }
}

/// Formats a duration as `mm:ss` (minutes keep growing past 59).
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

pub fn resolution_label(width: u32, height: u32) -> String {
    match (width, height) {
        (1280, 720) => "720p".into(),
        (1600, 900) => "900p".into(),
        (1920, 1080) => "1080p".into(),
        (2560, 1440) => "1440p".into(),
        (3840, 2160) => "4K".into(),
        _ => format!("{width}x{height}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(kind: SourceKind, id: u64) -> CaptureSource {
        CaptureSource {
            kind,
            id,
            name: format!("s{id}"),
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn stop_shows_stopping_until_finished_and_errors_keep_phase() {
        let mut s = RecordingState::default();
        s.begin_start();
        s.apply(RecorderEvent::Error {
            message: "refresh failed".into(),
        });
        assert_eq!(s.phase, Phase::Starting);
        s.apply(RecorderEvent::RecordingStarted {
            path: "a.mp4".into(),
            encoder: "hw".into(),
        });
        s.begin_stop();
        assert_eq!(s.phase, Phase::Stopping);
        assert!(!s.can_stop());
        assert!(!s.settings_enabled());
        s.apply(RecorderEvent::RecordingFinished {
            path: "a.mp4".into(),
            stats: RecordingStats::default(),
            reason: StopReason::User,
        });
        assert_eq!(s.phase, Phase::Idle);
    }

    #[test]
    fn selection_survives_refresh_and_clears_when_gone() {
        let mut s = RecordingState::default();
        s.apply(RecorderEvent::SourcesListed(vec![
            src(SourceKind::Monitor, 1),
            src(SourceKind::Window, 2),
        ]));
        s.select(SourceKind::Window, 2);
        s.apply(RecorderEvent::SourcesListed(vec![
            src(SourceKind::Window, 2),
            src(SourceKind::Monitor, 1),
        ]));
        assert!(s.selected_source().is_some());
        s.apply(RecorderEvent::SourcesListed(vec![src(
            SourceKind::Monitor,
            2,
        )]));
        assert_eq!(s.selected, None);
    }

    #[test]
    fn start_rules() {
        let mut s = RecordingState::default();
        assert!(!s.can_start());
        assert!(s.start_hint().is_some());
        s.apply(RecorderEvent::SourcesListed(vec![src(
            SourceKind::Monitor,
            1,
        )]));
        s.select(SourceKind::Monitor, 1);
        assert!(s.can_start() && s.settings_enabled() && !s.can_stop());
        s.begin_start();
        assert_eq!(s.phase, Phase::Starting);
        assert!(!s.can_start() && !s.can_stop() && !s.settings_enabled());
    }

    #[test]
    fn lifecycle() {
        let mut s = RecordingState::default();
        s.begin_start();
        s.apply(RecorderEvent::RecordingStarted {
            path: "a.mp4".into(),
            encoder: "NVENC".into(),
        });
        assert!(s.can_stop());
        let stats = RecordingStats {
            frames_encoded: 5,
            ..Default::default()
        };
        s.apply(RecorderEvent::Stats(stats));
        assert_eq!(s.stats.frames_encoded, 5);
        s.apply(RecorderEvent::RecordingFinished {
            path: "a.mp4".into(),
            stats,
            reason: StopReason::User,
        });
        assert_eq!(s.phase, Phase::Idle);
        assert!(s.finished.is_some() && s.encoder.is_none());
    }

    #[test]
    fn failure_and_error_recover() {
        let mut s = RecordingState::default();
        s.begin_start();
        s.apply(RecorderEvent::Error {
            message: "bad".into(),
        });
        // A non-fatal error does not end the pending start.
        assert_eq!(s.phase, Phase::Starting);
        assert_eq!(s.error.as_deref(), Some("bad"));
        s.dismiss_error();
        s.apply(RecorderEvent::RecordingFailed {
            message: "boom".into(),
        });
        assert_eq!(s.phase, Phase::Idle);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "01:05");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "60:00");
        assert_eq!(resolution_label(2560, 1440), "1440p");
        assert_eq!(resolution_label(100, 50), "100x50");
    }
    #[test]
    fn newer_failed_save_still_prevents_an_older_snapshot_from_writing() {
        let order = SettingsSaveOrder::default();
        let old = order.request();
        let new = order.request();
        assert!(order.save(new, || Err("disk unavailable".into())).is_err());
        let mut wrote_old = false;
        order
            .save(old, || {
                wrote_old = true;
                Ok(())
            })
            .unwrap();
        assert!(!wrote_old);
    }

    #[test]
    fn only_latest_requested_settings_are_written() {
        let order = SettingsSaveOrder::default();
        let old = order.request();
        let new = order.request();
        let mut saved = Vec::new();
        order
            .save(old, || {
                saved.push("old");
                Ok(())
            })
            .unwrap();
        order
            .save(new, || {
                saved.push("new");
                Ok(())
            })
            .unwrap();
        assert_eq!(saved, vec!["new"]);
    }
}
