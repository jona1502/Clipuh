//! Turns real recorder and storage events into user-facing notices.
//!
//! Only engine-confirmed events produce notices: a clip is reported saved on
//! `ReplaySaved` (sent after the file is finalized), replay is reported on
//! after `ReplayStarted`, never when the request is merely sent.

use clipper_core::recording::{AudioSourceState, AudioSourceStatus, CaptureSource, RecorderEvent};

use crate::notification_center::{Category, Notice, Severity, ToastAction};
use crate::state::{ReplayPhase, replay_stop_message};
use crate::storage_status::VolumeUsage;

/// Notice key for every clip-save result, so a newer one replaces the older.
pub(crate) const CLIP_SAVE_KEY: &str = "clip-save";

/// App state just before `event` is applied.
pub(crate) struct Before<'a> {
    pub replay: &'a ReplayPhase,
    pub selected_source: Option<&'a CaptureSource>,
    pub audio: &'a [AudioSourceState],
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn notices(event: &RecorderEvent, before: &Before<'_>) -> Vec<Notice> {
    use Category as C;
    use Severity as S;
    let one = |n: Notice| vec![n];
    match event {
        RecorderEvent::ReplaySaving => one(Notice::new(
            CLIP_SAVE_KEY,
            C::ClipSave,
            S::Info,
            "Saving clip…",
            "Writing the last seconds of your replay.",
        )
        .progress()),
        RecorderEvent::ReplaySaved { path, duration } => one(Notice::new(
            CLIP_SAVE_KEY,
            C::ClipSave,
            S::Success,
            "Clip saved",
            format!("Last {} s · {}", duration.as_secs(), file_name(path)),
        )
        .action(ToastAction::OpenClip(path.clone()))),
        RecorderEvent::ReplaySaveFailed { message } => one(Notice::new(
            CLIP_SAVE_KEY,
            C::ClipSave,
            S::Error,
            "Clip not saved",
            message,
        )
        .desktop()),
        RecorderEvent::ReplayStarted { .. } => one(Notice::new(
            "replay",
            C::Replay,
            S::Success,
            "Instant Replay on",
            "Save the last moments with your Save Clip shortcut.",
        )),
        RecorderEvent::ReplayStopped { error: None } => one(Notice::new(
            "replay",
            C::Replay,
            S::Info,
            "Instant Replay off",
            "",
        )),
        RecorderEvent::ReplayStopped { error: Some(error) } => one(Notice::new(
            "replay",
            C::Replay,
            S::Error,
            "Instant Replay stopped",
            replay_stop_message(error),
        )
        .desktop()),
        // A rejected StartReplay arrives as a plain Error while starting.
        RecorderEvent::Error { message } if *before.replay == ReplayPhase::Starting => {
            one(Notice::new(
                "replay",
                C::Replay,
                S::Error,
                "Instant Replay could not start",
                replay_stop_message(message),
            )
            .desktop())
        }
        RecorderEvent::RecordingFinished { path, .. } => one(Notice::new(
            "recording",
            C::Recording,
            S::Success,
            "Recording saved",
            file_name(path),
        )
        .action(ToastAction::OpenClip(path.clone()))),
        RecorderEvent::RecordingFailed { message } => one(Notice::new(
            "recording",
            C::Recording,
            S::Error,
            "Recording failed",
            message,
        )
        .desktop()),
        RecorderEvent::SourcesListed(sources) => match before.selected_source {
            Some(old) if !sources.iter().any(|s| s.kind == old.kind && s.id == old.id) => {
                one(Notice::new(
                    "capture-source",
                    C::Capture,
                    S::Warning,
                    "Capture source unavailable",
                    format!(
                        "{} is no longer available. Select another source.",
                        old.name
                    ),
                )
                .desktop())
            }
            _ => Vec::new(),
        },
        RecorderEvent::AudioSources(states) => states
            .iter()
            .filter_map(|state| audio_notice(state, before.audio))
            .collect(),
        RecorderEvent::Stopped => one(Notice::new(
            "recorder",
            C::Capture,
            S::Error,
            "Recorder stopped",
            "Restart clipuh to capture again.",
        )
        .desktop()),
        _ => Vec::new(),
    }
}

/// A source that was capturing and now waits (device unplugged, app closed)
/// or failed. Repeated reports of the same status say nothing new.
fn audio_notice(state: &AudioSourceState, before: &[AudioSourceState]) -> Option<Notice> {
    let previous = before.iter().find(|s| s.source == state.source);
    if previous.is_some_and(|p| p.status == state.status) {
        return None;
    }
    let was_capturing =
        previous.is_some_and(|p| matches!(p.status, AudioSourceStatus::Capturing { .. }));
    let key = format!("audio-{}", state.source);
    match &state.status {
        AudioSourceStatus::Waiting(reason) if was_capturing => Some(Notice::new(
            key,
            Category::Audio,
            Severity::Warning,
            "Audio source disconnected",
            reason.clone(),
        )),
        AudioSourceStatus::Failed(reason) => Some(
            Notice::new(
                key,
                Category::Audio,
                Severity::Error,
                "Audio source failed",
                reason.clone(),
            )
            .desktop(),
        ),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StorageLevel {
    #[default]
    Ok,
    Low,
    Full,
}

const GIB: u64 = 1 << 30;
/// Below this a few long clips can fill the disk.
pub(crate) const LOW_FREE_BYTES: u64 = 5 * GIB;
/// Below this saving a clip is likely to fail.
pub(crate) const FULL_FREE_BYTES: u64 = GIB / 2;

pub(crate) fn storage_level(usage: &VolumeUsage) -> StorageLevel {
    match usage.free_bytes {
        f if f < FULL_FREE_BYTES => StorageLevel::Full,
        f if f < LOW_FREE_BYTES => StorageLevel::Low,
        _ => StorageLevel::Ok,
    }
}

/// A notice only when storage got worse than it was.
pub(crate) fn storage_notice(previous: StorageLevel, now: StorageLevel) -> Option<Notice> {
    if now <= previous {
        return None;
    }
    let (severity, title, body) = match now {
        StorageLevel::Ok => return None,
        StorageLevel::Low => (
            Severity::Warning,
            "Low storage space",
            "Less than 5 GB free on the clip drive. Free up space or change the clip folder.",
        ),
        StorageLevel::Full => (
            Severity::Error,
            "Storage almost full",
            "Less than 512 MB free on the clip drive. New clips may fail to save.",
        ),
    };
    Some(
        Notice::new("storage", Category::Storage, severity, title, body)
            .action(ToastAction::OpenSettings)
            .desktop(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_core::recording::SourceKind;
    use std::time::Duration;

    fn before<'a>(
        replay: &'a ReplayPhase,
        source: Option<&'a CaptureSource>,
        audio: &'a [AudioSourceState],
    ) -> Before<'a> {
        Before {
            replay,
            selected_source: source,
            audio,
        }
    }
    fn src(id: u64) -> CaptureSource {
        CaptureSource {
            kind: SourceKind::Monitor,
            id,
            name: format!("Display {id}"),
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn save_success_only_after_saved_event() {
        let off = ReplayPhase::Off;
        let b = before(&off, None, &[]);
        let saving = notices(&RecorderEvent::ReplaySaving, &b);
        assert!(saving[0].progress);
        assert_ne!(saving[0].severity, Severity::Success);
        let saved = notices(
            &RecorderEvent::ReplaySaved {
                path: "C:/clips/a.mp4".into(),
                duration: Duration::from_secs(30),
            },
            &b,
        );
        assert_eq!(saved[0].severity, Severity::Success);
        assert_eq!(saved[0].key, CLIP_SAVE_KEY);
        assert!(saved[0].body.contains("a.mp4"));
        let failed = notices(
            &RecorderEvent::ReplaySaveFailed {
                message: "disk full".into(),
            },
            &b,
        );
        assert_eq!(failed[0].severity, Severity::Error);
        assert!(failed[0].desktop);
    }

    #[test]
    fn replay_state_feedback() {
        let starting = ReplayPhase::Starting;
        let b = before(&starting, None, &[]);
        let on = notices(
            &RecorderEvent::ReplayStarted {
                encoder: "x".into(),
            },
            &b,
        );
        assert_eq!(on[0].severity, Severity::Success);
        let err = notices(
            &RecorderEvent::Error {
                message: "no encoder".into(),
            },
            &b,
        );
        assert_eq!(err[0].title, "Instant Replay could not start");
        let off = ReplayPhase::Off;
        assert!(
            notices(
                &RecorderEvent::Error {
                    message: "x".into()
                },
                &before(&off, None, &[])
            )
            .is_empty()
        );
        let stopped = notices(
            &RecorderEvent::ReplayStopped {
                error: Some("capture lost".into()),
            },
            &b,
        );
        assert_eq!(stopped[0].severity, Severity::Error);
        assert!(stopped[0].body.contains("capture lost"));
        let quiet = notices(&RecorderEvent::ReplayStopped { error: None }, &b);
        assert_eq!(quiet[0].severity, Severity::Info);
    }

    #[test]
    fn source_disappearing_warns_once() {
        let off = ReplayPhase::Off;
        let s1 = src(1);
        let b = before(&off, Some(&s1), &[]);
        assert!(notices(&RecorderEvent::SourcesListed(vec![src(1), src(2)]), &b).is_empty());
        let gone = notices(&RecorderEvent::SourcesListed(vec![src(2)]), &b);
        assert_eq!(gone[0].severity, Severity::Warning);
        assert!(
            notices(
                &RecorderEvent::SourcesListed(vec![]),
                &before(&off, None, &[])
            )
            .is_empty()
        );
    }

    #[test]
    fn audio_disconnect_detection() {
        let off = ReplayPhase::Off;
        let capturing = [AudioSourceState {
            source: 3,
            status: AudioSourceStatus::Capturing { track: None },
        }];
        let waiting = AudioSourceState {
            source: 3,
            status: AudioSourceStatus::Waiting("Device removed".into()),
        };
        let n = notices(
            &RecorderEvent::AudioSources(vec![waiting.clone()]),
            &before(&off, None, &capturing),
        );
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].key, "audio-3");
        // Same status again, or waiting from the start: nothing new.
        let w = [waiting.clone()];
        assert!(
            notices(
                &RecorderEvent::AudioSources(vec![waiting.clone()]),
                &before(&off, None, &w)
            )
            .is_empty()
        );
        assert!(
            notices(
                &RecorderEvent::AudioSources(vec![waiting]),
                &before(&off, None, &[])
            )
            .is_empty()
        );
    }

    #[test]
    fn storage_levels_and_transitions() {
        let u = |free| VolumeUsage {
            total_bytes: 100 * GIB,
            free_bytes: free,
        };
        assert_eq!(storage_level(&u(50 * GIB)), StorageLevel::Ok);
        assert_eq!(storage_level(&u(2 * GIB)), StorageLevel::Low);
        assert_eq!(storage_level(&u(GIB / 4)), StorageLevel::Full);
        assert!(storage_notice(StorageLevel::Ok, StorageLevel::Ok).is_none());
        assert!(storage_notice(StorageLevel::Low, StorageLevel::Low).is_none());
        assert!(storage_notice(StorageLevel::Full, StorageLevel::Low).is_none());
        assert_eq!(
            storage_notice(StorageLevel::Ok, StorageLevel::Low)
                .unwrap()
                .severity,
            Severity::Warning
        );
        assert_eq!(
            storage_notice(StorageLevel::Low, StorageLevel::Full)
                .unwrap()
                .severity,
            Severity::Error
        );
    }
}
