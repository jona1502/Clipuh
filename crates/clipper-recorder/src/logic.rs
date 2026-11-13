//! Platform-independent recorder decisions: file naming, stop classification,
//! counters and lifecycle validation.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clipper_core::recording::{RecordingStats, ReplayOptions, ReplayStatus, ReplayTier};
use clipper_core::{Command, Core, CoreError, Event, State};
use clipper_replay::hybrid::{BufferStats, BufferTier};

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

/// `Clipper_Replay_YYYY-MM-DD_HH-MM-SS.mp4` from local time components.
pub fn replay_file_name(
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
) -> String {
    format!("Clipper_Replay_{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}.mp4")
}

/// File names of clips that are queued or being exported. Two saves in the
/// same second must not pick the same name just because the first file does
/// not exist yet.
#[derive(Clone, Debug, Default)]
pub struct ClipNames {
    reserved: Arc<Mutex<HashSet<String>>>,
}

impl ClipNames {
    /// Picks the first name that is neither reserved nor `on_disk`, and
    /// reserves it until [`ClipNames::release`].
    pub fn reserve(&self, base: &str, on_disk: impl Fn(&str) -> bool) -> String {
        let mut reserved = self.reserved.lock().unwrap_or_else(PoisonError::into_inner);
        let name = unique_file_name(base, |candidate| {
            reserved.contains(candidate) || on_disk(candidate)
        });
        reserved.insert(name.clone());
        name
    }

    pub fn release(&self, name: &str) {
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Longest replay the worker accepts (the UI offers up to 5 minutes).
pub const MAX_REPLAY_DURATION: Duration = Duration::from_secs(3600);

pub fn valid_replay_duration(duration: Duration) -> bool {
    !duration.is_zero() && duration <= MAX_REPLAY_DURATION
}

/// Rejects replay options the pipeline cannot honour.
pub fn validate_replay_options(options: &ReplayOptions) -> Result<(), String> {
    if !valid_replay_duration(options.duration) {
        return Err("the replay length is invalid".into());
    }
    let recording = &options.recording;
    if recording.fps == 0 || recording.width == 0 || recording.height == 0 {
        return Err("the video settings are invalid".into());
    }
    if options.memory_limit_bytes == 0 {
        return Err("the replay memory limit is invalid".into());
    }
    Ok(())
}

/// Turns an internal message into the lowercase sentence fragment the UI
/// prefixes with "Replay stopped:".
pub fn replay_failure_text(message: &str) -> String {
    if message.contains("audio device is unavailable") {
        return "the selected audio device is unavailable".into();
    }
    let trimmed = message.trim().trim_end_matches('.');
    let mut chars = trimmed.chars();
    match chars.next() {
        // Keep acronyms such as "WASAPI" intact.
        Some(first) if !chars.clone().next().is_some_and(char::is_uppercase) => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => trimmed.to_owned(),
    }
}

pub fn replay_tier(tier: BufferTier) -> ReplayTier {
    match tier {
        BufferTier::Ram => ReplayTier::Ram,
        BufferTier::Hybrid => ReplayTier::Hybrid,
        BufferTier::RamOnlyDegraded => ReplayTier::RamOnlyDegraded,
    }
}

/// UI status from buffer statistics; `buffer` is `None` before the first
/// encoded packet defined the layout.
pub fn replay_status(
    buffer: Option<&BufferStats>,
    configured: Duration,
    exports_in_flight: usize,
    stats: RecordingStats,
) -> ReplayStatus {
    let Some(buffer) = buffer else {
        return ReplayStatus {
            configured,
            exports_in_flight,
            stats,
            ..ReplayStatus::default()
        };
    };
    ReplayStatus {
        buffered: buffer.buffered,
        configured,
        ram_bytes: buffer.ram_bytes as u64,
        disk_bytes: buffer.disk_bytes,
        tier: replay_tier(buffer.tier),
        exports_in_flight,
        stats,
        warning: buffer.disk_error.clone(),
    }
}

/// Constant frame rate grid of the replay video: slot `n` covers
/// `[n * interval, (n + 1) * interval)` on the session timeline and holds
/// exactly one frame, real or repeated.
#[derive(Clone, Copy, Debug)]
pub struct SlotTimeline {
    interval: i64,
    next: i64,
    started: bool,
}

impl SlotTimeline {
    pub fn new(interval: i64) -> Self {
        Self {
            interval: interval.max(1),
            next: 0,
            started: false,
        }
    }

    /// Nearest slot of a frame timestamp.
    pub fn slot_of(&self, sample_time: i64) -> i64 {
        (sample_time + self.interval / 2).div_euclid(self.interval)
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// First slot that is still empty.
    pub fn next(&self) -> i64 {
        self.next
    }

    pub fn time_of(&self, slot: i64) -> i64 {
        slot * self.interval
    }

    /// End of the newest submitted slot (0 before the first one).
    pub fn end_ticks(&self) -> i64 {
        self.next * self.interval
    }

    /// A real frame whose slot is already taken (or passed) must be skipped.
    pub fn is_late(&self, slot: i64) -> bool {
        self.started && slot < self.next
    }

    /// Starts the grid at `slot`; no-op once started.
    pub fn begin(&mut self, slot: i64) {
        if !self.started {
            self.started = true;
            self.next = slot.max(0);
        }
    }

    /// The slot `next` was filled.
    pub fn advance(&mut self) {
        self.next += 1;
    }
}

/// Status warning text of a replay session (`None` when all is well).
pub fn replay_warning(
    disk: Option<&str>,
    waiting_for_video: bool,
    audio_glitch_frames: u64,
    invalid_packets: u64,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(disk) = disk {
        parts.push(disk.to_owned());
    }
    if waiting_for_video {
        parts.push("waiting for the first video frame".into());
    }
    if audio_glitch_frames > 0 {
        parts.push(format!("audio glitch: {audio_glitch_frames} frames lost"));
    }
    if invalid_packets > 0 {
        parts.push(format!("dropped {invalid_packets} invalid packets"));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// Deletes `Clipper_Replay_*.mp4.partial` files in `directory` last modified
/// more than `max_age` before `now`. Never touches any other file. Returns the
/// number of removed files.
pub fn sweep_stale_partials(
    directory: &Path,
    now: std::time::SystemTime,
    max_age: Duration,
) -> usize {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("Clipper_Replay_") && name.ends_with(".mp4.partial")) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
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
    fn replay_names_are_unique_across_reservations_and_disk() {
        let names = ClipNames::default();
        let base = replay_file_name(2026, 1, 2, 3, 4, 5);
        assert_eq!(base, "Clipper_Replay_2026-01-02_03-04-05.mp4");
        let a = names.reserve(&base, |_| false);
        let b = names.reserve(&base, |_| false);
        let c = names.reserve(&base, |n| n.ends_with("_3.mp4"));
        assert_eq!(a, base);
        assert_eq!(b, "Clipper_Replay_2026-01-02_03-04-05_2.mp4");
        assert_eq!(c, "Clipper_Replay_2026-01-02_03-04-05_4.mp4");
        assert_eq!(names.len(), 3);
        names.release(&b);
        assert_eq!(names.reserve(&base, |_| false), b);
    }

    #[test]
    fn replay_option_validation() {
        use clipper_core::recording::{QualityProfile, RecordingAudioOptions, RecordingOptions};
        let mut options = ReplayOptions {
            recording: RecordingOptions {
                audio: RecordingAudioOptions::default(),
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_mbps: 16,
                profile: QualityProfile::Balanced,
                output_directory: PathBuf::from("out"),
            },
            duration: Duration::from_secs(30),
            memory_limit_bytes: 64 << 20,
            cache_directory: None,
            disk_limit_bytes: 0,
        };
        assert!(validate_replay_options(&options).is_ok());
        options.duration = Duration::ZERO;
        assert!(validate_replay_options(&options).is_err());
        options.duration = Duration::from_secs(30);
        options.recording.fps = 0;
        assert!(validate_replay_options(&options).is_err());
        options.recording.fps = 60;
        options.memory_limit_bytes = 0;
        assert!(validate_replay_options(&options).is_err());
        assert!(valid_replay_duration(Duration::from_secs(300)));
        assert!(!valid_replay_duration(
            MAX_REPLAY_DURATION + Duration::from_secs(1)
        ));
    }

    #[test]
    fn failure_text_is_a_lowercase_fragment() {
        assert_eq!(
            replay_failure_text("Selected Render audio device is unavailable. Select one."),
            "the selected audio device is unavailable"
        );
        assert_eq!(
            replay_failure_text("An audio source failed; recording is incomplete."),
            "an audio source failed; recording is incomplete"
        );
        assert_eq!(replay_failure_text("WASAPI died."), "WASAPI died");
        assert_eq!(replay_failure_text(""), "");
    }

    #[test]
    fn status_maps_buffer_stats() {
        let buffer = BufferStats {
            buffered: Duration::from_secs(9),
            ram_bytes: 10,
            disk_bytes: 20,
            tier: BufferTier::Hybrid,
            disk_error: Some("x".into()),
            ..BufferStats::default()
        };
        let status = replay_status(
            Some(&buffer),
            Duration::from_secs(30),
            2,
            RecordingStats::default(),
        );
        assert_eq!(status.buffered, Duration::from_secs(9));
        assert_eq!((status.ram_bytes, status.disk_bytes), (10, 20));
        assert_eq!(status.tier, ReplayTier::Hybrid);
        assert_eq!(status.exports_in_flight, 2);
        assert_eq!(status.warning.as_deref(), Some("x"));
        let empty = replay_status(None, Duration::from_secs(30), 0, RecordingStats::default());
        assert_eq!(empty.buffered, Duration::ZERO);
        assert_eq!(empty.configured, Duration::from_secs(30));
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
    fn slots_round_to_the_nearest_frame_and_reject_late_frames() {
        let mut timeline = SlotTimeline::new(166_666);
        assert_eq!(timeline.slot_of(0), 0);
        assert_eq!(timeline.slot_of(83_332), 0);
        assert_eq!(timeline.slot_of(83_334), 1);
        assert_eq!(timeline.slot_of(166_666 * 5 + 1000), 5);
        assert!(!timeline.is_late(0), "nothing is late before the start");
        timeline.begin(3);
        timeline.begin(9);
        assert_eq!(timeline.next(), 3);
        assert_eq!(timeline.end_ticks(), 3 * 166_666);
        assert!(timeline.is_late(2));
        assert!(!timeline.is_late(3));
        timeline.advance();
        assert!(timeline.is_late(3));
        assert_eq!(timeline.time_of(7), 7 * 166_666);
    }

    #[test]
    fn warning_lists_every_problem() {
        assert_eq!(replay_warning(None, false, 0, 0), None);
        assert_eq!(
            replay_warning(Some("disk"), true, 480, 2).as_deref(),
            Some(
                "disk; waiting for the first video frame; audio glitch: 480 frames lost; dropped 2 invalid packets"
            )
        );
    }

    #[test]
    fn sweep_removes_only_old_replay_partials() {
        let dir = std::env::temp_dir().join(format!("clipper-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Clipper_Replay_2026-01-01_10-00-00.mp4.partial",
            "Clipper_Replay_2026-01-01_10-00-00.mp4",
            "Clipper_2026-01-01_10-00-00.mp4.partial",
            "other.partial",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let hour = Duration::from_secs(3600);
        // Fresh files survive.
        assert_eq!(
            sweep_stale_partials(&dir, std::time::SystemTime::now(), hour),
            0
        );
        // Two hours later the replay partial is stale, nothing else is.
        let later = std::time::SystemTime::now() + 2 * hour;
        assert_eq!(sweep_stale_partials(&dir, later, hour), 1);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "Clipper_2026-01-01_10-00-00.mp4.partial",
                "Clipper_Replay_2026-01-01_10-00-00.mp4",
                "other.partial"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
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
