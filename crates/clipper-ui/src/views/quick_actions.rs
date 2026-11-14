//! Quick actions (tray menu, toast buttons), in-app toasts and the routing of
//! notices to the in-app stack, the clip overlay and native notifications.
//!
//! Every action calls the same code paths as the main window (replay toggle,
//! `RecorderCommand::SaveReplay`, library navigation); nothing here keeps its
//! own copy of replay or library state.

use std::path::Path;
use std::time::Instant;

use clipper_core::recording::RecorderCommand;
use clipper_platform::windows::tray::{
    self, Balloon, BalloonKind, TrayCommand, TrayHandle, TrayMenuState, TrayReplay,
};
use clipper_platform::windows::{notify, shell};
use gpui::prelude::*;
use gpui::{Context, Div, Task, Window, div, px};

use super::{ClipperWindow, Page};
use crate::components::{ButtonVariant, Tone, button, status_dot};
use crate::feedback::{self, StorageLevel};
use crate::notification_center::{
    Category, Notice, NotificationCenter, Severity, ToastAction, allowed, display_duration,
};
use crate::recent_clips::RecentClips;
use crate::state::{Phase, RecordingState, ReplayPhase};
use crate::storage_status::StorageStatus;
use crate::theme::ActiveTheme;

const TOAST_W: f32 = 340.0;
const TOAST_GAP: f32 = 12.0;

/// Sound follows the route that owns the result, independently of rendering.
/// The overlay already owns replay results outside the focused in-app stack.
fn notice_sound(
    notice: &Notice,
    settings: &clipper_storage::NotificationSettings,
    in_front: bool,
    native_delivered: bool,
    quiet_time: bool,
) -> bool {
    if !allowed(notice, settings) || !settings.sound || notice.progress {
        return false;
    }
    if notice.category == Category::ClipSave {
        return in_front && settings.in_app;
    }
    if notice.category == Category::Recording
        && notice.severity == Severity::Success
        && matches!(notice.action, Some(ToastAction::OpenClip(_)))
    {
        return true;
    }
    native_delivered && !quiet_time
}

/// Notification and quick-action state owned by the main window.
#[derive(Default)]
pub(crate) struct Feedback {
    pub toasts: NotificationCenter,
    /// The single pending expiry wake-up; dropping it cancels it.
    pub toast_timer: Option<Task<()>>,
    pub tray: Option<TrayHandle>,
    pub main_hwnd: Option<isize>,
    pub storage_level: StorageLevel,
    /// The library has at least one clip (updated on library changes).
    pub has_recent_clip: bool,
    /// The close-to-tray hint was shown this session.
    pub tray_hint_shown: bool,
}

/// Why Save Clip cannot run right now, in plain words; `None` when it can.
pub(crate) fn save_unavailable(recording: &RecordingState) -> Option<&'static str> {
    match recording.replay.phase {
        ReplayPhase::Running { .. } => None,
        ReplayPhase::Starting => Some("Instant Replay is still starting. Try again in a moment."),
        ReplayPhase::Stopping => Some("Instant Replay is stopping."),
        ReplayPhase::Off if recording.phase != Phase::Idle => {
            Some("Clips are saved from Instant Replay. Stop the recording and turn it on.")
        }
        ReplayPhase::Off => Some("Instant Replay is off. Turn it on to save clips."),
    }
}

/// The tray's view of the authoritative app state.
pub(crate) fn tray_state(recording: &RecordingState, has_recent_clip: bool) -> TrayMenuState {
    let replay = match recording.replay.phase {
        ReplayPhase::Running { .. } => TrayReplay::On,
        ReplayPhase::Starting => TrayReplay::Starting,
        ReplayPhase::Stopping => TrayReplay::Stopping,
        ReplayPhase::Off if recording.phase != Phase::Idle => TrayReplay::Unavailable,
        ReplayPhase::Off => TrayReplay::Off,
    };
    TrayMenuState {
        replay,
        can_save: save_unavailable(recording).is_none(),
        has_recent_clip,
    }
}

fn tone(severity: Severity) -> Tone {
    match severity {
        Severity::Info => Tone::Neutral,
        Severity::Success => Tone::Success,
        Severity::Warning => Tone::Warning,
        Severity::Error => Tone::Danger,
    }
}

fn balloon_kind(severity: Severity) -> BalloonKind {
    match severity {
        Severity::Info | Severity::Success => BalloonKind::Info,
        Severity::Warning => BalloonKind::Warning,
        Severity::Error => BalloonKind::Error,
    }
}

fn open_error(err: &std::io::Error, what: &str) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        format!("{what} not found. It may have been moved or deleted.")
    } else {
        err.to_string()
    }
}

impl ClipperWindow {
    /// The main window is visible and in front, so the user sees in-app toasts.
    pub(super) fn main_in_front(&self) -> bool {
        self.feedback
            .main_hwnd
            .is_some_and(tray::is_foreground_window)
    }

    /// Routes one notice by the user's preferences:
    /// - clip-save results go to the existing overlay toast unless the main
    ///   window is in front (then in-app), never to both;
    /// - everything else goes to the in-app stack, and important notices also
    ///   to a native notification while the main window is not in front.
    pub(super) fn notify_user(&mut self, notice: Notice, cx: &mut Context<Self>) {
        let settings = &self.config.notifications;
        if !allowed(&notice, settings) {
            return;
        }
        let in_front = self.main_in_front();
        if notice.category == Category::ClipSave && !in_front {
            return;
        }
        let desktop = settings.native && notice.desktop && !in_front;
        let native_delivered = desktop && self.feedback.tray.is_some();
        if notice_sound(
            &notice,
            settings,
            in_front,
            native_delivered,
            native_delivered && notify::notification_quiet_time_active(),
        ) {
            self.notifier.play_sound(settings.custom_sound.clone());
        }
        if desktop && let Some(tray) = &self.feedback.tray {
            tray.notify(Balloon {
                kind: balloon_kind(notice.severity),
                title: notice.title.clone(),
                body: notice.body.clone(),
                // The app owns default/custom playback; no second system sound.
                sound: false,
            });
        }
        if settings.in_app {
            let duration = display_duration(&notice, settings);
            self.feedback.toasts.push(notice, Instant::now(), duration);
            self.schedule_toast_expiry(cx);
            cx.notify();
        }
    }

    /// Keeps exactly one timer armed for the earliest toast deadline.
    fn schedule_toast_expiry(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.feedback.toasts.next_deadline() else {
            self.feedback.toast_timer = None;
            return;
        };
        self.feedback.toast_timer = Some(cx.spawn(async move |this, cx| {
            let wait = deadline.saturating_duration_since(Instant::now());
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                if this.feedback.toasts.expire(Instant::now()) {
                    cx.notify();
                }
                this.schedule_toast_expiry(cx);
            });
        }));
    }

    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.feedback.toasts.dismiss(id) {
            self.schedule_toast_expiry(cx);
            cx.notify();
        }
    }

    /// Notice for a new free-space reading, if storage got worse.
    pub(super) fn on_storage_status(&mut self, status: &StorageStatus, cx: &mut Context<Self>) {
        let StorageStatus::Ready(usage) = status else {
            return;
        };
        let level = feedback::storage_level(usage);
        let previous = std::mem::replace(&mut self.feedback.storage_level, level);
        if let Some(notice) = feedback::storage_notice(previous, level) {
            self.notify_user(notice, cx);
        }
    }

    pub(super) fn on_library_changed(&mut self, cx: &mut Context<Self>) {
        self.feedback.has_recent_clip =
            matches!(self.library.read(cx).recent(), RecentClips::Ready(c) if !c.is_empty());
    }

    /// Pushes the current state to the tray; a no-op when nothing changed.
    pub(super) fn sync_tray(&self) {
        if let Some(tray) = &self.feedback.tray {
            tray.set_state(tray_state(&self.recording, self.feedback.has_recent_clip));
        }
    }

    pub(super) fn run_tray_command(
        &mut self,
        command: TrayCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            TrayCommand::OpenMain => self.show_main(window),
            TrayCommand::ToggleReplay => self.quick_toggle_replay(cx),
            TrayCommand::SaveClip => self.quick_save_clip(cx),
            TrayCommand::OpenLibrary => {
                self.show_main(window);
                self.navigate(Page::Clips, window, cx);
            }
            TrayCommand::OpenRecentClip => self.open_recent_clip(window, cx),
            TrayCommand::OpenClipFolder => self.open_clip_folder(cx),
            TrayCommand::OpenSettings => {
                self.show_main(window);
                self.navigate(Page::Settings, window, cx);
            }
            // Recorder and hotkey shut down in order once the app loop ends.
            TrayCommand::Exit => cx.quit(),
        }
    }

    /// With the tray running and `app.minimize_to_tray` on, closing the main
    /// window hides it instead of quitting; replay keeps running. The setting
    /// is read at close time, so a Settings change applies immediately.
    pub(super) fn close_to_tray(hwnd: isize, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let Some(this) = this.upgrade() else {
                return true;
            };
            let app = &this.read(cx).config.app;
            if !(app.tray_icon && app.minimize_to_tray) {
                return true;
            }
            tray::hide_window(hwnd);
            this.update(cx, |this, cx| {
                if !std::mem::replace(&mut this.feedback.tray_hint_shown, true) {
                    let notice = Notice::new(
                        "tray-hint",
                        Category::Action,
                        Severity::Info,
                        "clipuh is still running",
                        "Instant Replay keeps running. Use the tray icon to reopen or exit.",
                    )
                    .desktop();
                    this.notify_user(notice, cx);
                }
            });
            false
        });
    }

    fn show_main(&self, window: &mut Window) {
        if let Some(hwnd) = self.feedback.main_hwnd {
            tray::show_window(hwnd);
        }
        window.activate_window();
    }

    fn action_notice(
        &mut self,
        severity: Severity,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        let notice = Notice::new("quick-action", Category::Action, severity, title, body).desktop();
        self.notify_user(notice, cx);
    }

    /// Same command as the global hotkey; the result arrives as Replay* events.
    pub(super) fn quick_save_clip(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = save_unavailable(&self.recording) {
            self.action_notice(Severity::Warning, "Clip not saved", reason, cx);
            return;
        }
        // Every press is a separate save: the recorder bounds its queue and
        // rejects overflow with a visible error, so nothing is dropped here.
        if let Err(error) = self.handle.send(RecorderCommand::SaveReplay) {
            let body = format!("Could not save the replay: {error}.");
            self.action_notice(Severity::Error, "Clip not saved", &body, cx);
        }
    }

    /// The capture bar's toggle; explains instead of silently flipping the
    /// saved preference when replay cannot start right now.
    pub(super) fn quick_toggle_replay(&mut self, cx: &mut Context<Self>) {
        let phase = &self.recording.replay.phase;
        if *phase == ReplayPhase::Off && !self.recording.can_start_replay() {
            let hint = if self.audio_testing {
                "Stop the audio test before starting Instant Replay."
            } else {
                self.recording
                    .replay_hint()
                    .unwrap_or("Instant Replay cannot start right now.")
            };
            self.action_notice(Severity::Warning, "Instant Replay is off", hint, cx);
            return;
        }
        let previous_error = self.recording.error.clone();
        self.toggle_replay(cx);
        if self.recording.error != previous_error
            && let Some(error) = self.recording.error.clone()
        {
            self.action_notice(
                Severity::Error,
                "Instant Replay could not start",
                &error,
                cx,
            );
        }
    }

    fn newest_clip(&self, cx: &Context<Self>) -> Option<std::path::PathBuf> {
        match self.library.read(cx).recent() {
            RecentClips::Ready(clips) => clips.into_iter().next().map(|c| c.clip.path),
            _ => None,
        }
    }

    /// Opens the newest clip the library knows; a missing file refreshes the
    /// library index instead of failing silently.
    pub(super) fn open_recent_clip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.newest_clip(cx) else {
            self.action_notice(
                Severity::Info,
                "No clips yet",
                "Saved clips appear here.",
                cx,
            );
            return;
        };
        self.open_clip_path(&path, window, cx);
    }

    fn open_clip_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = shell::open_path(path) {
            let body = open_error(&error, "Clip");
            self.action_notice(Severity::Error, "Could not open clip", &body, cx);
            if error.kind() == std::io::ErrorKind::NotFound {
                self.refresh_recent_clips(window, cx);
            }
        }
    }

    pub(super) fn open_clip_folder(&mut self, cx: &mut Context<Self>) {
        let result = self
            .config
            .recording_options()
            .map_err(|e| e.to_string())
            .and_then(|o| {
                shell::open_path(&o.output_directory).map_err(|e| open_error(&e, "Clip folder"))
            });
        if let Err(body) = result {
            self.action_notice(Severity::Error, "Could not open clip folder", &body, cx);
        }
    }

    fn run_toast_action(
        &mut self,
        id: u64,
        action: ToastAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_toast(id, cx);
        match action {
            ToastAction::OpenClip(path) => self.open_clip_path(&path, window, cx),
            ToastAction::OpenSettings => self.navigate(Page::Settings, window, cx),
        }
    }

    /// Compact toasts below the capture bar, top right: clear of the capture
    /// controls, the sidebar and the bottom-aligned editor transport.
    pub(super) fn toast_stack(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.feedback.toasts.is_empty() {
            return None;
        }
        let t = *cx.theme();
        let rows = self.feedback.toasts.toasts().map(|toast| {
            let id = toast.id;
            let n = &toast.notice;
            let title = if toast.count > 1 {
                format!("{} ×{}", n.title, toast.count)
            } else {
                n.title.clone()
            };
            let mut header = div()
                .flex()
                .items_center()
                .gap_2()
                .child(status_dot(&t, tone(n.severity)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(title),
                );
            if let Some(action) = n.action.clone() {
                header = header.child(
                    button(
                        &t,
                        ("toast-action", id as usize),
                        action.label(),
                        ButtonVariant::Secondary,
                        None,
                        true,
                    )
                    .flex_none()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_toast_action(id, action.clone(), window, cx)
                    })),
                );
            }
            header = header.child(
                button(
                    &t,
                    ("toast-dismiss", id as usize),
                    "×",
                    ButtonVariant::Ghost,
                    None,
                    true,
                )
                .flex_none()
                .on_click(cx.listener(move |this, _, _, cx| this.dismiss_toast(id, cx))),
            );
            let body = (!n.body.is_empty()).then(|| {
                div()
                    .pl(px(14.))
                    .text_xs()
                    .text_color(t.colors.text_secondary)
                    .line_clamp(2)
                    .child(n.body.clone())
            });
            div()
                .id(("toast", id as usize))
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(t.colors.border_strong)
                .bg(t.colors.surface)
                .text_color(t.colors.text)
                .shadow_md()
                .occlude()
                .child(header)
                .children(body)
        });
        Some(
            div()
                .absolute()
                .top(px(super::capture_bar::BAR_HEIGHT + TOAST_GAP))
                .right(px(16.))
                .w(px(TOAST_W))
                .flex()
                .flex_col()
                .gap_2()
                .children(rows),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_core::recording::{CaptureSource, RecorderEvent, SourceKind};

    fn running() -> RecordingState {
        let mut s = RecordingState::default();
        s.apply(RecorderEvent::ReplayStarted {
            encoder: "test".into(),
        });
        s
    }

    #[test]
    fn save_availability_follows_replay_phase() {
        let mut s = RecordingState::default();
        assert!(save_unavailable(&s).unwrap().contains("off"));
        s.replay.begin_start();
        assert!(save_unavailable(&s).unwrap().contains("starting"));
        let mut s = running();
        assert_eq!(save_unavailable(&s), None);
        s.replay.begin_stop();
        assert!(save_unavailable(&s).is_some());
    }

    #[test]
    fn tray_state_mirrors_engine_confirmed_replay() {
        let mut s = RecordingState::default();
        assert_eq!(tray_state(&s, false).replay, TrayReplay::Off);
        // Requested but not confirmed: not "On" yet.
        s.replay.begin_start();
        let t = tray_state(&s, true);
        assert_eq!(t.replay, TrayReplay::Starting);
        assert!(!t.can_save);
        assert!(t.has_recent_clip);
        s.apply(RecorderEvent::ReplayStarted {
            encoder: "x".into(),
        });
        let t = tray_state(&s, false);
        assert_eq!(t.replay, TrayReplay::On);
        assert!(t.can_save);
        s.apply(RecorderEvent::ReplayStopped {
            error: Some("encoder lost".into()),
        });
        assert_eq!(tray_state(&s, false).replay, TrayReplay::Off);
    }

    #[test]
    fn tray_replay_unavailable_while_recording() {
        let mut s = RecordingState::default();
        s.apply(RecorderEvent::SourcesListed(vec![CaptureSource {
            kind: SourceKind::Monitor,
            id: 1,
            name: "D".into(),
            width: 1,
            height: 1,
        }]));
        s.select(SourceKind::Monitor, 1);
        s.apply(RecorderEvent::RecordingStarted {
            path: "a.mp4".into(),
            encoder: "x".into(),
        });
        assert_eq!(tray_state(&s, false).replay, TrayReplay::Unavailable);
        assert!(save_unavailable(&s).unwrap().contains("recording"));
    }

    #[test]
    fn recorder_shutdown_turns_tray_off() {
        let mut s = running();
        s.apply(RecorderEvent::Stopped);
        let t = tray_state(&s, false);
        assert_eq!(t.replay, TrayReplay::Off);
        assert!(!t.can_save);
    }

    #[test]
    fn missing_recent_clip_is_not_found() {
        let missing = Path::new("Z:/clipuh-test/definitely-missing.mp4");
        let error = shell::open_path(missing).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(open_error(&error, "Clip").contains("moved or deleted"));
    }

    #[test]
    fn missing_file_message() {
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert!(open_error(&e, "Clip").starts_with("Clip not found"));
        let e = std::io::Error::other("denied");
        assert_eq!(open_error(&e, "Clip"), "denied");
    }

    #[test]
    fn completed_saves_sound_once_in_the_route_that_owns_them() {
        let settings = clipper_storage::NotificationSettings::default();
        let saved = Notice::new(
            "clip-save",
            Category::ClipSave,
            Severity::Success,
            "Saved",
            "",
        );
        assert!(notice_sound(&saved, &settings, true, false, false));
        assert!(!notice_sound(&saved, &settings, false, false, false));
        let mut overlay = settings.clone();
        overlay.in_app = false;
        assert!(!notice_sound(&saved, &overlay, true, false, false));
        assert!(!notice_sound(
            &saved.clone().progress(),
            &settings,
            true,
            false,
            false
        ));
        let manual = Notice::new(
            "recording",
            Category::Recording,
            Severity::Success,
            "Saved",
            "",
        )
        .action(ToastAction::OpenClip("a.mp4".into()));
        assert!(notice_sound(&manual, &settings, true, false, false));
        assert!(notice_sound(&manual, &settings, false, false, false));
        let replay = Notice::new("replay", Category::Replay, Severity::Success, "Started", "");
        assert!(!notice_sound(&replay, &settings, true, false, false));
    }

    #[test]
    fn notification_sound_respects_switches_failure_policy_and_native_quiet_time() {
        let warning =
            Notice::new("audio", Category::Audio, Severity::Error, "Failed", "").desktop();
        let mut settings = clipper_storage::NotificationSettings::default();
        assert!(notice_sound(&warning, &settings, false, true, false));
        assert!(!notice_sound(&warning, &settings, false, true, true));
        assert!(!notice_sound(&warning, &settings, false, false, false));
        settings.sound = false;
        assert!(!notice_sound(&warning, &settings, false, true, false));
        settings.sound = true;
        settings.enabled = false;
        assert!(!notice_sound(&warning, &settings, false, true, false));
        settings.enabled = true;
        settings.notify_on_failure = false;
        let failed = Notice::new(
            "clip-save",
            Category::ClipSave,
            Severity::Error,
            "Failed",
            "",
        );
        assert!(!notice_sound(&failed, &settings, true, false, false));
    }
}
