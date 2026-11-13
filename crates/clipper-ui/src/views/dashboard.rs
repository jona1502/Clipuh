use clipper_core::recording::{
    CaptureSource, QualityProfile, RecorderCommand, RecordingStats, SourceKind,
};
use clipper_storage::config::ReplayMode;
use clipper_storage::{VIDEO_FPS, VIDEO_RESOLUTIONS};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, px, rgb};

use super::ClipperWindow;
use crate::components::{
    ButtonVariant, Tone, button, chip, empty_state, list_item, panel, property_row, section_header,
    stat_tile, status_dot, switch, value_text,
};
use crate::state::{
    Phase, ReplayPhase, format_elapsed, format_megabytes, resolution_label, tier_label,
};
use crate::theme;

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
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
        .child(capture_settings(window, cx))
        .child(overview(window))
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

fn capture_settings(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let enabled = window.recording.settings_enabled();
    let config = &window.config;
    let current = (config.video.width, config.video.height);

    let resolutions = VIDEO_RESOLUTIONS.into_iter().map(|(w, h)| {
        chip(
            ("res", h),
            resolution_label(w, h),
            current == (w, h),
            enabled,
        )
        .when(enabled, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                this.config.video.width = w;
                this.config.video.height = h;
                this.persist_config(cx);
            }))
        })
    });
    let fps = VIDEO_FPS.into_iter().map(|value| {
        chip(
            ("fps", value),
            format!("{value} fps"),
            config.video.fps == value,
            enabled,
        )
        .when(enabled, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                this.config.video.fps = value;
                this.persist_config(cx);
            }))
        })
    });
    let current_quality = QualityProfile::from(config.recording.quality);
    let quality = QualityProfile::ALL
        .into_iter()
        .enumerate()
        .map(|(index, profile)| {
            chip(
                ("quality", index),
                profile.label(),
                current_quality == profile,
                enabled,
            )
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.config.recording.quality = profile.into();
                    this.persist_config(cx);
                }))
            })
        });

    let chips = |items: Vec<gpui::Stateful<Div>>| div().flex().gap_1p5().children(items);
    let note = (!enabled).then(|| "Locked while recording".into());

    section("Capture settings", note).child(
        panel()
            .child(property_row(
                "Resolution",
                None,
                chips(resolutions.collect()),
                true,
            ))
            .child(property_row(
                "Frame rate",
                None,
                chips(fps.collect()),
                false,
            ))
            .child(property_row(
                "Quality",
                None,
                chips(quality.collect()),
                false,
            )),
    )
}

fn overview(window: &ClipperWindow) -> Div {
    let config = &window.config;
    let video = &config.video;
    let mode = match config.replay.mode {
        ReplayMode::Hybrid => "Hybrid",
    };
    let encoder = window
        .recording
        .encoder
        .clone()
        .unwrap_or_else(|| "Auto".to_owned());
    let mb_per_minute = video.bitrate_mbps * 60 / 8;

    section("Configuration", None).child(
        div()
            .flex()
            .gap_3()
            .child(stat_tile(
                "Encoder",
                "H.264",
                format!("{encoder} · {} Mbps", video.bitrate_mbps),
            ))
            .child(stat_tile(
                "Storage",
                format!("~{mb_per_minute} MB/min"),
                "Estimate from bitrate",
            ))
            .child(stat_tile(
                "Replay buffer",
                format!("{} s", config.replay.duration_seconds),
                format!("{mode} · Save: {}", config.replay.hotkey),
            )),
    )
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
