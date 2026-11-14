//! Left card of the Capture page: pick the capture source, see what the
//! engine is doing with it, and start or stop a manual recording.

use clipper_core::recording::{CaptureSource, RecorderCommand, SourceKind};
use gpui::prelude::*;
use gpui::{Context, Div, SharedString, Window, div, px};

use super::{ClipperWindow, Confirm, Dismiss, SelectNext, SelectPrevious};
use crate::components::{
    ButtonVariant, Decor, DropdownOption, Icon, Tone, button, button_with_icon, dropdown_decorated,
    icon, icon_card, icon_tile, status_dot, step, tooltip,
};
use crate::state::{Phase, ReplayPhase, format_elapsed};
use crate::theme::ActiveTheme;

/// How a source is captured. There is no game-specific backend.
fn type_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Monitor => "Desktop Capture",
        SourceKind::Window => "Window Capture",
    }
}

fn kind_noun(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Monitor => "Display",
        SourceKind::Window => "Window",
    }
}

/// Status line text and tone, derived only from engine state.
fn source_status(
    phase: Phase,
    replay: &ReplayPhase,
    selected: bool,
    source_name: Option<&str>,
    error: Option<&str>,
) -> (String, Tone) {
    let busy = match (phase, replay) {
        (Phase::Starting, _) | (_, ReplayPhase::Starting) => Some("Starting…"),
        (Phase::Stopping, _) | (_, ReplayPhase::Stopping) => Some("Stopping…"),
        _ => None,
    };
    if let Some(label) = busy {
        return (label.into(), Tone::Warning);
    }
    if phase == Phase::Recording || matches!(replay, ReplayPhase::Running { .. }) {
        return (
            format!("Capturing: {}", source_name.unwrap_or("source")),
            Tone::Success,
        );
    }
    if let Some(error) = error {
        let short: String = error.chars().take(60).collect();
        let ellipsis = if short.len() < error.len() { "…" } else { "" };
        return (format!("Error: {short}{ellipsis}"), Tone::Danger);
    }
    if selected && source_name.is_none() {
        return ("Source unavailable".into(), Tone::Warning);
    }
    ("Idle".into(), Tone::Neutral)
}

impl ClipperWindow {
    /// Dropdown rows and the selected row. A disabled placeholder row leads the
    /// list when nothing valid is selected.
    fn source_options(&self) -> (Vec<DropdownOption>, usize) {
        let state = &self.recording;
        let current = state.selected_source();
        let offset = usize::from(current.is_none());
        let mut options = Vec::with_capacity(state.sources.len() + offset);
        if current.is_none() {
            options.push(DropdownOption {
                label: "Choose a source".into(),
                detail: None,
                enabled: false,
                reason: None,
            });
        }
        options.extend(state.sources.iter().map(|s| DropdownOption {
            label: s.name.clone().into(),
            detail: Some(format!("{} · {}×{}", kind_noun(s.kind), s.width, s.height).into()),
            enabled: true,
            reason: None,
        }));
        let selected = current
            .and_then(|c| {
                state
                    .sources
                    .iter()
                    .position(|s| s.kind == c.kind && s.id == c.id)
            })
            .map_or(0, |i| i + offset);
        (options, selected)
    }

    fn source_toggle(&mut self, cx: &mut Context<Self>) {
        if self.source_menu.take().is_none() && self.recording.settings_enabled() {
            let (_, selected) = self.source_options();
            self.source_menu = Some(selected);
        }
        cx.notify();
    }

    fn source_close(&mut self, cx: &mut Context<Self>) {
        if self.source_menu.take().is_some() {
            cx.notify();
        }
    }

    fn source_apply(&mut self, index: usize, cx: &mut Context<Self>) {
        self.source_menu = None;
        let (options, _) = self.source_options();
        let offset = options.len() - self.recording.sources.len();
        let picked: Option<CaptureSource> = index
            .checked_sub(offset)
            .and_then(|i| self.recording.sources.get(i).cloned());
        if let Some(source) = picked
            && self.recording.settings_enabled()
        {
            self.recording.select(source.kind, source.id);
        }
        cx.notify();
    }

    fn source_confirm(&mut self, cx: &mut Context<Self>) {
        match self.source_menu {
            Some(highlighted) => self.source_apply(highlighted, cx),
            None => self.source_toggle(cx),
        }
    }

    fn source_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(from) = self.source_menu else {
            self.source_toggle(cx);
            return;
        };
        let (options, _) = self.source_options();
        let next = step(&options, from, delta);
        if next != from {
            self.source_menu = Some(next);
            cx.notify();
        }
    }

    /// Left card of the Capture page (capture.png): the selected source as a
    /// prominent panel (the source menu's trigger), then Change Source next
    /// to the manual Record button. Status and hints stay as short lines.
    pub(super) fn source_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let t = *cx.theme();
        let state = &self.recording;
        let enabled = state.settings_enabled();
        let muted = |text: SharedString| {
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .truncate()
                .child(text)
        };

        let refresh = div()
            .id("refresh-sources")
            .flex_none()
            .size(px(32.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(t.colors.text_secondary)
            .tooltip(tooltip("Refresh the list of displays and windows"))
            .child(icon(Icon::Refresh, px(15.)))
            .when(enabled, |this| {
                this.cursor_pointer()
                    .hover(|style| style.bg(t.colors.surface_hover).text_color(t.colors.text))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(RecorderCommand::RefreshSources);
                        cx.notify();
                    }))
            })
            .when(!enabled, |this| this.opacity(0.45));

        let current = state.selected_source();
        // The selected source, as the source menu's trigger.
        let panel = if state.sources.is_empty() {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_3()
                .rounded_lg()
                .border_1()
                .border_color(t.colors.border)
                .bg(t.colors.surface_raised)
                .child(icon_tile(&t, Icon::Monitor, px(40.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_base().child("No sources found"))
                        .child(muted("Open the game or window, then refresh.".into())),
                )
                .into_any_element()
        } else {
            let (options, selected) = self.source_options();
            let focus = &self.source_focus;
            let entity = cx.entity();
            let (on_toggle, on_select, on_dismiss) = (entity.clone(), entity.clone(), entity);
            let kind_icon = match current.map(|s| s.kind) {
                Some(SourceKind::Window) => Icon::Game,
                _ => Icon::Monitor,
            };
            let subtitle = current.map_or("No source selected", |s| type_label(s.kind));
            div()
                .relative()
                .w_full()
                .child(dropdown_decorated(
                    &t,
                    "source-menu",
                    &options,
                    selected,
                    self.source_menu,
                    focus,
                    focus.is_focused(window),
                    enabled,
                    Decor {
                        panel: true,
                        leading: Some(icon_tile(&t, kind_icon, px(40.)).into_any_element()),
                        subtitle: Some(subtitle.into()),
                        ..Decor::default()
                    },
                    move |_, app| on_toggle.update(app, |this, cx| this.source_toggle(cx)),
                    move |index, _, app| {
                        on_select.update(app, |this, cx| this.source_apply(index, cx))
                    },
                    move |_, app| on_dismiss.update(app, |this, cx| this.source_close(cx)),
                ))
                .on_action(cx.listener(|this, _: &Confirm, _, cx| this.source_confirm(cx)))
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.source_step(1, cx)))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.source_step(-1, cx)))
                .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.source_close(cx)))
                .into_any_element()
        };

        let (status, tone) = source_status(
            state.phase,
            &state.replay.phase,
            state.selected.is_some(),
            current.map(|s| s.name.as_str()),
            state.error.as_deref().or(state.replay.error.as_deref()),
        );
        let lock_note = if enabled {
            None
        } else if state.phase == Phase::Idle {
            Some("Stop instant replay to change the source.")
        } else {
            Some("Stop the recording to change the source.")
        };
        // One short line under the status: live recording stats, where the
        // last recording went, why the source is locked, or why recording
        // cannot start.
        let detail: Option<String> = match (&state.phase, &state.finished) {
            (Phase::Recording, _) => {
                let s = &state.stats;
                Some(format!(
                    "{} · {:.0} fps · {} dropped",
                    format_elapsed(s.elapsed),
                    s.current_fps,
                    s.frames_dropped
                ))
            }
            (Phase::Idle, Some(finished)) => Some(format!(
                "Recording saved: {}",
                finished
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )),
            _ => lock_note.map(str::to_owned),
        };

        let change = button_with_icon(
            &t,
            "change-source",
            icon(Icon::Monitor, px(15.)).into_any_element(),
            "Change Source",
            ButtonVariant::Secondary,
            enabled && !state.sources.is_empty(),
        )
        .flex_1()
        .min_w_0()
        .justify_center()
        .when_some(lock_note, |this, note| this.tooltip(tooltip(note)))
        .on_click(cx.listener(|this, _, window, cx| {
            window.focus(&this.source_focus);
            this.source_toggle(cx);
        }));

        // Manual recording keeps its own control next to Change Source.
        let (label, variant, button_enabled) = match state.phase {
            Phase::Idle => ("Record", ButtonVariant::Primary, state.can_start()),
            Phase::Starting => ("Starting…", ButtonVariant::Primary, false),
            Phase::Recording => ("Stop", ButtonVariant::Danger, state.can_stop()),
            Phase::Stopping => ("Saving…", ButtonVariant::Danger, false),
        };
        let stop = state.phase == Phase::Recording;
        let hint = (state.phase == Phase::Idle)
            .then(|| state.start_hint())
            .flatten()
            .unwrap_or(if stop {
                "Stop and save the recording"
            } else {
                "Record the selected source to a file"
            });
        let record = button(&t, "record-toggle", label, variant, None, button_enabled)
            .flex_none()
            .tooltip(tooltip(hint))
            .when(button_enabled, |this| {
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

        icon_card(
            &t,
            Icon::Game,
            "Capture Source",
            "Select what to capture.",
            Some(refresh.into_any_element()),
        )
        .child(panel)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .text_sm()
                        .child(status_dot(&t, tone))
                        .child(div().min_w_0().truncate().child(status)),
                )
                .children(detail.map(|text| muted(text.into()))),
        )
        // Bottom-aligned like the other two cards' bottom controls.
        // The bottom control's wrapper takes the card's spare height and
        // keeps the control at its end, so all three cards' bottom controls
        // line up (no extra spacer child: that over-measured the row).
        .child(
            div()
                .flex_grow()
                .flex()
                .flex_col()
                .justify_end()
                .child(div().flex().gap_2().child(change).child(record)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        phase: Phase,
        replay: ReplayPhase,
        sel: bool,
        name: Option<&str>,
        err: Option<&str>,
    ) -> (String, Tone) {
        source_status(phase, &replay, sel, name, err)
    }

    #[test]
    fn capturing_and_transitions() {
        let running = ReplayPhase::Running {
            encoder: "x".into(),
        };
        assert_eq!(
            status(Phase::Idle, running, true, Some("Main"), None),
            ("Capturing: Main".into(), Tone::Success)
        );
        assert_eq!(
            status(Phase::Recording, ReplayPhase::Off, true, Some("Main"), None).1,
            Tone::Success
        );
        assert_eq!(
            status(Phase::Starting, ReplayPhase::Off, true, None, None).0,
            "Starting…"
        );
        assert_eq!(
            status(Phase::Idle, ReplayPhase::Stopping, true, None, None).0,
            "Stopping…"
        );
    }

    #[test]
    fn unavailable_error_and_idle() {
        assert_eq!(
            status(Phase::Idle, ReplayPhase::Off, true, None, None),
            ("Source unavailable".into(), Tone::Warning)
        );
        assert_eq!(
            status(Phase::Idle, ReplayPhase::Off, true, Some("A"), Some("boom")),
            ("Error: boom".into(), Tone::Danger)
        );
        assert_eq!(
            status(Phase::Idle, ReplayPhase::Off, false, None, None),
            ("Idle".into(), Tone::Neutral)
        );
    }

    #[test]
    fn type_labels_never_say_game() {
        assert_eq!(type_label(SourceKind::Monitor), "Desktop Capture");
        assert_eq!(type_label(SourceKind::Window), "Window Capture");
    }
}
