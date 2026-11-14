//! The global capture bar: one row above every page with the capture source,
//! resolution, frame rate, replay length, Save Clip hotkey and the instant
//! replay switch. Every control edits the same config and recorder state as
//! the rest of the app; nothing here keeps its own copy.

use clipper_core::recording::SourceKind;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Stateful, Window, div, px};

use super::{CaptureMenu, ClipperWindow, Confirm, Dismiss, Page, SelectNext, SelectPrevious};
use crate::components::{Icon, dropdown_ghost, icon, icon_tile, toggle, tooltip, vertical_divider};
use crate::state::{Phase, ReplayPhase};
use crate::theme::{ActiveTheme, Theme};

/// Height of the bar; tall enough for the source tile and two-line label.
pub(super) const BAR_HEIGHT: f32 = 72.0;
/// Narrowest a dropdown slot gets; slots share the remaining width.
const MENU_WIDTH: f32 = 84.0;
/// Horizontal inset of the bar and of page content from the window edges.
pub(super) const PAGE_GUTTER: f32 = 24.0;

/// What the bar shows for the capture source.
#[derive(Debug, PartialEq, Eq)]
struct SourceLabel {
    name: String,
    method: &'static str,
    /// The previously selected window or monitor is gone.
    missing: bool,
}

fn source_label(selected: Option<(SourceKind, &str)>, was_selected: bool) -> SourceLabel {
    match selected {
        Some((kind, name)) => SourceLabel {
            name: name.to_owned(),
            method: match kind {
                SourceKind::Monitor => "Desktop Capture",
                SourceKind::Window => "Window Capture",
            },
            missing: false,
        },
        None if was_selected => SourceLabel {
            name: "Source unavailable".to_owned(),
            method: "Choose another on Capture",
            missing: true,
        },
        None => SourceLabel {
            name: "No source".to_owned(),
            method: "Choose one on Capture",
            missing: false,
        },
    }
}

/// Label and tone of the replay switch for each engine phase.
fn replay_label(phase: &ReplayPhase, failed: bool) -> &'static str {
    match phase {
        ReplayPhase::Off if failed => "Replay failed",
        ReplayPhase::Off => "Replay off",
        ReplayPhase::Starting => "Starting…",
        ReplayPhase::Running { .. } => "Replay active",
        ReplayPhase::Stopping => "Stopping…",
    }
}

impl ClipperWindow {
    pub(super) fn capture_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = *cx.theme();
        let menus = [
            CaptureMenu::Resolution,
            CaptureMenu::FrameRate,
            CaptureMenu::ReplayDuration,
        ]
        .map(|menu| self.bar_menu(&t, menu, window, cx));

        // A rounded card above every page, controls spread evenly between
        // dividers like the references.
        div()
            .id("capture-bar")
            .h(px(BAR_HEIGHT))
            .flex_none()
            .mx(px(PAGE_GUTTER))
            .mt_4()
            .px_3()
            .flex()
            .flex_row()
            .flex_nowrap()
            .items_center()
            .gap(t.metrics.gap)
            .rounded_xl()
            .border_1()
            .border_color(t.colors.border)
            .bg(t.colors.surface)
            .child(self.bar_source(&t, cx))
            .child(vertical_divider(&t).h(px(36.)))
            .children(menus.into_iter().flat_map(|menu| {
                [
                    menu.into_any_element(),
                    vertical_divider(&t).h(px(36.)).into_any_element(),
                ]
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .justify_center()
                    .child(self.hotkey_bar_control(&t, window, cx)),
            )
            .child(vertical_divider(&t).h(px(36.)))
            .child(self.bar_replay(&t, window, cx))
    }

    fn bar_source(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self
            .recording
            .selected_source()
            .map(|source| (source.kind, source.name.as_str()));
        let label = source_label(selected, self.recording.selected.is_some());
        // A manual recording shows where the source is, in recording red, so
        // it stays visible on every page.
        let recording = self.recording.phase != Phase::Idle;
        let method = if recording {
            self.recording.status_label()
        } else {
            label.method
        };
        let kind_icon = match selected.map(|(kind, _)| kind) {
            Some(SourceKind::Window) => Icon::Game,
            _ => Icon::Monitor,
        };
        div()
            .id("capture-bar-source")
            .flex_shrink()
            .min_w(px(110.))
            .max_w(px(300.))
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_1()
            .rounded_lg()
            .cursor_pointer()
            .hover(|style| style.bg(t.colors.surface_hover))
            .tooltip(tooltip("Choose the capture source on the Capture page"))
            .on_click(cx.listener(|this, _, window, cx| this.navigate(Page::Dashboard, window, cx)))
            .child(icon_tile(t, kind_icon, px(44.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if label.missing {
                                t.colors.warning
                            } else {
                                t.colors.text
                            })
                            .truncate()
                            .child(label.name),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if recording {
                                t.colors.recording
                            } else {
                                t.colors.text_muted
                            })
                            .truncate()
                            .child(method),
                    ),
            )
            .child(icon(Icon::ChevronDown, px(12.)).text_color(t.colors.text_muted))
            .into_any_element()
    }

    fn bar_menu(
        &self,
        t: &Theme,
        menu: CaptureMenu,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let options = self.capture_options(menu);
        let selected = self.capture_selected(menu);
        let focus = &self.capture_focus[menu as usize];
        let highlighted = self
            .capture_menu
            .filter(|(open, _)| *open == menu)
            .map(|(_, highlighted)| highlighted);
        let pending = self.capture_pending(menu);
        let entity = cx.entity();
        let (on_toggle, on_select, on_dismiss) = (entity.clone(), entity.clone(), entity);
        let trigger = dropdown_ghost(
            t,
            ("capture-bar-menu", menu as usize),
            &options,
            selected,
            highlighted,
            focus,
            focus.is_focused(window),
            true,
            move |_, app| on_toggle.update(app, |this, cx| this.capture_toggle(menu, cx)),
            move |index, _, app| {
                on_select.update(app, |this, cx| this.capture_apply(menu, index, cx))
            },
            move |_, app| on_dismiss.update(app, |this, cx| this.capture_close(cx)),
        );
        div()
            .id(("capture-bar-slot", menu as usize))
            .relative()
            .flex_1()
            .min_w(px(MENU_WIDTH))
            .child(trigger)
            // A changed resolution or frame rate only takes effect when replay
            // or recording restarts; say so instead of implying it is live.
            .tooltip(tooltip(if pending {
                format!("{} applies when replay or recording restarts", menu.label())
            } else {
                menu.label().to_owned()
            }))
            .when(pending, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .right(px(-3.))
                        .size(px(8.))
                        .rounded_full()
                        .bg(t.colors.warning),
                )
            })
            .on_action(cx.listener(move |this, _: &Confirm, _, cx| this.capture_confirm(menu, cx)))
            .on_action(
                cx.listener(move |this, _: &SelectNext, _, cx| this.capture_step(menu, 1, cx)),
            )
            .on_action(
                cx.listener(move |this, _: &SelectPrevious, _, cx| this.capture_step(menu, -1, cx)),
            )
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.capture_close(cx)))
            .into_any_element()
    }

    fn bar_replay(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let replay = &self.recording.replay;
        let failed = replay.error.is_some();
        let label = replay_label(&replay.phase, failed);
        let busy = matches!(replay.phase, ReplayPhase::Starting | ReplayPhase::Stopping);
        // The switch shows what the engine reports, never a local guess.
        let on = matches!(
            replay.phase,
            ReplayPhase::Starting | ReplayPhase::Running { .. }
        );
        let can_start = self.recording.can_start_replay();
        let enabled = !busy && (on || can_start);
        let hint: SharedString = match (&replay.error, enabled, on) {
            (Some(error), _, _) => error.clone().into(),
            (None, false, false) if !busy => self
                .recording
                .replay_hint()
                .unwrap_or("Instant replay can't start right now")
                .into(),
            _ => "Instant replay keeps the last moments ready to save".into(),
        };
        let this = cx.entity().downgrade();
        div()
            .id("capture-bar-replay")
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .tooltip(tooltip(hint))
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .text_color(if failed {
                        t.colors.danger
                    } else if replay.is_running() {
                        t.colors.success
                    } else if on {
                        t.colors.text
                    } else {
                        t.colors.text_secondary
                    })
                    .child(label),
            )
            .child(toggle(
                t,
                "capture-bar-replay-switch",
                on,
                enabled,
                &self.replay_focus,
                self.replay_focus.is_focused(window),
                move |_, _, cx| {
                    this.update(cx, |this, cx| this.toggle_replay(cx)).ok();
                },
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_label_names_the_capture_method() {
        let desktop = source_label(Some((SourceKind::Monitor, "Desktop 1")), true);
        assert_eq!(desktop.name, "Desktop 1");
        assert_eq!(desktop.method, "Desktop Capture");
        assert!(!desktop.missing);
        let game = source_label(Some((SourceKind::Window, "VALORANT")), true);
        assert_eq!(game.method, "Window Capture");
    }

    #[test]
    fn source_label_reports_missing_and_empty_selection() {
        assert!(source_label(None, true).missing);
        let none = source_label(None, false);
        assert_eq!(none.name, "No source");
        assert!(!none.missing);
    }

    #[test]
    fn replay_label_follows_engine_phase() {
        assert_eq!(replay_label(&ReplayPhase::Off, false), "Replay off");
        assert_eq!(replay_label(&ReplayPhase::Off, true), "Replay failed");
        assert_eq!(replay_label(&ReplayPhase::Starting, false), "Starting…");
        let running = ReplayPhase::Running {
            encoder: "NVENC".into(),
        };
        assert_eq!(replay_label(&running, false), "Replay active");
        assert_eq!(replay_label(&ReplayPhase::Stopping, false), "Stopping…");
    }
}
