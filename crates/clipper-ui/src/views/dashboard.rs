use clipper_core::recording::{
    CaptureSource, QualityProfile, RecorderCommand, RecordingStats, SourceKind,
};
use clipper_storage::config::ReplayMode;
use clipper_storage::storage_projection::StorageProjection;
use clipper_storage::{VIDEO_FPS, VIDEO_RESOLUTIONS};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, px, rgb};

use super::ClipperWindow;
use crate::components::{
    ButtonVariant, Tone, button, chip, empty_state, list_item, panel, property_row, section_header,
    stat_tile, status_dot, value_text,
};
use crate::state::{Phase, format_decimal_size, format_elapsed, format_mib, resolution_label};
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
        .child(recording(window, cx))
        .child(sources(window, cx))
        .child(capture_settings(window, cx))
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

    let bitrate = [8u32, 16, 24, 32, 50].into_iter().map(|value| {
        chip(
            ("bitrate", value as usize),
            format!("{value} Mbps"),
            config.video.bitrate_mbps == value,
            enabled,
        )
        .when(enabled, |this| {
            this.on_click(cx.listener(move |this, _, _, cx| {
                this.config.video.bitrate_mbps = value;
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
            ))
            .child(property_row(
                "Base bitrate",
                Some("The quality preset adjusts the encoded target used by estimates.".into()),
                chips(bitrate.collect()),
                false,
            )),
    )
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
        div().pt_2().child(
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
        ),
    )
    .child(replay_details);

    section("Configuration", None).child(
        div()
            .flex()
            .gap_3()
            .child(encoder_tile)
            .child(storage_tile)
            .child(replay_tile),
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
}
