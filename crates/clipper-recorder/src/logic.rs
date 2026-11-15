//! Platform-independent recorder decisions: file naming, stop classification,
//! counters and lifecycle validation.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

// ------------------------------------------------- failures and recovery --

/// What failed, as far as recovery is concerned (see
/// `docs/quality/capture-encoder-recovery.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The captured window or monitor is gone: needs the user (never switch
    /// to another source on their behalf).
    SourceLost,
    /// The D3D11 device was removed or reset (driver update/crash, TDR, GPU
    /// unplugged). Every GPU resource of the session is invalid; a new session
    /// on a new device may work once the driver is back.
    DeviceLost,
    /// The hardware encoder failed or stopped producing output.
    EncoderFailure,
    /// Unsupported hardware, format or settings: retrying cannot help.
    Configuration,
    /// Anything else (audio device lost, buffer error, clock gap): no retry.
    Other,
}

// HRESULTs (as u32) with a known meaning for recovery.
const DXGI_ERROR_DEVICE_REMOVED: u32 = 0x887A_0005;
const DXGI_ERROR_DEVICE_HUNG: u32 = 0x887A_0006;
const DXGI_ERROR_DEVICE_RESET: u32 = 0x887A_0007;
const DXGI_ERROR_DRIVER_INTERNAL_ERROR: u32 = 0x887A_0020;
const D3DDDIERR_DEVICEREMOVED: u32 = 0x8876_0870;
const DXGI_ERROR_UNSUPPORTED: u32 = 0x887A_0004;
const RO_E_CLOSED: u32 = 0x8000_0013;
const HRESULT_INVALID_WINDOW_HANDLE: u32 = 0x8007_0578;
const E_NOTIMPL: u32 = 0x8000_4001;
const MF_E_INVALIDMEDIATYPE: u32 = 0xC00D_36B4;
const MF_E_UNSUPPORTED_D3D_TYPE: u32 = 0xC00D_6D76;

/// Classifies an HRESULT; `None` when the code alone does not say (the
/// caller then uses the failing component's default).
pub fn classify_hresult(code: u32) -> Option<FailureKind> {
    match code {
        DXGI_ERROR_DEVICE_REMOVED
        | DXGI_ERROR_DEVICE_HUNG
        | DXGI_ERROR_DEVICE_RESET
        | DXGI_ERROR_DRIVER_INTERNAL_ERROR
        | D3DDDIERR_DEVICEREMOVED => Some(FailureKind::DeviceLost),
        RO_E_CLOSED | HRESULT_INVALID_WINDOW_HANDLE => Some(FailureKind::SourceLost),
        DXGI_ERROR_UNSUPPORTED | E_NOTIMPL | MF_E_INVALIDMEDIATYPE | MF_E_UNSUPPORTED_D3D_TYPE => {
            Some(FailureKind::Configuration)
        }
        _ => None,
    }
}

/// A classified runtime failure: `detail` is the technical text for the log,
/// [`Failure::message`] the sentence fragment for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: String,
    pub code: Option<u32>,
}

impl Failure {
    pub fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: replay_failure_text(&detail.into()),
            code: None,
        }
    }

    /// From a Windows error of a component whose failures default to
    /// `default` when the code is not specific.
    pub fn from_code(default: FailureKind, code: u32, detail: impl Into<String>) -> Self {
        Self {
            code: Some(code),
            ..Self::new(classify_hresult(code).unwrap_or(default), detail)
        }
    }

    /// Lowercase fragment for "Replay stopped: ...". GPU and encoder failures
    /// get an explanation instead of a raw HRESULT text.
    pub fn message(&self) -> String {
        let code = self
            .code
            .map(|code| format!(" (0x{code:08X})"))
            .unwrap_or_default();
        match self.kind {
            FailureKind::DeviceLost => {
                format!("the GPU device was lost (graphics driver reset or GPU removed){code}")
            }
            FailureKind::EncoderFailure => format!("the hardware video encoder failed{code}"),
            _ => self.detail.clone(),
        }
    }
}

/// Waits before automatic restart attempts: one per attempt.
const RECOVERY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
];

/// A restarted session must run this long before a new failure starts a
/// fresh budget; earlier failures count against the same one (no flapping).
pub const RECOVERY_STABLE_AFTER: Duration = Duration::from_secs(60);

/// Automatic restarts allowed per failure kind. Only failures that a new
/// session on a new device can fix are retried; the restart uses the user's
/// original source and settings, never another source.
pub fn max_recovery_attempts(kind: FailureKind) -> usize {
    match kind {
        FailureKind::DeviceLost => RECOVERY_DELAYS.len(),
        FailureKind::EncoderFailure => 1,
        FailureKind::SourceLost | FailureKind::Configuration | FailureKind::Other => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDecision {
    /// Restart attempt `attempt` of `of` is scheduled after `delay`.
    Retry {
        attempt: usize,
        of: usize,
        delay: Duration,
    },
    /// No (further) automatic restart: the user has to start again.
    GiveUp,
    /// A duplicate or stale report, or recovery is shut down: nothing changes.
    Ignored,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RecoveryState {
    #[default]
    Idle,
    Waiting {
        due: Instant,
    },
    Attempting,
}

/// Bounded automatic restart of the replay session, driven by the recorder
/// worker (single thread): at most one pending or running attempt, explicit
/// cancellation by user commands and shutdown, and a generation number that
/// makes reports about an older attempt harmless.
#[derive(Debug, Default)]
pub struct Recovery {
    state: RecoveryState,
    generation: u64,
    attempts: usize,
    recovered_at: Option<Instant>,
    shut_down: bool,
}

impl Recovery {
    /// A running session failed.
    pub fn on_failure(&mut self, kind: FailureKind, now: Instant) -> RecoveryDecision {
        if self.shut_down || self.state != RecoveryState::Idle {
            return RecoveryDecision::Ignored;
        }
        // A failure soon after an automatic restart continues that budget.
        let flapping = self
            .recovered_at
            .is_some_and(|at| now.duration_since(at) < RECOVERY_STABLE_AFTER);
        if !flapping {
            self.attempts = 0;
        }
        self.schedule(kind, now)
    }

    fn schedule(&mut self, kind: FailureKind, now: Instant) -> RecoveryDecision {
        let of = max_recovery_attempts(kind);
        if self.attempts >= of {
            self.state = RecoveryState::Idle;
            self.attempts = 0;
            self.recovered_at = None;
            return RecoveryDecision::GiveUp;
        }
        let delay = RECOVERY_DELAYS[self.attempts.min(RECOVERY_DELAYS.len() - 1)];
        self.attempts += 1;
        self.generation += 1;
        self.state = RecoveryState::Waiting { due: now + delay };
        RecoveryDecision::Retry {
            attempt: self.attempts,
            of,
            delay,
        }
    }

    /// When the worker must wake up for the pending attempt.
    pub fn next_due(&self) -> Option<Instant> {
        match self.state {
            RecoveryState::Waiting { due } => Some(due),
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.state != RecoveryState::Idle
    }

    /// Starts the pending attempt if it is due; returns its generation.
    pub fn take_due(&mut self, now: Instant) -> Option<u64> {
        match self.state {
            RecoveryState::Waiting { due } if now >= due => {
                self.state = RecoveryState::Attempting;
                Some(self.generation)
            }
            _ => None,
        }
    }

    /// The attempt `generation` started a new session. `false` for a stale
    /// report (the attempt was cancelled meanwhile): the caller must not keep
    /// the session.
    pub fn attempt_succeeded(&mut self, generation: u64, now: Instant) -> bool {
        if self.shut_down
            || generation != self.generation
            || self.state != RecoveryState::Attempting
        {
            return false;
        }
        self.state = RecoveryState::Idle;
        self.recovered_at = Some(now);
        true
    }

    /// The attempt `generation` could not start a session.
    pub fn attempt_failed(
        &mut self,
        generation: u64,
        kind: FailureKind,
        now: Instant,
    ) -> RecoveryDecision {
        if self.shut_down
            || generation != self.generation
            || self.state != RecoveryState::Attempting
        {
            return RecoveryDecision::Ignored;
        }
        self.state = RecoveryState::Idle;
        self.schedule(kind, now)
    }

    /// The user took over (stop, new start, recording): drop any pending or
    /// running attempt. Idempotent.
    pub fn cancel(&mut self) {
        self.state = RecoveryState::Idle;
        self.generation += 1;
        self.attempts = 0;
        self.recovered_at = None;
    }

    /// Application shutdown: no attempt ever again. Idempotent.
    pub fn shutdown(&mut self) {
        self.cancel();
        self.shut_down = true;
    }
}

/// The encoder must answer submitted video with output within this time.
pub const ENCODER_STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Detects a hardware encoder that silently stops producing output (without
/// an error), which would otherwise leave the replay "running" with a buffer
/// that no longer grows. Event driven: two `Option` updates per service step.
#[derive(Clone, Copy, Debug, Default)]
pub struct OutputWatchdog {
    waiting_since: Option<Instant>,
}

impl OutputWatchdog {
    /// Video is due from the encoder (frames were submitted or are waiting).
    pub fn expect_output(&mut self, now: Instant) {
        self.waiting_since.get_or_insert(now);
    }

    /// The encoder produced video output.
    pub fn output(&mut self) {
        self.waiting_since = None;
    }

    pub fn stalled(&self, now: Instant) -> bool {
        self.waiting_since
            .is_some_and(|since| now.duration_since(since) >= ENCODER_STALL_TIMEOUT)
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

/// Shortest silence after a slot's end before it is filled with a repeated
/// frame: 1/60 s, the WGC delivery latency budget.
const MIN_REPEAT_LAG_TICKS: i64 = 166_667;

/// Silence after a slot's end before a static-scene gap is filled with a
/// repeated frame, so a merely late real frame is not overtaken by a repeat
/// (which would discard it as late and duplicate the previous frame).
///
/// One frame interval, but never less than [`MIN_REPEAT_LAG_TICKS`]: WGC
/// delivers frames some milliseconds after their timestamp, so at 120 fps a
/// one-interval (8.3 ms) lag replaced about a quarter of the real frames by
/// repeats. The lag only delays the static-scene fill; audio is mixed far
/// further behind real time.
pub fn repeat_lag_ticks(interval: i64) -> i64 {
    interval.max(MIN_REPEAT_LAG_TICKS)
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
    fn repeat_lag_is_one_interval_but_at_least_a_60_fps_frame() {
        use super::repeat_lag_ticks;
        // 30 / 60 fps: unchanged, one frame interval.
        assert_eq!(repeat_lag_ticks(333_333), 333_333);
        assert_eq!(repeat_lag_ticks(166_666), 166_667);
        // 120 / 240 fps: raised to the 60 fps budget.
        assert_eq!(repeat_lag_ticks(83_333), 166_667);
        assert_eq!(repeat_lag_ticks(41_666), 166_667);
    }

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

    // ------------------------------------------------- failures and recovery --

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    /// Runs one due attempt at `now`; returns its generation.
    fn attempt(recovery: &mut Recovery, now: Instant) -> u64 {
        let due = recovery.next_due().expect("an attempt must be pending");
        assert!(now >= due, "attempt run before it was due");
        recovery.take_due(now).expect("due attempt")
    }

    #[test]
    fn hresults_are_classified_by_meaning_not_by_failure() {
        use FailureKind::*;
        for code in [
            0x887A_0005,
            0x887A_0006,
            0x887A_0007,
            0x887A_0020,
            0x8876_0870,
        ] {
            assert_eq!(classify_hresult(code), Some(DeviceLost), "{code:#X}");
        }
        assert_eq!(classify_hresult(0x8000_0013), Some(SourceLost));
        assert_eq!(classify_hresult(0x8007_0578), Some(SourceLost));
        assert_eq!(classify_hresult(0xC00D_36B4), Some(Configuration));
        // E_FAIL and E_OUTOFMEMORY say nothing about the cause.
        assert_eq!(classify_hresult(0x8000_4005), None);
        assert_eq!(classify_hresult(0x8007_000E), None);
        let lost = Failure::from_code(
            EncoderFailure,
            0x887A_0005,
            "Encoder failed: device removed",
        );
        assert_eq!(lost.kind, DeviceLost);
        assert_eq!(
            lost.message(),
            "the GPU device was lost (graphics driver reset or GPU removed) (0x887A0005)"
        );
        assert_eq!(lost.detail, "encoder failed: device removed");
        let generic = Failure::from_code(EncoderFailure, 0x8000_4005, "encoder failed: E_FAIL");
        assert_eq!(generic.kind, EncoderFailure);
        assert_eq!(
            generic.message(),
            "the hardware video encoder failed (0x80004005)"
        );
        let closed = Failure::new(SourceLost, "The capture source was closed.");
        assert_eq!(closed.message(), "the capture source was closed");
    }

    /// TEST A + G: a recoverable failure schedules exactly one restart; once it
    /// succeeds recovery is idle again.
    #[test]
    fn device_loss_is_restarted_once_after_a_delay() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        assert!(!recovery.is_pending());
        let decision = recovery.on_failure(FailureKind::DeviceLost, t0);
        assert_eq!(
            decision,
            RecoveryDecision::Retry {
                attempt: 1,
                of: 3,
                delay: secs(2)
            }
        );
        assert_eq!(
            recovery.take_due(t0 + secs(1)),
            None,
            "not before the delay"
        );
        let generation = attempt(&mut recovery, t0 + secs(2));
        assert_eq!(
            recovery.next_due(),
            None,
            "no second attempt while one runs"
        );
        assert!(recovery.attempt_succeeded(generation, t0 + secs(3)));
        assert!(!recovery.is_pending());
    }

    /// TEST B + D: source loss and configuration errors are never retried.
    #[test]
    fn source_loss_and_configuration_errors_need_the_user() {
        let t0 = Instant::now();
        for kind in [
            FailureKind::SourceLost,
            FailureKind::Configuration,
            FailureKind::Other,
        ] {
            let mut recovery = Recovery::default();
            assert_eq!(
                recovery.on_failure(kind, t0),
                RecoveryDecision::GiveUp,
                "{kind:?}"
            );
            assert!(!recovery.is_pending());
        }
    }

    /// TEST C: a restart after device loss whose new device cannot be created
    /// yet (driver still resetting) is retried with growing delays, and the
    /// source vanishing meanwhile ends recovery at once.
    #[test]
    fn device_loss_backs_off_and_stops_when_the_source_is_gone() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::DeviceLost, t0);
        let generation = attempt(&mut recovery, t0 + secs(2));
        assert_eq!(
            recovery.attempt_failed(generation, FailureKind::DeviceLost, t0 + secs(2)),
            RecoveryDecision::Retry {
                attempt: 2,
                of: 3,
                delay: secs(5)
            }
        );
        let generation = attempt(&mut recovery, t0 + secs(7));
        assert_eq!(
            recovery.attempt_failed(generation, FailureKind::SourceLost, t0 + secs(7)),
            RecoveryDecision::GiveUp
        );
        assert!(!recovery.is_pending());
    }

    /// TEST H: repeated failures end in GiveUp after the bounded attempts.
    #[test]
    fn retries_are_bounded() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        let mut now = t0;
        let mut decision = recovery.on_failure(FailureKind::DeviceLost, now);
        let mut attempts = 0;
        while let RecoveryDecision::Retry { delay, .. } = decision {
            attempts += 1;
            now += delay;
            let generation = attempt(&mut recovery, now);
            decision = recovery.attempt_failed(generation, FailureKind::DeviceLost, now);
        }
        assert_eq!(decision, RecoveryDecision::GiveUp);
        assert_eq!(attempts, 3);
        assert_eq!(now - t0, secs(2 + 5 + 10));
        // Encoder failures get a single restart.
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::EncoderFailure, t0);
        let generation = attempt(&mut recovery, t0 + secs(2));
        assert_eq!(
            recovery.attempt_failed(generation, FailureKind::EncoderFailure, t0 + secs(2)),
            RecoveryDecision::GiveUp
        );
    }

    /// A restarted session that fails again before it ran stably counts
    /// against the same budget: no endless restart loop.
    #[test]
    fn flapping_sessions_cannot_restart_forever() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        let mut now = t0;
        let mut restarts = 0;
        loop {
            match recovery.on_failure(FailureKind::DeviceLost, now) {
                RecoveryDecision::Retry { delay, .. } => {
                    now += delay;
                    let generation = attempt(&mut recovery, now);
                    assert!(recovery.attempt_succeeded(generation, now));
                    restarts += 1;
                    now += secs(10); // fails again 10 s after each restart
                }
                RecoveryDecision::GiveUp => break,
                RecoveryDecision::Ignored => panic!("unexpected"),
            }
            assert!(restarts <= 3);
        }
        assert_eq!(restarts, 3);
        // A session that ran stably gets a fresh budget.
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::DeviceLost, t0);
        let generation = attempt(&mut recovery, t0 + secs(2));
        recovery.attempt_succeeded(generation, t0 + secs(2));
        assert!(matches!(
            recovery.on_failure(
                FailureKind::DeviceLost,
                t0 + secs(2) + RECOVERY_STABLE_AFTER
            ),
            RecoveryDecision::Retry { attempt: 1, .. }
        ));
    }

    /// TEST I + Q + R: a user stop cancels the pending attempt; cancel is
    /// idempotent; a later explicit start and failure start a fresh budget.
    #[test]
    fn user_stop_cancels_and_a_new_start_is_clean() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::DeviceLost, t0);
        recovery.cancel();
        recovery.cancel();
        assert_eq!(
            recovery.take_due(t0 + secs(60)),
            None,
            "cancelled attempt ran"
        );
        assert!(!recovery.is_pending());
        assert!(matches!(
            recovery.on_failure(FailureKind::DeviceLost, t0 + secs(61)),
            RecoveryDecision::Retry { attempt: 1, .. }
        ));
    }

    /// TEST I (running attempt) + L: an attempt cancelled while it ran reports
    /// back stale; its session must not be kept, and a newer attempt's
    /// generation is not confused with it.
    #[test]
    fn stale_attempt_reports_are_ignored() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::DeviceLost, t0);
        let old = attempt(&mut recovery, t0 + secs(2));
        recovery.cancel();
        assert!(
            !recovery.attempt_succeeded(old, t0 + secs(3)),
            "stale success kept"
        );
        assert_eq!(
            recovery.attempt_failed(old, FailureKind::DeviceLost, t0 + secs(3)),
            RecoveryDecision::Ignored
        );
        recovery.on_failure(FailureKind::DeviceLost, t0 + secs(4));
        let new = attempt(&mut recovery, t0 + secs(6));
        assert_ne!(old, new);
        assert!(!recovery.attempt_succeeded(old, t0 + secs(6)));
        assert!(recovery.attempt_succeeded(new, t0 + secs(6)));
    }

    /// TEST J + R: after shutdown nothing is ever scheduled or kept.
    #[test]
    fn shutdown_ends_recovery_for_good() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        recovery.on_failure(FailureKind::DeviceLost, t0);
        let generation = attempt(&mut recovery, t0 + secs(2));
        recovery.shutdown();
        recovery.shutdown();
        assert!(!recovery.attempt_succeeded(generation, t0 + secs(3)));
        assert_eq!(
            recovery.on_failure(FailureKind::DeviceLost, t0 + secs(4)),
            RecoveryDecision::Ignored
        );
        assert_eq!(recovery.next_due(), None);
    }

    /// TEST K: the same failure reported twice (pump error and source-closed
    /// signal in one step) schedules one restart only.
    #[test]
    fn duplicate_failures_start_one_recovery() {
        let t0 = Instant::now();
        let mut recovery = Recovery::default();
        assert!(matches!(
            recovery.on_failure(FailureKind::DeviceLost, t0),
            RecoveryDecision::Retry { attempt: 1, .. }
        ));
        assert_eq!(
            recovery.on_failure(FailureKind::DeviceLost, t0),
            RecoveryDecision::Ignored
        );
        assert_eq!(
            recovery.on_failure(FailureKind::EncoderFailure, t0 + secs(1)),
            RecoveryDecision::Ignored
        );
        assert_eq!(recovery.next_due(), Some(t0 + secs(2)));
    }

    /// TEST E: an encoder that stops answering is detected after the timeout;
    /// normal latency and a quiet (not yet started) encoder are not.
    #[test]
    fn watchdog_detects_a_silent_encoder() {
        let t0 = Instant::now();
        let mut watchdog = OutputWatchdog::default();
        assert!(!watchdog.stalled(t0 + secs(3600)), "nothing expected yet");
        for step in 0..1000u32 {
            let now = t0 + Duration::from_millis(u64::from(step) * 16);
            watchdog.expect_output(now);
            if step % 3 == 2 {
                watchdog.output(); // ~50 ms encoder latency
            }
            assert!(!watchdog.stalled(now));
        }
        let quiet = t0 + secs(20);
        watchdog.output();
        watchdog.expect_output(quiet);
        watchdog.expect_output(quiet + secs(4));
        assert!(!watchdog.stalled(quiet + secs(4)));
        assert!(watchdog.stalled(quiet + ENCODER_STALL_TIMEOUT));
    }
}
