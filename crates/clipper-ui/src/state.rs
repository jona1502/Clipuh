//! GPUI-free view state for the recording panel.

use std::path::PathBuf;
use std::time::Duration;

use clipper_core::recording::{
    CaptureSource, RecorderEvent, RecordingStats, ReplayStatus, SourceKind, StopReason,
};
use clipper_storage::{AppConfig, config::VideoCodec, storage_projection::StorageProjection};

/// Most saved replay clips kept in the session list.
pub const MAX_SAVED_CLIPS: usize = 200;

/// Orders background saves by UI intent, including when a newer save fails.
#[derive(Debug, Default)]
pub struct SettingsSaveOrder {
    requested: std::sync::atomic::AtomicU64,
    writer: std::sync::Mutex<()>,
}

/// Only these settings affect the cached storage projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StorageProjectionKey {
    width: u32,
    height: u32,
    fps: u32,
    codec: VideoCodec,
    quality: clipper_storage::config::QualityPreset,
    bitrate_mbps: u32,
    /// Audio tracks the session is planned to encode on this Windows version; the only audio input.
    audio_tracks: u32,
    replay_duration_seconds: u32,
    replay_memory_limit_megabytes: u32,
}

impl From<&AppConfig> for StorageProjectionKey {
    fn from(config: &AppConfig) -> Self {
        Self {
            width: config.video.width,
            height: config.video.height,
            fps: config.video.fps,
            codec: config.video.codec,
            quality: config.recording.quality,
            bitrate_mbps: config.video.bitrate_mbps,
            audio_tracks: clipper_recorder::planned_audio_tracks(&config.audio.recording_options()),
            replay_duration_seconds: config.replay.duration_seconds,
            replay_memory_limit_megabytes: config.replay.memory_limit_megabytes,
        }
    }
}

/// Cached estimate; config changes that do not affect its key leave it intact.
#[derive(Debug, Default)]
pub(crate) struct StorageProjectionCache {
    key: Option<StorageProjectionKey>,
    result: Option<Result<StorageProjection, String>>,
}

impl StorageProjectionCache {
    /// Recomputes only when an input to the projection changes.
    pub(crate) fn refresh(&mut self, config: &AppConfig) -> bool {
        let key = StorageProjectionKey::from(config);
        self.refresh_with_key(key, || {
            config
                .storage_projection()
                .map_err(|error| error.to_string())
        })
    }

    fn refresh_with_key(
        &mut self,
        key: StorageProjectionKey,
        calculate: impl FnOnce() -> Result<StorageProjection, String>,
    ) -> bool {
        if self.key == Some(key) {
            return false;
        }
        self.key = Some(key);
        self.result = Some(calculate());
        true
    }

    pub(crate) fn projection(&self) -> Option<StorageProjection> {
        self.result.as_ref()?.as_ref().ok().copied()
    }

    pub(crate) fn snapshot(&self) -> Option<StorageProjectionSnapshot> {
        Some(StorageProjectionSnapshot {
            key: self.key?,
            projection: self.projection()?,
        })
    }

    pub(crate) fn key(&self) -> Option<StorageProjectionKey> {
        self.key
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.result.as_ref()?.as_ref().err().map(String::as_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StorageProjectionSnapshot {
    key: StorageProjectionKey,
    projection: StorageProjection,
}

impl StorageProjectionSnapshot {
    pub(crate) fn projection(self) -> StorageProjection {
        self.projection
    }

    pub(crate) fn matches_key(self, key: Option<StorageProjectionKey>) -> bool {
        key == Some(self.key)
    }
}

/// Accepted recording settings remain visible as the active estimate until the run ends.
#[derive(Debug, Default)]
pub(crate) struct StorageProjectionRun {
    pending: Option<StorageProjectionSnapshot>,
    active: Option<StorageProjectionSnapshot>,
}

impl StorageProjectionRun {
    pub(crate) fn start_accepted(&mut self, snapshot: Option<StorageProjectionSnapshot>) {
        self.pending = snapshot;
        self.active = None;
    }

    pub(crate) fn recording_started(&mut self) {
        self.active = self.pending.take();
    }

    pub(crate) fn apply_event(&mut self, event: &RecorderEvent) {
        match event {
            RecorderEvent::RecordingStarted { .. } => self.recording_started(),
            RecorderEvent::RecordingFinished { .. }
            | RecorderEvent::RecordingFailed { .. }
            | RecorderEvent::Stopped => self.clear(),
            _ => {}
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pending = None;
        self.active = None;
    }

    pub(crate) fn pending(&self) -> Option<StorageProjectionSnapshot> {
        self.pending
    }

    pub(crate) fn active(&self) -> Option<StorageProjectionSnapshot> {
        self.active
    }

    pub(crate) fn displayed(&self) -> Option<StorageProjectionSnapshot> {
        self.active.or(self.pending)
    }
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

/// Instant replay buffer lifecycle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ReplayPhase {
    #[default]
    Off,
    Starting,
    Running {
        encoder: String,
    },
    /// Stop was requested; running exports still finish.
    Stopping,
}

#[derive(Debug, Default)]
pub struct ReplayState {
    pub phase: ReplayPhase,
    /// Latest status while running; cleared when the buffer stops.
    pub status: Option<ReplayStatus>,
    /// Why the replay stopped or could not start, in plain words.
    pub error: Option<String>,
}

impl ReplayState {
    pub fn is_off(&self) -> bool {
        self.phase == ReplayPhase::Off
    }

    pub fn is_running(&self) -> bool {
        matches!(self.phase, ReplayPhase::Running { .. })
    }

    pub fn begin_start(&mut self) {
        self.phase = ReplayPhase::Starting;
        self.status = None;
        self.error = None;
    }

    pub fn begin_stop(&mut self) {
        if self.is_running() {
            self.phase = ReplayPhase::Stopping;
        }
    }

    fn reset(&mut self) {
        self.phase = ReplayPhase::Off;
        self.status = None;
    }
}

/// A replay clip saved during this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedClip {
    pub path: PathBuf,
    pub duration: Duration,
}

#[cfg(test)]
impl SavedClip {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Turns a worker-provided stop reason into a sentence for the user.
pub fn replay_stop_message(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "Replay stopped unexpectedly.".into();
    }
    let mut message = if raw.starts_with("Replay") {
        raw.to_owned()
    } else {
        let mut chars = raw.chars();
        let first = chars.next().map(|c| c.to_lowercase().to_string());
        format!(
            "Replay stopped: {}{}",
            first.unwrap_or_default(),
            chars.as_str()
        )
    };
    if !message.ends_with(['.', '!', '?']) {
        message.push('.');
    }
    message
}

#[derive(Debug, Default)]
pub struct RecordingState {
    pub replay: ReplayState,
    /// Replay clips saved this session, newest first (bounded).
    pub saved_clips: Vec<SavedClip>,
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
            // Mixer meters and source status live in `ClipperWindow`, not here.
            RecorderEvent::AudioLevels(_)
            | RecorderEvent::AudioSources(_)
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
            // A command rejected while replay is starting means it never started.
            RecorderEvent::Error { message } => {
                if self.replay.phase == ReplayPhase::Starting {
                    self.replay.reset();
                    self.replay.error = Some(replay_stop_message(&message));
                } else {
                    self.error = Some(message);
                }
            }
            RecorderEvent::ReplayStarted { encoder } => {
                self.replay.phase = ReplayPhase::Running { encoder };
                self.replay.status = None;
                self.replay.error = None;
            }
            // A late status after the buffer stopped must not revive it.
            RecorderEvent::ReplayStatus(status) => {
                if self.replay.is_running() {
                    self.replay.status = Some(status);
                }
            }
            RecorderEvent::ReplayStopped { error } => {
                self.replay.reset();
                if let Some(error) = error {
                    self.replay.error = Some(replay_stop_message(&error));
                }
            }
            // Save progress and failures surface as toasts.
            RecorderEvent::ReplaySaving | RecorderEvent::ReplaySaveFailed { .. } => {}
            RecorderEvent::ReplaySaved { path, duration } => {
                self.saved_clips.retain(|clip| clip.path != path);
                self.saved_clips.insert(0, SavedClip { path, duration });
                self.saved_clips.truncate(MAX_SAVED_CLIPS);
            }
            RecorderEvent::Stopped => {
                self.replay.reset();
                self.reset_run();
                self.error = Some("The recorder stopped. Restart clipuh to record again; the log files have the details.".into());
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
        self.phase == Phase::Idle && self.replay.is_off() && self.selected_source().is_some()
    }

    /// Replay and manual recording share one capture session.
    pub fn can_start_replay(&self) -> bool {
        self.can_start()
    }

    pub fn can_stop(&self) -> bool {
        self.phase == Phase::Recording
    }

    /// Settings and source selection are locked while a run is active.
    pub fn settings_enabled(&self) -> bool {
        self.phase == Phase::Idle && self.replay.is_off()
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
        if !self.replay.is_off() && self.phase == Phase::Idle {
            return Some("Turn off instant replay to record manually.");
        }
        match self.phase {
            Phase::Idle if self.selected_source().is_none() => Some("Select a source to record."),
            _ => None,
        }
    }

    pub fn replay_hint(&self) -> Option<&'static str> {
        if self.phase != Phase::Idle {
            return Some("Stop the recording to use instant replay.");
        }
        if self.replay.is_off() && self.selected_source().is_none() {
            return Some("Select a source on the Capture page to start instant replay.");
        }
        None
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
        assert_eq!(resolution_label(1600, 900), "900p");
        assert_eq!(resolution_label(100, 50), "100x50");
    }

    #[test]
    fn storage_projection_cache_ignores_unrelated_config_and_recomputes_for_inputs() {
        let mut config = AppConfig::default();
        let mut cache = StorageProjectionCache::default();
        let projection = config.storage_projection().unwrap();
        let mut calculations = 0;
        let key = StorageProjectionKey::from(&config);
        assert!(cache.refresh_with_key(key, || {
            calculations += 1;
            Ok(projection)
        }));

        config.audio.sources[1].gain_percent = 50;
        config.notifications.enabled = false;
        assert_eq!(StorageProjectionKey::from(&config), key);
        assert!(
            !cache.refresh_with_key(StorageProjectionKey::from(&config), || {
                calculations += 1;
                Ok(projection)
            })
        );
        assert_eq!(calculations, 1);

        config.video.bitrate_mbps = 24;
        assert!(
            cache.refresh_with_key(StorageProjectionKey::from(&config), || {
                calculations += 1;
                Ok(projection)
            })
        );
        assert_eq!(calculations, 2);
    }

    #[test]
    fn storage_projection_cache_recalculates_real_estimates_and_preserves_active_snapshot() {
        let mut config = AppConfig::default();
        let mut cache = StorageProjectionCache::default();
        let mut run = StorageProjectionRun::default();

        assert!(cache.refresh(&config));
        let original = cache.projection().unwrap();
        assert_eq!(original.sizes[1], 124_320_000);
        let active_snapshot = cache.snapshot().unwrap();
        run.start_accepted(Some(active_snapshot));
        run.apply_event(&RecorderEvent::RecordingStarted {
            path: "active.mp4".into(),
            encoder: "H.264".into(),
        });

        config.recording.quality = clipper_storage::config::QualityPreset::Quality;
        assert!(cache.refresh(&config));
        let quality = cache.projection().unwrap();
        assert_eq!(quality.sizes[1], 184_320_000);
        assert_eq!(
            run.active().map(StorageProjectionSnapshot::projection),
            Some(original)
        );
        assert_ne!(
            run.active().map(StorageProjectionSnapshot::projection),
            Some(quality)
        );

        config.audio.separate_tracks = false;
        assert!(cache.refresh(&config));
        let mixed = cache.projection().unwrap();
        assert_eq!(mixed.sizes[1], 181_440_000);
        assert_eq!(
            run.active().map(StorageProjectionSnapshot::projection),
            Some(original)
        );

        run.apply_event(&RecorderEvent::RecordingFinished {
            path: "active.mp4".into(),
            stats: RecordingStats::default(),
            reason: StopReason::User,
        });
        assert_eq!(run.displayed(), None);
    }

    #[test]
    fn recording_projection_moves_from_pending_to_active_and_clears() {
        let projection = AppConfig::default().storage_projection().unwrap();
        let mut cache = StorageProjectionCache::default();
        assert!(cache.refresh(&AppConfig::default()));
        let snapshot = cache.snapshot().unwrap();
        let mut run = StorageProjectionRun::default();

        run.start_accepted(Some(snapshot));
        assert_eq!(
            run.pending().map(StorageProjectionSnapshot::projection),
            Some(projection)
        );
        assert_eq!(run.active(), None);
        assert_eq!(
            run.displayed().map(StorageProjectionSnapshot::projection),
            Some(projection)
        );

        run.recording_started();
        assert_eq!(run.pending(), None);
        assert_eq!(
            run.active().map(StorageProjectionSnapshot::projection),
            Some(projection)
        );

        run.apply_event(&RecorderEvent::RecordingFailed {
            message: "failed to start".into(),
        });
        assert_eq!(run.displayed(), None);
    }

    #[test]
    fn active_snapshot_detects_resolution_and_fps_changes_even_when_size_is_unchanged() {
        let mut config = AppConfig::default();
        let mut cache = StorageProjectionCache::default();
        assert!(cache.refresh(&config));
        let snapshot = cache.snapshot().unwrap();
        let active_projection = snapshot.projection();

        config.video.fps = 120;
        assert!(cache.refresh(&config));
        assert_eq!(cache.projection().unwrap(), active_projection);
        assert!(!snapshot.matches_key(cache.key()));
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

    fn state_with_source() -> RecordingState {
        let mut s = RecordingState::default();
        s.apply(RecorderEvent::SourcesListed(vec![src(
            SourceKind::Monitor,
            1,
        )]));
        s.select(SourceKind::Monitor, 1);
        s
    }

    #[test]
    fn replay_lifecycle_and_status() {
        let mut s = state_with_source();
        assert!(s.can_start_replay() && s.can_start());
        s.replay.begin_start();
        assert_eq!(s.replay.phase, ReplayPhase::Starting);
        assert!(!s.can_start() && !s.can_start_replay() && !s.settings_enabled());
        s.apply(RecorderEvent::ReplayStarted {
            encoder: "NVENC".into(),
        });
        assert!(s.replay.is_running());
        let status = ReplayStatus {
            buffered: Duration::from_secs(12),
            ..Default::default()
        };
        s.apply(RecorderEvent::ReplayStatus(status.clone()));
        assert_eq!(s.replay.status, Some(status));
        assert!(!s.can_start());
        assert!(s.start_hint().is_some());
        s.replay.begin_stop();
        assert_eq!(s.replay.phase, ReplayPhase::Stopping);
        s.apply(RecorderEvent::ReplayStopped { error: None });
        assert!(s.replay.is_off() && s.replay.status.is_none() && s.replay.error.is_none());
        assert!(s.can_start());
        // A late status does not revive the buffer.
        s.apply(RecorderEvent::ReplayStatus(ReplayStatus::default()));
        assert!(s.replay.status.is_none());
    }

    #[test]
    fn saves_ending_during_stop_neither_hold_nor_revive_it() {
        let mut s = state_with_source();
        s.replay.begin_start();
        s.apply(RecorderEvent::ReplayStarted {
            encoder: "hw".into(),
        });
        s.apply(RecorderEvent::ReplaySaving);
        s.apply(RecorderEvent::ReplaySaving);
        s.replay.begin_stop();
        let path = PathBuf::from("a.mp4");
        s.apply(RecorderEvent::ReplaySaved {
            path: path.clone(),
            duration: Duration::from_secs(30),
        });
        s.apply(RecorderEvent::ReplaySaveFailed {
            message: "disk full".into(),
        });
        assert_eq!(s.replay.phase, ReplayPhase::Stopping);
        assert_eq!(s.saved_clips[0].path, path);
        s.apply(RecorderEvent::ReplayStopped { error: None });
        assert!(s.replay.is_off());
        // A save finishing after the stop changes nothing, and replay restarts.
        s.apply(RecorderEvent::ReplaySaved {
            path: PathBuf::from("b.mp4"),
            duration: Duration::from_secs(30),
        });
        assert!(s.replay.is_off() && s.can_start_replay());
        s.replay.begin_start();
        assert_eq!(s.replay.phase, ReplayPhase::Starting);
    }

    #[test]
    fn replay_stop_with_error_shows_it_and_goes_off() {
        let mut s = state_with_source();
        s.replay.begin_start();
        s.apply(RecorderEvent::ReplayStarted {
            encoder: "hw".into(),
        });
        s.apply(RecorderEvent::ReplayStopped {
            error: Some("The capture source was closed".into()),
        });
        assert!(s.replay.is_off());
        assert_eq!(
            s.replay.error.as_deref(),
            Some("Replay stopped: the capture source was closed.")
        );
        s.replay.begin_start();
        assert_eq!(s.replay.error, None);
    }

    #[test]
    fn rejected_replay_start_returns_to_off() {
        let mut s = state_with_source();
        s.replay.begin_start();
        s.apply(RecorderEvent::Error {
            message: "Replay buffer: no encoder".into(),
        });
        assert!(s.replay.is_off());
        assert_eq!(
            s.replay.error.as_deref(),
            Some("Replay buffer: no encoder.")
        );
        assert_eq!(s.error, None);
    }

    #[test]
    fn replay_and_recording_exclude_each_other() {
        let mut s = state_with_source();
        s.begin_start();
        assert!(!s.can_start_replay());
        assert!(s.replay_hint().is_some());
        s.apply(RecorderEvent::RecordingFailed {
            message: "x".into(),
        });
        s.replay.phase = ReplayPhase::Running {
            encoder: "hw".into(),
        };
        assert!(!s.can_start());
    }

    #[test]
    fn recorder_stop_turns_replay_off() {
        let mut s = state_with_source();
        s.replay.phase = ReplayPhase::Running {
            encoder: "hw".into(),
        };
        s.apply(RecorderEvent::Stopped);
        assert!(s.replay.is_off());
    }

    #[test]
    fn saved_clips_are_newest_first_deduplicated_and_bounded() {
        let mut s = RecordingState::default();
        let saved = |name: String| RecorderEvent::ReplaySaved {
            path: name.into(),
            duration: Duration::from_secs(30),
        };
        s.apply(saved("a.mp4".into()));
        s.apply(saved("b.mp4".into()));
        s.apply(saved("a.mp4".into()));
        let names: Vec<_> = s.saved_clips.iter().map(SavedClip::name).collect();
        assert_eq!(names, ["a.mp4", "b.mp4"]);
        for i in 0..MAX_SAVED_CLIPS + 20 {
            s.apply(saved(format!("c{i}.mp4")));
        }
        assert_eq!(s.saved_clips.len(), MAX_SAVED_CLIPS);
        assert_eq!(
            s.saved_clips[0].name(),
            format!("c{}.mp4", MAX_SAVED_CLIPS + 19)
        );
    }

    #[test]
    fn replay_formatting() {
        assert_eq!(replay_stop_message(""), "Replay stopped unexpectedly.");
        assert_eq!(
            replay_stop_message("Replay is already running."),
            "Replay is already running."
        );
    }
}
