mod clips;
mod dashboard;
mod editor;
mod settings;

use clipper_core::recording::{RecorderCommand, RecorderEvent, RecorderHandle};
use clipper_storage::AppConfig;
use gpui::prelude::*;
use gpui::{
    App, Context, Div, FocusHandle, FontWeight, KeyBinding, KeyDownEvent, Render, Task, Window,
    actions, div, px, rgb, rgba,
};

use crate::components::{Tone, badge, kbd, list_item, shortcut, status_dot};
use crate::state::{Phase, RecordingState};
use crate::theme;

actions!(
    clipper,
    [
        OpenDashboard,
        OpenClips,
        OpenEditor,
        OpenSettings,
        NextPage,
        PreviousPage,
        ToggleCommandPalette,
        FocusNext,
        FocusPrevious,
        SelectNext,
        SelectPrevious,
        Confirm,
        Dismiss,
    ]
);

const WINDOW_CONTEXT: &str = "ClipperWindow";
const PALETTE_CONTEXT: &str = "CommandPalette";

/// Registers the window's keyboard shortcuts. Call once at startup.
pub fn bind_keys(cx: &mut App) {
    let window = Some(WINDOW_CONTEXT);
    let palette = Some(PALETTE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("ctrl-1", OpenDashboard, window),
        KeyBinding::new("ctrl-2", OpenClips, window),
        KeyBinding::new("ctrl-3", OpenEditor, window),
        KeyBinding::new("ctrl-4", OpenSettings, window),
        KeyBinding::new("ctrl-tab", NextPage, window),
        KeyBinding::new("ctrl-shift-tab", PreviousPage, window),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, window),
        KeyBinding::new("tab", FocusNext, window),
        KeyBinding::new("shift-tab", FocusPrevious, window),
        KeyBinding::new("enter", Confirm, window),
        KeyBinding::new("down", SelectNext, palette),
        KeyBinding::new("up", SelectPrevious, palette),
        KeyBinding::new("enter", Confirm, palette),
        KeyBinding::new("escape", Dismiss, palette),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
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

    fn subtitle(self) -> &'static str {
        match self {
            Self::Dashboard => "Record, monitor and tune capture",
            Self::Clips => "Saved moments from your sessions",
            Self::Editor => "Trim and export a saved clip",
            Self::Settings => "How Clipper captures and stores",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Dashboard => "◧",
            Self::Clips => "▤",
            Self::Editor => "◫",
            Self::Settings => "◎",
        }
    }

    fn shortcut(self) -> &'static [&'static str] {
        match self {
            Self::Dashboard => &["Ctrl", "1"],
            Self::Clips => &["Ctrl", "2"],
            Self::Editor => &["Ctrl", "3"],
            Self::Settings => &["Ctrl", "4"],
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }

    fn offset(self, delta: isize) -> Self {
        let len = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + delta).rem_euclid(len) as usize]
    }
}

/// Command palette state. Commands are page navigation for now; capture actions
/// join the same list once a backend exists.
struct CommandPalette {
    focus: FocusHandle,
    query: String,
    selected: usize,
}

impl CommandPalette {
    fn matches(&self) -> Vec<Page> {
        let query = self.query.to_lowercase();
        Page::ALL
            .into_iter()
            .filter(|page| page.label().to_lowercase().contains(&query))
            .collect()
    }
}

/// Main desktop application shell.
pub struct ClipperWindow {
    handle: RecorderHandle,
    config: AppConfig,
    recording: RecordingState,
    /// Awaits recorder events; the UI re-renders only when one arrives.
    _events: Task<()>,
    active_page: Page,
    focus: FocusHandle,
    nav_focus: Vec<FocusHandle>,
    palette: Option<CommandPalette>,
}

impl ClipperWindow {
    pub fn new(
        handle: RecorderHandle,
        config: AppConfig,
        window: &mut Window,
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
        let focus = cx.focus_handle();
        window.focus(&focus);
        let nav_focus = (0..Page::ALL.len())
            .map(|index| cx.focus_handle().tab_index(index as isize).tab_stop(true))
            .collect();

        let mut this = Self {
            handle,
            config,
            recording: RecordingState::default(),
            _events: task,
            active_page: Page::Dashboard,
            focus,
            nav_focus,
            palette: None,
        };
        this.send(RecorderCommand::RefreshSources);
        this
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

    fn status_tone(&self) -> Tone {
        match self.recording.phase {
            Phase::Idle => Tone::Neutral,
            Phase::Starting => Tone::Warning,
            Phase::Recording | Phase::Stopping => Tone::Danger,
        }
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let palette_was_open = self.palette.take().is_some();
        if palette_was_open {
            window.focus(&self.focus);
        }
        if self.active_page != page || palette_was_open {
            self.active_page = page;
            cx.notify();
        }
    }

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_some() {
            window.focus(&self.focus);
        } else {
            let focus = cx.focus_handle();
            window.focus(&focus);
            self.palette = Some(CommandPalette {
                focus,
                query: String::new(),
                selected: 0,
            });
        }
        cx.notify();
    }

    fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            let len = palette.matches().len() as isize;
            if len > 0 {
                palette.selected = (palette.selected as isize + delta).rem_euclid(len) as usize;
                cx.notify();
            }
        }
    }

    fn confirm_palette(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let page = self
            .palette
            .as_ref()
            .and_then(|palette| palette.matches().get(palette.selected).copied());
        if let Some(page) = page {
            self.navigate(page, window, cx);
        }
    }

    fn dismiss_palette(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    /// Typing filters the palette. Navigation keys arrive as actions, not here.
    fn palette_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }

        let changed = if keystroke.key == "backspace" {
            palette.query.pop().is_some()
        } else if let Some(text) = keystroke
            .key_char
            .as_deref()
            .filter(|text| !text.chars().any(char::is_control))
        {
            palette.query.push_str(text);
            true
        } else {
            false
        };

        if changed {
            palette.selected = 0;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let navigation = Page::ALL
            .into_iter()
            .zip(&self.nav_focus)
            .map(|(page, focus)| {
                let focused = focus.is_focused(window);
                list_item(
                    ("nav", page.index()),
                    page.glyph(),
                    page.label(),
                    Some(page.shortcut()),
                    self.active_page == page,
                )
                .track_focus(focus)
                .border_1()
                .border_color(rgba(if focused { theme::FOCUS_RING } else { 0 }))
                .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
                .on_action(
                    cx.listener(move |this, _: &Confirm, window, cx| {
                        this.navigate(page, window, cx)
                    }),
                )
            });

        div()
            .w(px(232.))
            .h_full()
            .flex()
            .flex_col()
            .gap_4()
            .px_3()
            .py_4()
            .bg(rgb(theme::SIDEBAR))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(
                        div()
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(rgb(theme::ACCENT))
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme::TEXT_ON_ACCENT))
                            .child("C"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Clipper"),
                    ),
            )
            .child(
                div()
                    .id("open-command-palette")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::SURFACE))
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(theme::BORDER_STRONG)))
                    .text_sm()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_palette(&ToggleCommandPalette, window, cx)
                    }))
                    .child(div().flex_1().child("Search commands"))
                    .child(shortcut(&["Ctrl", "K"])),
            )
            .child(div().flex().flex_col().gap_0p5().children(navigation))
            .child(
                div()
                    .mt_auto()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_TERTIARY))
                    .child(status_dot(self.status_tone()))
                    .child(self.recording.status_label()),
            )
    }

    fn top_bar(&self) -> Div {
        let page = self.active_page;
        div()
            .h(px(56.))
            .flex_none()
            .px_8()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(page.label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_TERTIARY))
                            .child(page.subtitle()),
                    ),
            )
            .child(badge(self.recording.status_label(), self.status_tone()))
    }

    fn page_content(&self, cx: &mut Context<Self>) -> Div {
        match self.active_page {
            Page::Dashboard => dashboard::content(self, cx),
            Page::Clips => clips::content(),
            Page::Editor => editor::content(),
            Page::Settings => settings::content(&self.config),
        }
    }

    fn command_palette(&self, palette: &CommandPalette, cx: &mut Context<Self>) -> Div {
        let matches = palette.matches();
        let rows = matches.iter().enumerate().map(|(index, page)| {
            let page = *page;
            list_item(
                ("palette", index),
                page.glyph(),
                format!("Open {}", page.label()),
                Some(page.shortcut()),
                index == palette.selected,
            )
            .on_click(cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)))
        });

        let query = if palette.query.is_empty() {
            div()
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child("Search commands…")
        } else {
            div().child(palette.query.clone())
        };

        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(96.))
            .bg(rgba(theme::SCRIM))
            .child(
                // Backdrop: blocks clicks to the shell below and closes the palette.
                div()
                    .id("command-palette-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss_palette(&Dismiss, window, cx)
                    })),
            )
            .child(
                div()
                    .id("command-palette")
                    .key_context(PALETTE_CONTEXT)
                    .occlude()
                    .track_focus(&palette.focus)
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| {
                            this.move_palette_selection(1, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        this.move_palette_selection(-1, cx)
                    }))
                    .on_action(cx.listener(Self::confirm_palette))
                    .on_action(cx.listener(Self::dismiss_palette))
                    .on_key_down(cx.listener(Self::palette_key_down))
                    .w(px(520.))
                    .h_auto()
                    .flex()
                    .flex_col()
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(theme::BORDER_STRONG))
                    .bg(rgb(theme::SURFACE))
                    .shadow_lg()
                    .text_color(rgb(theme::TEXT))
                    .child(
                        div()
                            .h(px(48.))
                            .px_4()
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(rgb(theme::BORDER))
                            .text_sm()
                            .child(query),
                    )
                    .child(div().p_2().flex().flex_col().gap_0p5().children(rows).when(
                        matches.is_empty(),
                        |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_3()
                                    .text_sm()
                                    .text_color(rgb(theme::TEXT_TERTIARY))
                                    .child("No matching commands"),
                            )
                        },
                    ))
                    .child(
                        div()
                            .h(px(36.))
                            .px_4()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_4()
                            .border_t_1()
                            .border_color(rgb(theme::BORDER))
                            .text_xs()
                            .text_color(rgb(theme::TEXT_TERTIARY))
                            .child(hint(&["↑", "↓"], "Navigate"))
                            .child(hint(&["Enter"], "Open"))
                            .child(hint(&["Esc"], "Close")),
                    ),
            )
    }
}

fn hint(keys: &[&'static str], label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .children(keys.iter().map(|key| kbd(*key)))
        .child(label)
}

impl Render for ClipperWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self
            .palette
            .as_ref()
            .map(|palette| self.command_palette(palette, cx));

        div()
            .id("clipper-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenDashboard, window, cx| {
                this.navigate(Page::Dashboard, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenClips, window, cx| {
                    this.navigate(Page::Clips, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenEditor, window, cx| {
                this.navigate(Page::Editor, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.navigate(Page::Settings, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextPage, window, cx| {
                this.navigate(this.active_page.offset(1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousPage, window, cx| {
                this.navigate(this.active_page.offset(-1), window, cx)
            }))
            .on_action(cx.listener(Self::toggle_palette))
            // Tab must not move focus out of an open palette, or its keys stop reaching it.
            .on_action(cx.listener(|this, _: &FocusNext, window, _| {
                if this.palette.is_none() {
                    window.focus_next()
                }
            }))
            .on_action(cx.listener(|this, _: &FocusPrevious, window, _| {
                if this.palette.is_none() {
                    window.focus_prev()
                }
            }))
            .relative()
            .size_full()
            .flex()
            .bg(rgb(theme::BACKGROUND))
            .text_color(rgb(theme::TEXT))
            .child(self.sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.top_bar())
                    .child(
                        div().id("main-scroll").flex_1().overflow_y_scroll().child(
                            div()
                                .w_full()
                                .max_w(px(960.))
                                .mx_auto()
                                .px_8()
                                .py_6()
                                .child(self.page_content(cx)),
                        ),
                    ),
            )
            .children(palette)
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn page_offset_wraps_in_both_directions() {
        assert_eq!(Page::Dashboard.offset(1), Page::Clips);
        assert_eq!(Page::Settings.offset(1), Page::Dashboard);
        assert_eq!(Page::Dashboard.offset(-1), Page::Settings);
    }

    #[test]
    fn page_shortcuts_follow_navigation_order() {
        for (index, page) in Page::ALL.into_iter().enumerate() {
            assert_eq!(page.index(), index);
            assert_eq!(page.shortcut()[1], (index + 1).to_string());
        }
    }
}
