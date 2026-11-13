use clipper_core::recording::{
    CaptureSource, QualityProfile, RecorderCommand, RecordingStats, SourceKind,
};
use clipper_storage::{AppConfig, VIDEO_FPS, VIDEO_RESOLUTIONS};
use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, div, px, rgb};

use super::ClipperWindow;
use crate::components::{ButtonKind, button, empty_state, list_row, pill};
use crate::state::{Phase, format_elapsed, resolution_label};
use crate::theme;

const RED: u32 = 0xe5484d;

pub(super) fn content(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(recording_panel(window, cx))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Recent clips"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::MUTED))
                        .child("Your library is empty"),
                ),
        )
        .child(empty_state(
            "Nothing to replay yet",
            "When clips are available, your latest highlights will appear here.",
            "◌",
        ))
}

fn section_label(text: &'static str) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::MUTED))
        .child(text)
}

fn recording_panel(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let mut panel = div()
        .w_full()
        .rounded_2xl()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
        .p_6()
        .flex()
        .flex_col()
        .gap_5();

    if let Some(message) = &state.error {
        panel = panel.child(error_banner(message.clone(), cx));
    }

    panel
        .child(control_row(window, cx))
        .child(status_row(window))
        .child(source_picker(window, cx))
        .child(settings_pills(window, cx))
}

fn error_banner(message: String, cx: &mut Context<ClipperWindow>) -> impl IntoElement {
    div()
        .id("error-banner")
        .w_full()
        .px_4()
        .py_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(RED))
        .bg(rgb(0x2a1517))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .on_click(cx.listener(|this, _, _, cx| {
            this.recording.dismiss_error();
            cx.notify();
        }))
        .child(div().flex_1().text_sm().child(message))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::MUTED))
                .child("Click to dismiss"),
        )
}

fn control_row(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let (label, kind, enabled) = match state.phase {
        Phase::Idle => ("Start recording", ButtonKind::Primary, state.can_start()),
        Phase::Starting => ("Starting...", ButtonKind::Primary, false),
        Phase::Recording => ("Stop recording", ButtonKind::Danger, state.can_stop()),
    };
    let recording = state.phase == Phase::Recording;
    let on_click = cx.listener(move |this, _, _, cx| {
        if recording {
            this.send(RecorderCommand::StopRecording);
        } else {
            this.start_recording();
        }
        cx.notify();
    });

    let mut row = div().flex().items_center().gap_4().child(button(
        "record-toggle",
        label,
        kind,
        enabled,
        on_click,
    ));
    if let Some(hint) = state.start_hint() {
        row = row.child(div().text_xs().text_color(rgb(theme::MUTED)).child(hint));
    }
    row
}

fn status_row(window: &ClipperWindow) -> Div {
    let state = &window.recording;
    let mut row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_5()
        .gap_y_2()
        .text_sm();

    match state.phase {
        Phase::Idle => {
            row = row.child(
                div()
                    .text_color(rgb(theme::MUTED))
                    .child(state.status_label()),
            );
            if let Some(finished) = &state.finished {
                row = row
                    .child(div().child(format!("Saved {}", finished.path.display())))
                    .child(stats_text(&finished.stats));
            }
        }
        Phase::Starting => {
            row = row.child(div().text_color(rgb(theme::MUTED)).child("Starting..."));
        }
        Phase::Recording => {
            row = row
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size(px(9.)).rounded_full().bg(rgb(RED)))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format_elapsed(state.stats.elapsed)),
                        ),
                )
                .child(stats_text(&state.stats));
            if let Some(encoder) = &state.encoder {
                row = row.child(
                    div()
                        .text_color(rgb(theme::MUTED))
                        .child(format!("Encoder: {encoder}")),
                );
            }
        }
    }
    row
}

fn stats_text(stats: &RecordingStats) -> Div {
    div().text_color(rgb(theme::MUTED)).child(format!(
        "{:.0} fps \u{b7} {} frames \u{b7} {} dropped",
        stats.current_fps, stats.frames_encoded, stats.frames_dropped
    ))
}

fn source_picker(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let state = &window.recording;
    let enabled = state.settings_enabled();
    let group = |kind: SourceKind, title: &'static str, cx: &mut Context<ClipperWindow>| {
        let rows: Vec<_> = state
            .sources
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| source_row(s, state.selected == Some((s.kind, s.id)), enabled, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_label(title))
            .children(rows)
    };

    let mut list = div()
        .id("source-list")
        .max_h(px(200.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3();
    list = if state.sources.is_empty() {
        list.child(
            div()
                .text_xs()
                .text_color(rgb(theme::MUTED))
                .child("No sources found. Try Refresh."),
        )
    } else {
        list.child(group(SourceKind::Monitor, "Displays", cx))
            .child(group(SourceKind::Window, "Windows", cx))
    };

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Source"),
                )
                .child(button(
                    "refresh-sources",
                    "Refresh",
                    ButtonKind::Subtle,
                    enabled,
                    cx.listener(|this, _, _, cx| {
                        this.send(RecorderCommand::RefreshSources);
                        cx.notify();
                    }),
                )),
        )
        .child(list)
}

fn source_row(
    source: &CaptureSource,
    selected: bool,
    enabled: bool,
    cx: &mut Context<ClipperWindow>,
) -> impl IntoElement + use<> {
    let (kind, id) = (source.kind, source.id);
    let prefix = match kind {
        SourceKind::Monitor => "mon",
        SourceKind::Window => "win",
    };
    list_row(
        (prefix, id),
        source.name.clone(),
        format!("{}x{}", source.width, source.height),
        selected,
        enabled,
        cx.listener(move |this, _, _, cx| {
            this.recording.select(kind, id);
            cx.notify();
        }),
    )
}

fn settings_pills(window: &ClipperWindow, cx: &mut Context<ClipperWindow>) -> Div {
    let enabled = window.recording.settings_enabled();
    let config: &AppConfig = &window.config;
    let current = (config.video.width, config.video.height);

    let resolutions = VIDEO_RESOLUTIONS.into_iter().map(|(w, h)| {
        pill(
            ("res", u64::from(h)),
            resolution_label(w, h),
            current == (w, h),
            enabled,
            cx.listener(move |this, _, _, cx| {
                this.config.video.width = w;
                this.config.video.height = h;
                this.persist_config(cx);
            }),
        )
    });
    let fps = VIDEO_FPS.into_iter().map(|value| {
        pill(
            ("fps", u64::from(value)),
            format!("{value} fps"),
            config.video.fps == value,
            enabled,
            cx.listener(move |this, _, _, cx| {
                this.config.video.fps = value;
                this.persist_config(cx);
            }),
        )
    });
    let quality = QualityProfile::ALL
        .into_iter()
        .enumerate()
        .map(|(index, profile)| {
            pill(
                ("quality", index as u64),
                profile.label(),
                QualityProfile::from(config.recording.quality) == profile,
                enabled,
                cx.listener(move |this, _, _, cx| {
                    this.config.recording.quality = profile.into();
                    this.persist_config(cx);
                }),
            )
        });

    let group = |title: &'static str| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(84.)).child(section_label(title)))
    };

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(group("RESOLUTION").children(resolutions))
        .child(group("FRAME RATE").children(fps))
        .child(group("QUALITY").children(quality))
}

impl ClipperWindow {
    fn start_recording(&mut self) {
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

    /// Saves the config off the UI thread; failures show up in the error banner.
    fn persist_config(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { config.save().map_err(|error| error.to_string()) })
                .await;
            if let Err(message) = result {
                this.update(cx, |this, cx| {
                    this.recording
                        .fail(format!("Could not save settings: {message}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}
