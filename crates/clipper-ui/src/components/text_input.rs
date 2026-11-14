//! Single-line text input entity.
//!
//! Limitations (shortcut: kept tiny on purpose, upgrade if a real editor is needed): no
//! cursor movement, selection or IME; typing appends and Backspace deletes at the end only.
//! Ctrl+V pastes the first clipboard line. Enter emits `Submitted` and Escape blurs, both via
//! actions in the "TextInput" key context (`components::bind_keys`) because bindings run
//! before `on_key_down`.
//! Compact numeric editors additionally support select-all replacement (Ctrl+A).

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
    max_len: usize,
    allowed: fn(char) -> bool,
    focus: FocusHandle,
    leading_icon: Option<super::RasterIcon>,
    selected_all: bool,
    compact: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Every printable character (the default filter).
fn any_char(_: char) -> bool {
    true
}

/// Appends printable `typed` chars that pass `allowed` while staying within
/// `max` chars. Returns whether text changed.
fn push_str(text: &mut String, typed: &str, max: usize, allowed: fn(char) -> bool) -> bool {
    let mut changed = false;
    for c in typed.chars().filter(|c| !c.is_control() && allowed(*c)) {
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
fn paste(text: &mut String, clip: &str, max: usize, allowed: fn(char) -> bool) -> bool {
    push_str(text, clip.lines().next().unwrap_or(""), max, allowed)
}

fn replace_selection(
    text: &mut String,
    selected: &mut bool,
    typed: &str,
    max: usize,
    allowed: fn(char) -> bool,
) -> bool {
    if !typed.chars().any(|c| !c.is_control() && allowed(c)) {
        return false;
    }
    if std::mem::take(selected) {
        text.clear();
    }
    paste(text, typed, max, allowed)
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            placeholder: SharedString::default(),
            error: None,
            disabled: false,
            max_len: MAX_LEN,
            allowed: any_char,
            focus: cx.focus_handle().tab_stop(true),
            leading_icon: None,
            selected_all: false,
            compact: false,
        }
    }

    /// Draws an approved icon (muted) before the text, e.g. a search glass.
    pub fn leading_icon(mut self, icon: super::RasterIcon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    /// Fixed-height inline numeric field, without the regular form padding.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected_all = !self.text.is_empty();
        cx.notify();
    }

    /// Allows longer file names without changing the default for other fields.
    /// Only characters passing `allowed` can be typed or pasted (others are
    /// dropped), e.g. hex digits for a colour field.
    pub fn allowed_chars(mut self, allowed: fn(char) -> bool) -> Self {
        self.allowed = allowed;
        self
    }

    pub fn max_len(mut self, max: usize) -> Self {
        self.max_len = max.clamp(1, 255);
        self
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
        self.text = text.chars().take(self.max_len).collect();
        self.selected_all = false;
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
        if m.control && k.key == "a" {
            self.select_all(cx);
        } else if m.control && k.key == "v" {
            let clip = cx.read_from_clipboard().and_then(|c| c.text());
            if clip.is_some_and(|c| {
                replace_selection(
                    &mut self.text,
                    &mut self.selected_all,
                    c.lines().next().unwrap_or(""),
                    self.max_len,
                    self.allowed,
                )
            }) {
                self.changed(cx);
            }
        } else if k.key == "backspace" || (k.key == "delete" && self.selected_all) {
            if std::mem::take(&mut self.selected_all) {
                self.text.clear();
                self.changed(cx);
            } else if backspace(&mut self.text) {
                self.changed(cx);
            }
        } else if !m.control
            && !m.alt
            && !m.platform
            && !m.function
            && let Some(ch) = &k.key_char
            && replace_selection(
                &mut self.text,
                &mut self.selected_all,
                ch,
                self.max_len,
                self.allowed,
            )
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
            div()
                .when(self.selected_all && focused, |this| {
                    this.bg(t.colors.accent)
                })
                .child(self.text.clone())
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
                    .h(if self.compact {
                        px(24.)
                    } else {
                        t.metrics.control_height
                    })
                    .px(if self.compact {
                        px(3.)
                    } else {
                        t.metrics.control_padding
                    })
                    .min_w_0()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .bg(t.colors.surface_raised)
                    .text_sm()
                    .text_color(t.colors.text)
                    .when(self.disabled, |this| this.opacity(0.45))
                    .when(!self.disabled, |this| this.cursor_text())
                    .when_some(self.leading_icon, |this, icon| {
                        this.gap_2().child(super::raster_icon(
                            icon,
                            px(16.),
                            t.colors.text_muted,
                            window,
                        ))
                    })
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
        assert!(push_str(&mut s, "cde", 4, any_char));
        assert_eq!(s, "abcd");
        assert!(!push_str(&mut s, "x", 4, any_char));
        assert!(!push_str(&mut s, "\n", 10, any_char));
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut s = String::new();
        assert!(!backspace(&mut s));
        let mut s = String::from("é");
        assert!(backspace(&mut s) && s.is_empty());
    }

    #[test]
    fn filter_drops_disallowed_typed_and_pasted_chars() {
        let hex = |c: char| c == '#' || c.is_ascii_hexdigit();
        let mut s = String::new();
        assert!(paste(&mut s, " #9b5Cf6 xyz!", 7, hex));
        assert_eq!(s, "#9b5Cf6");
        assert!(!push_str(&mut s, "0", 7, hex), "full at 7 chars");
        let mut s = String::new();
        assert!(!push_str(&mut s, "zz", 7, hex));
        assert!(s.is_empty());
    }

    #[test]
    fn paste_takes_first_line() {
        let mut s = String::new();
        assert!(paste(&mut s, "#9B5CF6\nsecond", 64, any_char));
        assert_eq!(s, "#9B5CF6");
        assert!(!paste(&mut s, "", 64, any_char));
    }

    #[test]
    fn selected_text_is_replaced_only_by_an_accepted_edit() {
        let mut text = "12".to_owned();
        let mut selected = true;
        assert!(!replace_selection(
            &mut text,
            &mut selected,
            "\n",
            8,
            any_char
        ));
        assert_eq!(text, "12");
        assert!(selected);
        assert!(replace_selection(
            &mut text,
            &mut selected,
            "-6,5",
            8,
            any_char
        ));
        assert_eq!(text, "-6,5");
        assert!(!selected);
    }
}
