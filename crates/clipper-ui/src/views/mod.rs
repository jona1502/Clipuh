mod clips;
mod dashboard;
mod editor;
mod settings;

use clipper_core::recording::{RecorderCommand, RecorderEvent, RecorderHandle};
use clipper_storage::AppConfig;
use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, Render, Task, Window, div, px, rgb};

use crate::components::navigation_item;
use crate::state::{Phase, RecordingState};
use crate::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Dashboard,
    Clips,
    Editor,
    Settings,
}

impl Page {
    const ALL: [Self; 4] = [Self::Dashboard, Self::Clips, Self::Editor, Self::Settings];

    fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Clips => "Clips",
            Self::Editor => "Editor",
            Self::Settings => "Settings",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dashboard => "⌂",
            Self::Clips => "▤",
            Self::Editor => "✦",
            Self::Settings => "⚙",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Dashboard => "nav-dashboard",
            Self::Clips => "nav-clips",
            Self::Editor => "nav-editor",
            Self::Settings => "nav-settings",
        }
    }
}

/// Main desktop application shell.
pub struct ClipperWindow {
    active_page: Page,
    handle: RecorderHandle,
    config: AppConfig,
    recording: RecordingState,
    /// Awaits recorder events; the UI re-renders only when one arrives.
    _events: Task<()>,
}

impl ClipperWindow {
    pub fn new(
        handle: RecorderHandle,
        config: AppConfig,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = handle.events();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let updated = this.update(cx, |this, cx| {
                    this.recording.apply(event);
                    cx.notify();
                });
                if updated.is_err() {
                    return;
                }
            }
            // Channel closed without a final event: the worker is gone.
            let _ = this.update(cx, |this, cx| {
                if this.recording.phase != Phase::Idle {
                    this.recording.apply(RecorderEvent::Stopped);
                    cx.notify();
                }
            });
        });
        let mut window = Self {
            active_page: Page::default(),
            handle,
            config,
            recording: RecordingState::default(),
            _events: task,
        };
        window.send(RecorderCommand::RefreshSources);
        window
    }

    /// Queues a command; a full or closed queue surfaces as an error banner.
    fn send(&mut self, command: RecorderCommand) -> bool {
        match self.handle.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.recording.fail(error.to_string());
                false
            }
        }
    }
}

impl Render for ClipperWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::TEXT))
            .child(self.sidebar(cx))
            .child(self.main_panel(cx))
    }
}

impl ClipperWindow {
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let navigation = Page::ALL.into_iter().map(|page| {
            let active = self.active_page == page;
            let on_click = cx.listener(move |this, _, _, cx| {
                if this.active_page != page {
                    this.active_page = page;
                    cx.notify();
                }
            });

            navigation_item(page.id(), page.glyph(), page.label(), active, on_click)
        });

        div()
            .w(px(248.))
            .h_full()
            .flex()
            .flex_col()
            .px_4()
            .py_5()
            .gap_8()
            .bg(rgb(theme::SIDEBAR))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .bg(rgb(theme::ACCENT))
                            .text_color(rgb(0xffffff))
                            .font_weight(FontWeight::BOLD)
                            .child("C"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::BOLD)
                                    .child("CLIPPER"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme::MUTED))
                                    .child("GAME HIGHLIGHTS"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .pb_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::MUTED))
                            .child("WORKSPACE"),
                    )
                    .children(navigation),
            )
            .child(
                div()
                    .mt_auto()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::SURFACE))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("LOCAL WORKSPACE"),
                    )
                    .child(div().text_xs().text_color(rgb(theme::MUTED)).child(
                        match self.recording.phase {
                            Phase::Idle => "Ready to record. Clip storage is not connected yet.",
                            Phase::Starting => "Starting a recording...",
                            Phase::Recording => "Recording in progress.",
                            Phase::Stopping => "Saving the recording...",
                        },
                    )),
            )
    }

    fn main_panel(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .child(self.top_bar())
            .child(
                div()
                    .id("main-scroll")
                    .flex_1()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(1080.))
                            .mx_auto()
                            .px_8()
                            .pt_8()
                            .pb_10()
                            .flex()
                            .flex_col()
                            .gap_7()
                            .child(self.page_heading())
                            .child(self.page_content(cx)),
                    ),
            )
    }

    fn top_bar(&self) -> Div {
        div()
            .h(px(68.))
            .w_full()
            .px_8()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(theme::MUTED))
                    .child("CLIPPER")
                    .child(div().text_color(rgb(theme::BORDER)).child("/"))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT))
                            .child(self.active_page.label()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_full()
                    .px_3()
                    .py_2()
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::SURFACE))
                    .child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .bg(rgb(match self.recording.phase {
                                Phase::Idle => 0x78869a,
                                Phase::Starting => 0xe0b04a,
                                Phase::Recording | Phase::Stopping => 0xe5484d,
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::MUTED))
                            .child(self.recording.status_label()),
                    ),
            )
    }

    fn page_heading(&self) -> Div {
        let (eyebrow, title, subtitle) = match self.active_page {
            Page::Dashboard => (
                "OVERVIEW",
                "Your highlight workspace",
                "A calm home for the moments worth keeping.",
            ),
            Page::Clips => (
                "LIBRARY",
                "Your clips",
                "Browse saved moments from your sessions.",
            ),
            Page::Editor => (
                "CREATION",
                "Clip editor",
                "Shape a saved moment into something worth sharing.",
            ),
            Page::Settings => (
                "PREFERENCES",
                "Settings",
                "Set up how Clipper fits into your workflow.",
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme::ACCENT))
                    .child(eyebrow),
            )
            .child(div().text_3xl().font_weight(FontWeight::BOLD).child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme::MUTED))
                    .child(subtitle),
            )
    }

    fn page_content(&self, cx: &mut Context<Self>) -> Div {
        match self.active_page {
            Page::Dashboard => dashboard::content(self, cx),
            Page::Clips => clips::content(),
            Page::Editor => editor::content(),
            Page::Settings => settings::content(&self.config),
        }
    }
}
