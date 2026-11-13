//! GPUI-free view state for the recording panel.

use std::path::PathBuf;
use std::time::Duration;

use clipper_core::recording::{
    CaptureSource, RecorderEvent, RecordingStats, SourceKind, StopReason,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
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
            RecorderEvent::Error { message } => {
                if self.phase == Phase::Starting {
                    self.phase = Phase::Idle;
                }
                self.error = Some(message);
            }
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
        assert_eq!(s.phase, Phase::Idle);
        assert_eq!(s.error.as_deref(), Some("bad"));
        s.dismiss_error();
        s.begin_start();
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
}
