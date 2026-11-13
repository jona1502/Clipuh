use clipper_core::recording::{
    CaptureSource, QualityProfile, RecorderCommand, RecordingStats, SourceKind,
};
use clipper_storage::config::ReplayMode;
use clipper_storage::storage_projection::StorageProjection;
use clipper_storage::{VIDEO_FPS, VIDEO_RESOLUTIONS, video_mode_supported};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Window, div, px, rgb};

use super::{CaptureMenu, ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{
    ButtonVariant, DropdownOption, Tone, button, dropdown, empty_state, kbd, list_item, panel,
    property_row, section_header, stat_tile, status_dot, step, switch, value_text,
};
use crate::state::{
    Phase, ReplayPhase, format_decimal_size, format_elapsed, format_megabytes, format_mib,
    resolution_label, tier_label,
};
use crate::theme;

pub(super) fn content(
    window: &ClipperWindow,
    win: &Window,
    cx: &mut Context<ClipperWindow>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_6()
        .children(
            window
                .recording
                .error
                .clone()
                .map(|message| error_banner(message, cx)),
        )
        .child(replay(window, cx))
        .child(recording(window, cx))
        .child(sources(window, cx))
        .child(capture_settings(window, win, cx))
        .child(overview(window, cx))
        .child(performance(window))
        .child(recent_clips())
}

fn section(title: &'static str, note: Option<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(section_header(title, note))
}

fn error_banner(message: String, cx: &mut Context<ClipperWindow>) -> Div {
    panel()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .border_color(rgb(theme::RECORDING))
        .child(status_dot(Tone::Danger))
        .child(div().flex_1().text_sm().child(message))
        .child(
            button("dismiss-error", "Dismiss", ButtonVariant::Ghost, None, true).on_click(
                cx.listener(|this, _, _, cx| {
                    this.recording.dismiss_error();
                    cx.notify();
                }),
            ),
        )
}

fn replay(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let replay = &state.replay;
    let config = &window.config.replay;
    let (title, tone) = match replay.phase {
        ReplayPhase::Off => ("Instant replay is off", Tone::Neutral),
        ReplayPhase::Starting => ("Starting instant replay\u{2026}", Tone::Warning),
        ReplayPhase::Running { .. } => ("Instant replay is on", Tone::Success),
        ReplayPhase::Stopping => ("Stopping instant replay\u{2026}", Tone::Warning),
    };
    let detail: Option<SharedString> = match &replay.phase {
        ReplayPhase::Running { encoder } => Some(encoder.clone().into()),
        ReplayPhase::Off => state.replay_hint().map(Into::into).or_else(|| {
            Some(
                format!(
                    "Keeps the last {} s ready to save.",
                    config.duration_seconds
                )
                .into(),
            )
        }),
        _ => None,
    };
    let toggle_enabled = match replay.phase {
        ReplayPhase::Off => state.can_start_replay(),
        ReplayPhase::Running { .. } => true,
        ReplayPhase::Starting | ReplayPhase::Stopping => false,
    };
    let on = matches!(
        replay.phase,
        ReplayPhase::Starting | ReplayPhase::Running { .. }
    );
    let toggle = div()
        .id("replay-toggle")
        .when(!toggle_enabled, |this| this.opacity(0.45))
        .when(toggle_enabled, |this| {
            this.cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| this.toggle_replay(cx)))
        })
        .child(switch(on));
    let save_enabled = replay.is_running();
    let save = button(
        "save-replay",
        "Save replay",
        ButtonVariant::Secondary,
        None,
        save_enabled,
    )
    .when(save_enabled, |this| {
        this.on_click(cx.listener(|this, _, _, cx| {
            this.send(RecorderCommand::SaveReplay);
            cx.notify();
        }))
    });

    let header = div()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(status_dot(tone))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(detail.map(|detail| {
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .truncate()
                        .child(detail)
                })),
        )
        .child(save)
        .child(toggle);

    let mut card = panel().child(header);
    if replay.is_running() {
        let metric = |value: String| value_text(value);
        let (buffered, configured, ram, disk, tier, exports) = match &replay.status {
            Some(status) => (
                format_elapsed(status.buffered),
                format_elapsed(status.configured),
                format_megabytes(status.ram_bytes),
                format_megabytes(status.disk_bytes),
                tier_label(status.tier).to_owned(),
                match status.exports_in_flight {
                    0 => "None".to_owned(),
                    n => format!("{n} saving"),
                },
            ),
            None => (
                "\u{2014}".to_owned(),
                format_elapsed(std::time::Duration::from_secs(u64::from(
                    config.duration_seconds,
                ))),
                "\u{2014}".to_owned(),
                "\u{2014}".to_owned(),
                "\u{2014}".to_owned(),
                "\u{2014}".to_owned(),
            ),
        };
        card = card
            .child(property_row(
                "Buffered",
                Some("Footage available to save right now.".into()),
                metric(format!("{buffered} / {configured}")),
                false,
            ))
            .child(property_row("Memory", None, metric(ram), false))
            .child(property_row("Disk cache", None, metric(disk), false))
            .child(property_row("Storage", None, metric(tier), false))
            .child(property_row(
                "Saves in progress",
                None,
                metric(exports),
                false,
            ));
        if let Some(warning) = replay
            .status
            .as_ref()
            .and_then(|status| status.warning.clone())
        {
            card = card.child(replay_notice(warning, Tone::Warning));
        }
    }
    if let Some(error) = replay.error.clone() {
        card = card.child(replay_notice(error, Tone::Danger));
    }

    section(
        "Instant replay",
        Some(format!("Save: {}", config.hotkey).into()),
    )
    .child(card)
}

fn replay_notice(message: String, tone: Tone) -> Div {
    div()
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .border_t_1()
        .border_color(rgb(theme::BORDER))
        .child(status_dot(tone))
        .child(div().flex_1().min_w_0().text_sm().child(message))
}

fn recording(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let (label, variant, enabled) = match state.phase {
        Phase::Idle => ("Start recording", ButtonVariant::Primary, state.can_start()),
        Phase::Starting => ("Starting…", ButtonVariant::Primary, false),
        Phase::Recording => ("Stop recording", ButtonVariant::Danger, state.can_stop()),
        Phase::Stopping => ("Saving…", ButtonVariant::Danger, false),
    };
    let stop = state.phase == Phase::Recording;
    let toggle = button("record-toggle", label, variant, None, enabled).when(enabled, |this| {
        this.on_click(cx.listener(move |this, _, _, cx| {
            if stop {
                if this.send(RecorderCommand::StopRecording) {
                    this.recording.begin_stop();
                }
            } else {
                this.start_recording();
            }
            cx.notify();
        }))
    });

    let (title, detail): (SharedString, Option<SharedString>) = match state.phase {
        Phase::Recording => (
            format_elapsed(state.stats.elapsed).into(),
            state.encoder.as_ref().map(|encoder| encoder.clone().into()),
        ),
        Phase::Idle => match &state.finished {
            Some(finished) => (
                "Recording saved".into(),
                Some(finished.path.display().to_string().into()),
            ),
            None => (
                state.status_label().into(),
                state.start_hint().map(Into::into),
            ),
        },
        Phase::Starting | Phase::Stopping => (state.status_label().into(), None),
    };
    let tone = match state.phase {
        Phase::Idle if state.finished.is_some() => Tone::Success,
        Phase::Idle => Tone::Neutral,
        Phase::Starting => Tone::Warning,
        Phase::Recording | Phase::Stopping => Tone::Danger,
    };

    section("Recording", None).child(
        panel()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .child(status_dot(tone))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(detail.map(|detail| {
                        div()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_TERTIARY))
                            .truncate()
                            .child(detail)
                    })),
            )
            .child(toggle),
    )
}

fn sources(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let enabled = state.settings_enabled();
    let refresh = button(
        "refresh-sources",
        "Refresh",
        ButtonVariant::Ghost,
        None,
        enabled,
    )
    .when(enabled, |this| {
        this.on_click(cx.listener(|this, _, _, cx| {
            this.send(RecorderCommand::RefreshSources);
            cx.notify();
        }))
    });

    let mut list = div()
        .id("source-list")
        .max_h(px(240.))
        .overflow_y_scroll()
        .p_1()
        .flex()
        .flex_col()
        .gap_0p5();
    if state.sources.is_empty() {
        list = list.child(empty_state(
            "▭",
            "No sources found",
            "Open the game or window you want to record, then refresh.",
        ));
    } else {
        for (kind, title) in [
            (SourceKind::Monitor, "Displays"),
            (SourceKind::Window, "Windows"),
        ] {
            list = list
                .child(
                    div()
                        .px_2()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(title),
                )
                .children(
                    state
                        .sources
                        .iter()
                        .filter(|source| source.kind == kind)
                        .map(|source| {
                            let selected = state.selected == Some((source.kind, source.id));
                            source_row(source, selected, enabled, cx)
                        }),
                );
        }
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_header("Source", None))
                .child(refresh),
        )
        .child(panel().child(list))
}

fn source_row(
    source: &CaptureSource,
    selected: bool,
    enabled: bool,
    cx: &mut Context<ClipperWindow>,
) -> AnyElement {
    let (kind, id) = (source.kind, source.id);
    let (prefix, glyph) = match kind {
        SourceKind::Monitor => ("mon", "▭"),
        SourceKind::Window => ("win", "◰"),
    };
    list_item((prefix, id), glyph, source.name.clone(), None, selected)
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child(format!("{}×{}", source.width, source.height)),
        )
        .map(|this| {
            if enabled {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.recording.select(kind, id);
                    cx.notify();
                }))
            } else {
                this.cursor_default().opacity(0.45)
            }
        })
        .into_any_element()
}

/// Base bitrates offered in the capture settings.
const VIDEO_BITRATES_MBPS: [u32; 5] = [8, 16, 24, 32, 50];

const CAPTURE_MENUS: [CaptureMenu; 4] = [
    CaptureMenu::Resolution,
    CaptureMenu::FrameRate,
    CaptureMenu::Quality,
    CaptureMenu::Bitrate,
];

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

fn capture_settings(window: &ClipperWindow, win: &Window, cx: &mut Context<ClipperWindow>) -> Div {
    let note = (!window.recording.settings_enabled())
        .then(|| "Changes apply to the next recording.".into());
    let cards = CAPTURE_MENUS.into_iter().enumerate().map(|(index, menu)| {
        let options = window.capture_options(menu);
        let selected = window.capture_selected(menu);
        let focus = &window.capture_focus[index];
        let highlighted = window
            .capture_menu
            .filter(|(open, _)| *open == menu)
            .map(|(_, highlighted)| highlighted);
        let entity = cx.entity();
        let (toggle, select, dismiss) = (entity.clone(), entity.clone(), entity);
        div()
            .flex_grow()
            .flex_basis(px(180.))
            .min_w(px(180.))
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(menu.label()),
            )
            .child(dropdown(
                ("capture-menu", index),
                &options,
                selected,
                highlighted,
                focus,
                focus.is_focused(win),
                true,
                move |_, app| toggle.update(app, |this, cx| this.capture_toggle(menu, cx)),
                move |i, _, app| select.update(app, |this, cx| this.capture_apply(menu, i, cx)),
                move |_, app| dismiss.update(app, |this, cx| this.capture_close(cx)),
            ))
            .on_action(cx.listener(move |this, _: &Confirm, _, cx| this.capture_confirm(menu, cx)))
            .on_action(
                cx.listener(move |this, _: &SelectNext, _, cx| this.capture_step(menu, 1, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &SelectPrevious, _, cx| this.capture_step(menu, -1, cx)),
            )
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.capture_close(cx)))
    });
    section("Capture settings", note).child(div().flex().flex_wrap().gap_3().children(cards))
}

impl ClipperWindow {
    fn capture_options(&self, menu: CaptureMenu) -> Vec<DropdownOption> {
        let video = &self.config.video;
        match menu {
            CaptureMenu::Resolution => resolution_options(video.fps),
            CaptureMenu::FrameRate => fps_options(video.width, video.height),
            CaptureMenu::Quality => quality_options(video.bitrate_mbps),
            CaptureMenu::Bitrate => bitrate_options(),
        }
    }

    fn capture_selected(&self, menu: CaptureMenu) -> usize {
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
        }
        .unwrap_or(0)
    }

    fn capture_toggle(&mut self, menu: CaptureMenu, cx: &mut Context<Self>) {
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

    fn capture_apply(&mut self, menu: CaptureMenu, index: usize, cx: &mut Context<Self>) {
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
        }
        self.persist_config(cx);
    }

    fn capture_confirm(&mut self, menu: CaptureMenu, cx: &mut Context<Self>) {
        match self.capture_menu {
            Some((open, highlighted)) if open == menu => self.capture_apply(menu, highlighted, cx),
            _ => self.capture_toggle(menu, cx),
        }
    }

    fn capture_step(&mut self, menu: CaptureMenu, delta: isize, cx: &mut Context<Self>) {
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

fn overview(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let configured = window.storage_projection_cache.projection();
    let pending = window.storage_projection_run.pending();
    let active = window.storage_projection_run.active();
    let run_projection = window.storage_projection_run.displayed();
    let projection = run_projection
        .map(|snapshot| snapshot.projection())
        .or(configured);
    let is_pending = pending.is_some();
    let is_active = active.is_some();
    let differs_from_next =
        active.is_some_and(|snapshot| !snapshot.matches_key(window.storage_projection_cache.key()));

    let status = if is_pending {
        "Starting with this estimate"
    } else if differs_from_next {
        "Active estimate · next settings differ"
    } else if is_active {
        "Active estimate · next settings match"
    } else {
        "Next recording"
    };

    let encoder = window
        .recording
        .encoder
        .clone()
        .unwrap_or_else(|| "Auto".to_owned());
    let encoder_tile = stat_tile(
        "Encoder",
        "H.264",
        format!(
            "{encoder} · {}",
            format_video_rate(projection.map_or(
                u64::from(window.config.video.bitrate_mbps) * 1_000_000,
                |projection| projection.video_bps,
            ))
        ),
    );

    let card = |tile: Div| tile.flex_grow().flex_basis(px(180.)).min_w(px(180.)).p_3();
    let keys = window
        .config
        .replay
        .hotkey
        .split('+')
        .map(|key| kbd(key.trim().to_owned()));

    let storage_tile = if let Some(projection) = projection {
        let sizes = projection.sizes;
        let mut tile = panel()
            .flex_1()
            .min_w_0()
            .p_4()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child("Storage estimate"),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("{} / min", format_decimal_size(sizes[1]))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(format!(
                        "30 s {} · 1 min {}",
                        format_decimal_size(sizes[0]),
                        format_decimal_size(sizes[1]),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(format!(
                        "2 min {} · 5 min {}",
                        format_decimal_size(sizes[2]),
                        format_decimal_size(sizes[3]),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(target_rate_description(projection)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(projection_assumptions(projection)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(status),
            );
        if is_active && differs_from_next {
            if let Some(next) = configured {
                tile = tile.child(div().text_xs().text_color(rgb(theme::TEXT_TERTIARY)).child(
                    format!(
                        "Next: {} · {} fps · {} · {} / min · replay {} s / {} RAM",
                        resolution_label(window.config.video.width, window.config.video.height),
                        window.config.video.fps,
                        format_video_rate(next.video_bps),
                        format_decimal_size(next.sizes[1]),
                        next.replay_duration_seconds,
                        format_mib(next.replay.ram_budget_bytes),
                    ),
                ));
            } else if let Some(error) = window.storage_projection_cache.error() {
                tile = tile.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(format!("Next estimate unavailable: {error}")),
                );
            }
        }
        tile
    } else {
        panel()
            .flex_1()
            .min_w_0()
            .p_4()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child("Storage estimate"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(
                        window
                            .storage_projection_cache
                            .error()
                            .unwrap_or("Unavailable")
                            .to_owned(),
                    ),
            )
    };

    let mode = match window.config.replay.mode {
        ReplayMode::Hybrid => "Hybrid",
    };
    let replay_details = if let Some(projection) = projection {
        let replay = projection.replay;
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .text_xs()
            .text_color(rgb(theme::TEXT_TERTIARY))
            .child(format!(
                "{} total · RAM budget {} · SSD {} theoretical",
                // Total and SSD are estimates from the same target-rate projection.
                format_decimal_size(replay.total_bytes),
                format_mib(replay.ram_budget_bytes),
                format_decimal_size(replay.estimated_ssd_bytes),
            ))
            .child(replay_assumptions(projection))
            .child("Replay engine unavailable in this build; no live buffer or disk usage is measured.")
    } else {
        div()
            .text_xs()
            .text_color(rgb(theme::TEXT_TERTIARY))
            .child("Estimate unavailable; replay engine is not available in this build.")
    };
    let replay_tile = stat_tile(
        "Replay buffer",
        format!(
            "{} s",
            projection.map_or(window.config.replay.duration_seconds.into(), |projection| {
                projection.replay_duration_seconds
            },)
        ),
        format!("{mode} · Save: {}", window.config.replay.hotkey),
    )
    .child(
        div()
            .pt_2()
            .flex()
            .items_center()
            .gap_2()
            .child(
                button(
                    "save-replay",
                    "Save replay",
                    ButtonVariant::Secondary,
                    None,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.send(RecorderCommand::SaveReplay);
                    cx.notify();
                })),
            )
            .child(div().flex().gap_1().children(keys)),
    )
    .child(replay_details);

    section("Configuration", None).child(
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(card(encoder_tile))
            .child(card(storage_tile))
            .child(card(replay_tile)),
    )
}

fn target_rate_description(projection: StorageProjection) -> String {
    let audio = if projection.audio_tracks == 0 {
        "no AAC tracks".to_owned()
    } else {
        let per_track = projection.audio_bps / u64::from(projection.audio_tracks);
        format!(
            "{} × {} AAC",
            projection.audio_tracks,
            format_audio_rate(per_track),
        )
    };
    format!(
        "{} video · {audio}",
        format_video_rate(projection.video_bps)
    )
}

fn projection_assumptions(projection: StorageProjection) -> &'static str {
    if projection.variable_bitrate {
        "VBR target · excludes container overhead"
    } else {
        "Target bitrate · excludes container overhead"
    }
}

fn replay_assumptions(projection: StorageProjection) -> &'static str {
    if projection.variable_bitrate {
        "At target video/AAC rate · VBR · no container overhead"
    } else {
        "At target video/AAC rate · no container overhead"
    }
}

fn format_video_rate(bits_per_second: u64) -> String {
    let mbps = bits_per_second as f64 / 1_000_000.0;
    if mbps.fract().abs() < 0.05 {
        format!("{mbps:.0} Mbps")
    } else {
        format!("{mbps:.1} Mbps")
    }
}

fn format_audio_rate(bits_per_second: u64) -> String {
    format!("{} kbps", bits_per_second / 1_000)
}

fn performance(window: &ClipperWindow) -> Div {
    let state = &window.recording;
    let perf = &window.config.performance;
    // Live stats while recording; otherwise the last finished run, if any.
    let (stats, note): (Option<&RecordingStats>, &'static str) = match state.phase {
        Phase::Recording => (Some(&state.stats), "Live"),
        _ => match &state.finished {
            Some(finished) => (Some(&finished.stats), "Last recording"),
            None => (None, "No recording yet"),
        },
    };
    let metric = |value: Option<String>| value_text(value.unwrap_or_else(|| "—".to_owned()));

    section("Performance", Some(note.into())).child(
        panel()
            .child(property_row(
                "Frame rate",
                None,
                metric(stats.map(|s| format!("{:.0} fps", s.current_fps))),
                true,
            ))
            .child(property_row(
                "Frames encoded",
                None,
                metric(stats.map(|s| s.frames_encoded.to_string())),
                false,
            ))
            .child(property_row(
                "Dropped frames",
                Some("Lost when the encoder falls behind.".into()),
                metric(stats.map(|s| s.frames_dropped.to_string())),
                false,
            ))
            .child(property_row(
                "Queues",
                Some("Command and event capacity.".into()),
                value_text(format!(
                    "{} / {}",
                    perf.command_queue_capacity, perf.event_queue_capacity
                )),
                false,
            )),
    )
}

fn recent_clips() -> Div {
    section("Recent clips", Some("Library is empty".into())).child(panel().child(empty_state(
        "◌",
        "Nothing to replay yet",
        "When clips are available, your latest highlights will appear here.",
    )))
}

impl ClipperWindow {
    fn start_recording(&mut self) {
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
        if self.send(RecorderCommand::StartRecording { source, options }) {
            self.storage_projection_run
                .start_accepted(self.storage_projection_cache.snapshot());
            self.recording.begin_start();
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
        if self.send(RecorderCommand::StartReplay { source, options }) {
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
        let enabled = match self.recording.replay.phase {
            ReplayPhase::Running { .. } => {
                self.stop_replay();
                false
            }
            ReplayPhase::Off if self.recording.can_start_replay() => self.start_replay(),
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
