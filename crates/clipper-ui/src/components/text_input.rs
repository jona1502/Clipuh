//! Single-line text input entity.
//!
//! Limitations (shortcut: kept tiny on purpose, upgrade if a real editor is needed): no
//! cursor movement, selection or IME; typing appends and Backspace deletes at the end only.
//! Ctrl+V pastes the first clipboard line. Enter emits `Submitted` and Escape blurs, both via
//! actions in the "TextInput" key context (`components::bind_keys`) because bindings run
//! before `on_key_down`.

use gpui::prelude::*;
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, SharedString, Window, div, px,
};

use super::{Dismiss, Submit};
use crate::theme::ActiveTheme;

const MAX_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputEvent {
    Changed(String),
    Submitted(String),
    /// Escape: the owner should move focus elsewhere.
    Dismissed,
}

pub struct TextInput {
    text: String,
    placeholder: SharedString,
    error: Option<SharedString>,
    disabled: bool,
    focus: FocusHandle,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Appends printable `typed` while staying within `max` chars. Returns whether text changed.
fn push_str(text: &mut String, typed: &str, max: usize) -> bool {
    let mut changed = false;
    for c in typed.chars().filter(|c| !c.is_control()) {
        if text.chars().count() >= max {
            break;
        }
        text.push(c);
        changed = true;
    }
    changed
}

/// Removes the last char. Returns whether text changed.
fn backspace(text: &mut String) -> bool {
    text.pop().is_some()
}

/// Pastes only the first line of `clip`, trimmed of the line break.
fn paste(text: &mut String, clip: &str, max: usize) -> bool {
    push_str(text, clip.lines().next().unwrap_or(""), max)
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            placeholder: SharedString::default(),
            error: None,
            disabled: false,
            focus: cx.focus_handle().tab_stop(true),
        }
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Position in the window's Tab order.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.focus = self.focus.tab_index(index);
        self
    }

    // shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
    #[allow(dead_code)]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Programmatic update; does not emit `Changed`. Clears any error, since
    /// the caller supplies a known value.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = text.chars().take(MAX_LEN).collect();
        self.error = None;
        cx.notify();
    }

    pub fn set_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.error = error;
        cx.notify();
    }

    // shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
    #[allow(dead_code)]
    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        cx.notify();
    }

    // shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
    #[allow(dead_code)]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Changed(self.text.clone()));
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let k = &event.keystroke;
        let m = k.modifiers;
        if m.control && k.key == "v" {
            let clip = cx.read_from_clipboard().and_then(|c| c.text());
            if clip.is_some_and(|c| paste(&mut self.text, &c, MAX_LEN)) {
                self.changed(cx);
            }
        } else if k.key == "backspace" {
            if backspace(&mut self.text) {
                self.changed(cx);
            }
        } else if !m.control
            && !m.alt
            && !m.platform
            && !m.function
            && let Some(ch) = &k.key_char
            && push_str(&mut self.text, ch, MAX_LEN)
        {
            self.changed(cx);
        }
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *cx.theme();
        let focused = self.focus.is_focused(window) && !self.disabled;
        let border = if self.error.is_some() {
            t.colors.danger
        } else if focused {
            t.colors.focus_ring
        } else {
            t.colors.border
        };
        let shown = if self.text.is_empty() {
            div()
                .text_color(t.colors.text_muted)
                .child(self.placeholder.clone())
        } else {
            div().child(self.text.clone())
        };
        div()
            .id("text-input")
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(t.metrics.control_height)
                    .px(t.metrics.control_padding)
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(t.colors.surface_raised)
                    .text_sm()
                    .text_color(t.colors.text)
                    .when(self.disabled, |this| this.opacity(0.45))
                    .when(!self.disabled, |this| this.cursor_text())
                    .child(shown)
                    .when(focused, |this| {
                        this.child(div().text_color(t.colors.accent).child("|"))
                    }),
            )
            .children(self.error.clone().map(|e| {
                div()
                    .text_xs()
                    .text_color(t.colors.danger)
                    .max_w(px(360.))
                    .child(e)
            }))
            .key_context("TextInput")
            .track_focus(&self.focus)
            .when(!self.disabled, |this| {
                this.on_click(cx.listener(|this, _, window, _| window.focus(&this.focus)))
                    .on_key_down(cx.listener(Self::on_key))
                    .on_action(cx.listener(|this, _: &Submit, _, cx| {
                        cx.emit(TextInputEvent::Submitted(this.text.clone()));
                    }))
                    // The owner moves focus back to its own context; a bare blur
                    // would leave the window with no key context at all.
                    .on_action(
                        cx.listener(|_, _: &Dismiss, _, cx| cx.emit(TextInputEvent::Dismissed)),
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_respects_max_and_skips_control() {
        let mut s = String::from("ab");
        assert!(push_str(&mut s, "cde", 4));
        assert_eq!(s, "abcd");
        assert!(!push_str(&mut s, "x", 4));
        assert!(!push_str(&mut s, "\n", 10));
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut s = String::new();
        assert!(!backspace(&mut s));
        let mut s = String::from("é");
        assert!(backspace(&mut s) && s.is_empty());
    }

    #[test]
    fn paste_takes_first_line() {
        let mut s = String::new();
        assert!(paste(&mut s, "#9B5CF6\nsecond", 64));
        assert_eq!(s, "#9B5CF6");
        assert!(!paste(&mut s, "", 64));
    }
}
