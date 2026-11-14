use clipper_core::recording::{
    QualityProfile, REPLAY_DURATIONS_SECONDS, RecorderCommand, SourceKind,
};
use clipper_storage::{VIDEO_FPS, VIDEO_RESOLUTIONS, video_mode_supported};
use gpui::prelude::*;
use gpui::{Context, Div, Window, div, px};

use super::{CaptureMenu, ClipperWindow};
use crate::components::{ButtonVariant, DropdownOption, Tone, button, panel, status_dot, step};
use crate::state::{Phase, ReplayPhase, resolution_label};
use crate::theme::ActiveTheme;

pub(super) fn content(
    window: &ClipperWindow,
    win: &Window,
    cx: &mut Context<ClipperWindow>,
) -> Div {
    let t = *cx.theme();
    // Three equal cards in one row; each stretches to the tallest so their
    // edges line up. Below 3 x MIN_CARD_WIDTH they wrap instead of squeezing.
    let card = |content: Div| {
        div()
            .flex_1()
            .flex_basis(px(0.))
            .min_w(px(MIN_CARD_WIDTH))
            .flex()
            .flex_col()
            .child(content.flex_1())
    };
    div()
        .flex()
        .flex_col()
        .gap(t.metrics.section_gap)
        .children(
            window
                .recording
                .error
                .clone()
                .map(|message| error_banner(message, cx)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(t.metrics.gap * 2.)
                .child(card(window.source_card(win, cx)))
                .child(card(window.storage_card(win, cx)))
                .child(card(window.recent_card(cx))),
        )
        // The audio mixer region: today's working audio controls live here
        // until the Phase 4 mixer replaces them in place.
        .child(super::audio::content(window, cx))
}

/// Narrowest a dashboard card gets before the row wraps.
const MIN_CARD_WIDTH: f32 = 240.0;

fn error_banner(message: String, cx: &mut Context<ClipperWindow>) -> Div {
    let t = *cx.theme();
    panel(&t)
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .border_color(t.colors.recording)
        .child(status_dot(&t, Tone::Danger))
        .child(div().flex_1().text_sm().child(message))
        .child(
            button(
                &t,
                "dismiss-error",
                "Dismiss",
                ButtonVariant::Ghost,
                None,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.recording.dismiss_error();
                cx.notify();
            })),
        )
}

/// Base bitrates offered in the capture settings.
const VIDEO_BITRATES_MBPS: [u32; 5] = [8, 16, 24, 32, 50];

fn option(label: String, detail: Option<String>, reason: Option<String>) -> DropdownOption {
    DropdownOption {
        label: label.into(),
        detail: detail.map(Into::into),
        enabled: reason.is_none(),
        reason: reason.map(Into::into),
    }
}

fn resolution_options(fps: u32) -> Vec<DropdownOption> {
    VIDEO_RESOLUTIONS
        .into_iter()
        .map(|(w, h)| {
            option(
                resolution_label(w, h),
                Some(format!("{w} × {h}")),
                (!video_mode_supported(w, h, fps)).then(|| format!("Not supported at {fps} fps")),
            )
        })
        .collect()
}

fn fps_options(width: u32, height: u32) -> Vec<DropdownOption> {
    VIDEO_FPS
        .into_iter()
        .map(|fps| {
            option(
                format!("{fps} FPS"),
                None,
                (!video_mode_supported(width, height, fps))
                    .then(|| format!("Not supported at {}", resolution_label(width, height))),
            )
        })
        .collect()
}

fn quality_options(base_mbps: u32) -> Vec<DropdownOption> {
    QualityProfile::ALL
        .into_iter()
        .map(|profile| {
            let mode = if profile == QualityProfile::Performance {
                "CBR"
            } else {
                "VBR"
            };
            let mbps = profile.bitrate_bps(base_mbps) / 1_000_000;
            option(
                profile.label().into(),
                Some(format!("{mbps} Mbps {mode}")),
                None,
            )
        })
        .collect()
}

fn replay_duration_options() -> Vec<DropdownOption> {
    REPLAY_DURATIONS_SECONDS
        .into_iter()
        .map(|s| {
            let label = if s >= 120 && s % 60 == 0 {
                format!("{} min", s / 60)
            } else {
                format!("{s} sec")
            };
            option(label, None, None)
        })
        .collect()
}

/// True for resolution/fps when the running capture differs from config.
fn pending_video(
    menu: CaptureMenu,
    active: Option<(u32, u32, u32)>,
    config: (u32, u32, u32),
) -> bool {
    let Some(active) = active else { return false };
    match menu {
        CaptureMenu::Resolution => (active.0, active.1) != (config.0, config.1),
        CaptureMenu::FrameRate => active.2 != config.2,
        _ => false,
    }
}

fn bitrate_options() -> Vec<DropdownOption> {
    VIDEO_BITRATES_MBPS
        .into_iter()
        .map(|mbps| {
            option(
                format!("{mbps} Mbps"),
                Some("Quality preset adjusts the target".into()),
                None,
            )
        })
        .collect()
}

impl ClipperWindow {
    pub(super) fn capture_options(&self, menu: CaptureMenu) -> Vec<DropdownOption> {
        let video = &self.config.video;
        match menu {
            CaptureMenu::Resolution => resolution_options(video.fps),
            CaptureMenu::FrameRate => fps_options(video.width, video.height),
            CaptureMenu::Quality => quality_options(video.bitrate_mbps),
            CaptureMenu::Bitrate => bitrate_options(),
            CaptureMenu::ReplayDuration => replay_duration_options(),
        }
    }

    pub(super) fn capture_selected(&self, menu: CaptureMenu) -> usize {
        let video = &self.config.video;
        match menu {
            CaptureMenu::Resolution => VIDEO_RESOLUTIONS
                .iter()
                .position(|r| *r == (video.width, video.height)),
            CaptureMenu::FrameRate => VIDEO_FPS.iter().position(|f| *f == video.fps),
            CaptureMenu::Quality => QualityProfile::ALL
                .iter()
                .position(|p| *p == QualityProfile::from(self.config.recording.quality)),
            CaptureMenu::Bitrate => VIDEO_BITRATES_MBPS
                .iter()
                .position(|b| *b == video.bitrate_mbps),
            CaptureMenu::ReplayDuration => REPLAY_DURATIONS_SECONDS
                .iter()
                .position(|s| *s == self.config.replay.duration_seconds),
        }
        .unwrap_or(0)
    }

    pub(super) fn capture_toggle(&mut self, menu: CaptureMenu, cx: &mut Context<Self>) {
        if matches!(self.capture_menu, Some((open, _)) if open == menu) {
            self.capture_close(cx);
        } else {
            self.capture_menu = Some((menu, self.capture_selected(menu)));
            cx.notify();
        }
    }

    pub(super) fn capture_close(&mut self, cx: &mut Context<Self>) {
        if self.capture_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn capture_apply(
        &mut self,
        menu: CaptureMenu,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.capture_menu = None;
        let enabled = self
            .capture_options(menu)
            .get(index)
            .is_some_and(|o| o.enabled);
        if !enabled {
            cx.notify();
            return;
        }
        match menu {
            CaptureMenu::Resolution => {
                (self.config.video.width, self.config.video.height) = VIDEO_RESOLUTIONS[index];
            }
            CaptureMenu::FrameRate => self.config.video.fps = VIDEO_FPS[index],
            CaptureMenu::Quality => {
                self.config.recording.quality = QualityProfile::ALL[index].into()
            }
            CaptureMenu::Bitrate => self.config.video.bitrate_mbps = VIDEO_BITRATES_MBPS[index],
            CaptureMenu::ReplayDuration => {
                let seconds = REPLAY_DURATIONS_SECONDS[index];
                self.config.replay.duration_seconds = seconds;
                // Applied live; the buffer keeps capturing.
                if matches!(
                    self.recording.replay.phase,
                    ReplayPhase::Starting | ReplayPhase::Running { .. }
                ) {
                    self.send(RecorderCommand::SetReplayDuration(
                        std::time::Duration::from_secs(u64::from(seconds)),
                    ));
                }
            }
        }
        self.persist_config(cx);
    }

    pub(super) fn capture_confirm(&mut self, menu: CaptureMenu, cx: &mut Context<Self>) {
        match self.capture_menu {
            Some((open, highlighted)) if open == menu => self.capture_apply(menu, highlighted, cx),
            _ => self.capture_toggle(menu, cx),
        }
    }

    pub(super) fn capture_step(&mut self, menu: CaptureMenu, delta: isize, cx: &mut Context<Self>) {
        let Some((open, from)) = self.capture_menu.filter(|(open, _)| *open == menu) else {
            self.capture_toggle(menu, cx);
            return;
        };
        let next = step(&self.capture_options(open), from, delta);
        if next != from {
            self.capture_menu = Some((menu, next));
            cx.notify();
        }
    }
}

impl ClipperWindow {
    pub(super) fn start_recording(&mut self) {
        if self.audio_testing {
            self.recording
                .fail("Stop the audio test before recording.".into());
            return;
        }
        let Some(source) = self.recording.selected_source().cloned() else {
            return;
        };
        if !self.recording.can_start() {
            return;
        }
        let options = match self.config.recording_options() {
            Ok(options) => options,
            Err(error) => {
                self.recording.fail(error.to_string());
                return;
            }
        };
        let video = (options.width, options.height, options.fps);
        if self.send(RecorderCommand::StartRecording { source, options }) {
            self.active_video = Some(video);
            self.storage_projection_run
                .start_accepted(self.storage_projection_cache.snapshot());
            self.recording.begin_start();
        }
    }

    /// Whether the menu's configured value differs from what the running
    /// capture started with (it applies on restart).
    pub(super) fn capture_pending(&self, menu: CaptureMenu) -> bool {
        let video = &self.config.video;
        pending_video(
            menu,
            self.active_video,
            (video.width, video.height, video.fps),
        )
    }

    /// Clears `active_video` once neither replay nor recording is running.
    pub(super) fn refresh_active_video(&mut self) {
        if self.recording.phase == Phase::Idle && self.recording.replay.phase == ReplayPhase::Off {
            self.active_video = None;
        }
    }

    /// Starts replay with the selected source. Does not touch the config.
    fn start_replay(&mut self) -> bool {
        if self.audio_testing {
            self.recording
                .fail("Stop the audio test before starting instant replay.".into());
            return false;
        }
        let Some(source) = self.recording.selected_source().cloned() else {
            return false;
        };
        if !self.recording.can_start_replay() {
            return false;
        }
        let options = match self.config.replay_options() {
            Ok(options) => options,
            Err(error) => {
                self.recording.fail(error.to_string());
                return false;
            }
        };
        let video = (
            options.recording.width,
            options.recording.height,
            options.recording.fps,
        );
        if self.send(RecorderCommand::StartReplay { source, options }) {
            self.active_video = Some(video);
            self.recording.replay.begin_start();
            true
        } else {
            false
        }
    }

    fn stop_replay(&mut self) -> bool {
        if self.send(RecorderCommand::StopReplay) {
            self.recording.replay.begin_stop();
            true
        } else {
            false
        }
    }

    /// Turns replay on or off and remembers the choice. When it cannot start
    /// right now (no source, recording active) only the preference changes, so
    /// it starts on the next launch.
    pub(super) fn toggle_replay(&mut self, cx: &mut Context<Self>) {
        // A command that could not be sent changes nothing, so the saved
        // preference (autostart) only follows requests that went out.
        let enabled = match self.recording.replay.phase {
            ReplayPhase::Running { .. } => {
                if !self.stop_replay() {
                    cx.notify();
                    return;
                }
                false
            }
            ReplayPhase::Off if self.recording.can_start_replay() => {
                if !self.start_replay() {
                    cx.notify();
                    return;
                }
                true
            }
            ReplayPhase::Off => !self.config.replay.enabled,
            ReplayPhase::Starting | ReplayPhase::Stopping => return,
        };
        if self.config.replay.enabled != enabled {
            self.config.replay.enabled = enabled;
            self.persist_config(cx);
        }
        cx.notify();
    }

    /// Once per launch, starts replay if it was left enabled: with the selected
    /// source, else the first (primary) monitor.
    pub(super) fn maybe_autostart_replay(&mut self) {
        if !self.replay_autostart_pending || self.recording.sources.is_empty() {
            return;
        }
        self.replay_autostart_pending = false;
        if self.recording.selected_source().is_none()
            && let Some(monitor) = self
                .recording
                .sources
                .iter()
                .find(|source| source.kind == SourceKind::Monitor)
                .map(|source| source.id)
        {
            self.recording.select(SourceKind::Monitor, monitor);
        }
        self.start_replay();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_duration_labels() {
        let labels: Vec<_> = replay_duration_options()
            .into_iter()
            .map(|o| o.label.to_string())
            .collect();
        assert_eq!(labels, ["15 sec", "30 sec", "60 sec", "2 min", "5 min"]);
    }

    #[test]
    fn pending_only_when_running_video_differs() {
        let cfg = (1920, 1080, 60);
        assert!(!pending_video(CaptureMenu::Resolution, None, cfg));
        assert!(!pending_video(CaptureMenu::Resolution, Some(cfg), cfg));
        assert!(pending_video(
            CaptureMenu::Resolution,
            Some((2560, 1440, 60)),
            cfg
        ));
        assert!(!pending_video(
            CaptureMenu::FrameRate,
            Some((2560, 1440, 60)),
            cfg
        ));
        assert!(pending_video(
            CaptureMenu::FrameRate,
            Some((1920, 1080, 30)),
            cfg
        ));
        assert!(!pending_video(
            CaptureMenu::ReplayDuration,
            Some((1, 1, 1)),
            cfg
        ));
    }

    #[test]
    fn unsupported_resolution_is_disabled_with_reason() {
        let options = resolution_options(120);
        let k4 = options.iter().find(|o| o.label == "4K").unwrap();
        assert!(!k4.enabled);
        assert_eq!(
            k4.reason.as_ref().map(|r| r.as_ref()),
            Some("Not supported at 120 fps")
        );
        assert!(options.iter().find(|o| o.label == "900p").unwrap().enabled);
    }

    #[test]
    fn unsupported_fps_is_disabled_at_4k() {
        let options = fps_options(3840, 2160);
        let fps120 = options.iter().find(|o| o.label == "120 FPS").unwrap();
        assert!(!fps120.enabled);
        assert_eq!(
            fps120.reason.as_ref().map(|r| r.as_ref()),
            Some("Not supported at 4K")
        );
        assert!(options.iter().all(|o| o.label != "144 FPS"));
    }

    #[test]
    fn quality_details_show_effective_bitrate() {
        let details: Vec<_> = quality_options(16)
            .into_iter()
            .map(|o| o.detail.unwrap().to_string())
            .collect();
        assert_eq!(details, ["16 Mbps CBR", "16 Mbps VBR", "24 Mbps VBR"]);
    }
}
