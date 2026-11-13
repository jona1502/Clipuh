//! Platform-independent recorder decisions: file naming, stop classification,
//! counters and lifecycle validation.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use clipper_core::recording::RecordingStats;
use clipper_core::{Command, Core, CoreError, Event, State};

/// First name from `base`, `<stem>_2.<ext>`, `<stem>_3.<ext>`... for which
/// `exists` is false.
pub fn unique_file_name(base: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(base) {
        return base.to_owned();
    }
    let (stem, ext) = match base.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (base, String::new()),
    };
    for n in 2..=u32::MAX {
        let candidate = format!("{stem}_{n}{ext}");
        if !exists(&candidate) {
            return candidate;
        }
    }
    base.to_owned()
}

/// `<final>.partial`; renamed to `final` only after a successful finish.
pub fn partial_path(final_path: &Path) -> PathBuf {
    let mut name = final_path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// Encoded size for a source of `source` pixels and a configured `target`
/// size: preserves the source aspect ratio (no stretching), never upscales,
/// and keeps both dimensions even as H.264/NV12 requires.
pub fn fit_output_size(source: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (u64::from(source.0.max(2)), u64::from(source.1.max(2)));
    let (tw, th) = (u64::from(target.0.max(2)), u64::from(target.1.max(2)));
    // scale = min(tw/sw, th/sh, 1) in exact integer arithmetic.
    let (w, h) = if sw <= tw && sh <= th {
        (sw, sh)
    } else if tw * sh <= th * sw {
        (tw, sh * tw / sw)
    } else {
        (sw * th / sh, th)
    };
    let even = |value: u64| (value.max(2) as u32) & !1;
    (even(w), even(h))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    Success,
    Failure(String),
}

/// A file is only reported as successful when the encoder finalized it and at
/// least one frame was encoded.
pub fn classify_stop(frames_encoded: u64, finish: Result<(), String>) -> StopOutcome {
    match finish {
        Err(message) => StopOutcome::Failure(message),
        Ok(()) if frames_encoded == 0 => StopOutcome::Failure("no frames were captured".into()),
        Ok(()) => StopOutcome::Success,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub captured: u64,
    pub encoded: u64,
    pub dropped: u64,
}

impl Counters {
    pub fn snapshot(&self, elapsed: Duration, current_fps: f32) -> RecordingStats {
        RecordingStats {
            elapsed,
            frames_captured: self.captured,
            frames_encoded: self.encoded,
            frames_dropped: self.dropped,
            current_fps,
        }
    }
}

/// Validates recording commands with the shared lifecycle state machine so
/// invalid requests become errors instead of undefined behavior.
#[derive(Debug, Default)]
pub struct Lifecycle {
    core: Core,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn is_recording(&self) -> bool {
        self.core.state() == State::Recording
    }

    pub fn begin_start(&mut self) -> Result<(), String> {
        self.core
            .handle_command(Command::StartRecording)
            .map(|_| ())
            .map_err(describe)
    }

    pub fn started(&mut self) {
        self.complete(Event::RecordingStarted);
    }

    /// Start failed: back to a usable idle state.
    pub fn start_failed(&mut self, message: &str) {
        self.complete(Event::BackendFailed {
            operation: Command::StartRecording,
            message: message.to_owned(),
        });
        self.core = Core::new();
    }

    pub fn begin_stop(&mut self) -> Result<(), String> {
        self.core
            .handle_command(Command::StopRecording)
            .map(|_| ())
            .map_err(describe)
    }

    /// Recording ended (successfully or not); worker is idle again.
    pub fn stopped(&mut self) {
        self.complete(Event::RecordingStopped);
        self.core = Core::new();
    }

    fn complete(&mut self, event: Event) {
        // Events are only produced right after the matching command, so a
        // rejection means a worker bug; resetting keeps the worker usable.
        if self.core.handle_event(event).is_err() {
            self.core = Core::new();
        }
    }
}

fn describe(error: CoreError) -> String {
    match error {
        CoreError::InvalidCommand {
            command: Command::StartRecording,
            ..
        } => "a recording is already in progress".into(),
        CoreError::InvalidCommand {
            command: Command::StopRecording,
            state: State::Idle,
        } => "no recording is in progress".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn output_keeps_aspect_and_never_upscales() {
        use super::fit_output_size;
        // Same aspect: exact target.
        assert_eq!(fit_output_size((2560, 1440), (1920, 1080)), (1920, 1080));
        // Ultrawide monitor into a 16:9 preset: width-limited, no stretch.
        assert_eq!(fit_output_size((3440, 1440), (1920, 1080)), (1920, 802));
        // Tall window: height-limited.
        assert_eq!(fit_output_size((1000, 2000), (1920, 1080)), (540, 1080));
        // Small window: kept at source size, rounded to even.
        assert_eq!(fit_output_size((801, 601), (1920, 1080)), (800, 600));
        // Degenerate input never yields odd or zero sizes.
        assert_eq!(fit_output_size((0, 0), (1920, 1080)), (2, 2));
    }

    use super::*;

    #[test]
    fn unique_name_appends_counter_before_extension() {
        let taken = ["a.mp4", "a_2.mp4"];
        assert_eq!(unique_file_name("a.mp4", |n| taken.contains(&n)), "a_3.mp4");
        assert_eq!(unique_file_name("b.mp4", |n| taken.contains(&n)), "b.mp4");
        assert_eq!(unique_file_name("c", |n| n == "c"), "c_2");
    }

    #[test]
    fn partial_path_appends_suffix() {
        assert_eq!(
            partial_path(Path::new("out/Clipper_x.mp4")),
            PathBuf::from("out/Clipper_x.mp4.partial")
        );
    }

    #[test]
    fn zero_frames_or_failed_finish_is_failure() {
        assert_eq!(classify_stop(10, Ok(())), StopOutcome::Success);
        assert_eq!(
            classify_stop(0, Ok(())),
            StopOutcome::Failure("no frames were captured".into())
        );
        assert_eq!(
            classify_stop(10, Err("disk".into())),
            StopOutcome::Failure("disk".into())
        );
    }

    #[test]
    fn counters_snapshot() {
        let counters = Counters {
            captured: 5,
            encoded: 4,
            dropped: 1,
        };
        let stats = counters.snapshot(Duration::from_secs(2), 30.0);
        assert_eq!(
            (
                stats.frames_captured,
                stats.frames_encoded,
                stats.frames_dropped
            ),
            (5, 4, 1)
        );
        assert_eq!(stats.elapsed, Duration::from_secs(2));
    }

    #[test]
    fn lifecycle_rejects_invalid_commands_and_supports_cycles() {
        let mut life = Lifecycle::new();
        assert_eq!(life.begin_stop(), Err("no recording is in progress".into()));
        for _ in 0..3 {
            life.begin_start().unwrap();
            life.started();
            assert!(life.is_recording());
            assert_eq!(
                life.begin_start(),
                Err("a recording is already in progress".into())
            );
            life.begin_stop().unwrap();
            life.stopped();
            assert!(!life.is_recording());
        }
    }

    #[test]
    fn failed_start_leaves_lifecycle_usable() {
        let mut life = Lifecycle::new();
        life.begin_start().unwrap();
        life.start_failed("boom");
        assert!(!life.is_recording());
        life.begin_start().unwrap();
        life.started();
        assert!(life.is_recording());
    }
}
