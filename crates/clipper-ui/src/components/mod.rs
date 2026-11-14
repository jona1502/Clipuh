//! Shared building blocks. Views compose these instead of styling raw divs, so surfaces,
//! spacing and states stay consistent across pages.

mod dropdown;
mod icon;
mod raster_icon;
mod slider;
mod smooth_scroll;
mod text_input;
mod toggle;
mod tooltip;
pub use dropdown::{Decor, DropdownOption, dropdown, dropdown_decorated, dropdown_ghost, step};
pub use icon::{Icon, icon, icon_tile};
pub use raster_icon::{RasterIcon, raster_checkbox, raster_icon, raster_icon_hover};
pub use slider::slider;
pub use smooth_scroll::smooth_scroll;
pub use text_input::{TextInput, TextInputEvent};
pub use toggle::toggle;
pub use tooltip::tooltip;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, Hsla, IntoElement, KeyBinding, SharedString,
    Stateful, actions, div, px, transparent_black,
};

use crate::theme::Theme;

actions!(components, [Activate, Submit, Dismiss]);

/// Registers the key bindings the stateful components rely on. Call once at startup.
/// Actions (not raw key-down) are used because bindings run before `on_key_down` and the
/// window context already binds `enter`.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", Activate, Some("Toggle")),
        KeyBinding::new("enter", Activate, Some("Toggle")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new("escape", Dismiss, Some("TextInput")),
        KeyBinding::new("left", slider::SlideDown, Some("Slider")),
        KeyBinding::new("right", slider::SlideUp, Some("Slider")),
    ]);
}

/// Semantic color for badges and status dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn color(self, t: &Theme) -> Hsla {
        match self {
            Self::Neutral => t.colors.neutral,
            Self::Success => t.colors.success,
            Self::Warning => t.colors.warning,
            Self::Danger => t.colors.danger,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// The base surface: one subtle border, one fill, one radius.
pub fn panel(t: &Theme) -> Div {
    div()
        .rounded_xl()
        .border_1()
        .border_color(t.colors.border)
        .bg(t.colors.surface)
}

/// Small uppercase-free section label with optional trailing note.
// shortcut: no caller since the Phase 8 Settings cards; drop the allow when a page uses it.
#[allow(dead_code)]
pub fn section_header(
    t: &Theme,
    title: impl Into<SharedString>,
    trailing: Option<SharedString>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.colors.text_secondary)
                .child(title.into()),
        )
        .children(trailing.map(|note| div().text_xs().text_color(t.colors.text_muted).child(note)))
}

/// A keycap. Pass one key per call ("Ctrl", "K").
pub fn kbd(t: &Theme, key: impl Into<SharedString>) -> Div {
    div()
        .min_w(px(18.))
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(t.colors.border_strong)
        .bg(t.colors.surface_raised)
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.colors.text_secondary)
        .child(key.into())
}

/// Key combination such as `&["Ctrl", "K"]`.
pub fn shortcut(t: &Theme, keys: &[&'static str]) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .children(keys.iter().map(|key| kbd(t, *key)))
}

pub fn status_dot(t: &Theme, tone: Tone) -> Div {
    div().size(px(6.)).rounded_full().bg(tone.color(t))
}

/// Minimal pill: colored dot plus muted label.
// shortcut: no caller since the capture bar replaced the page header; drop the allow when a page uses it.
#[allow(dead_code)]
pub fn badge(t: &Theme, label: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(22.))
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(t.colors.border)
        .bg(t.colors.surface_raised)
        .text_xs()
        .text_color(t.colors.text_secondary)
        .child(status_dot(t, tone))
        .child(label.into())
}

/// One line of text cut off with an ellipsis.
///
/// Prefer this over `truncate()` for text whose container width is only known
/// after layout (flex columns, grid cards). GPUI 0.2.2 caches a text's
/// measured size keyed by its *wrap* width only; with `truncate()`
/// (`white-space: nowrap`) that key is empty, so the first measurement wins:
/// when the first layout probe offers a tiny width, the text stays "…" even
/// after the real width is known. A one-line clamp keeps normal wrapping, so
/// the available width is part of the key and the ellipsis is recomputed.
pub trait SingleLine: Styled + Sized {
    fn single_line(self) -> Self {
        self.line_clamp(1).text_ellipsis()
    }
}

impl<T: Styled> SingleLine for T {}

/// Compact button. Disabled buttons keep their layout but drop hover and pointer.
/// Callers attach `on_click` to the returned element.
pub fn button(
    t: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    keys: Option<&[&'static str]>,
    enabled: bool,
) -> Stateful<Div> {
    button_with(t, id, None, label, variant, keys, enabled)
}

/// Text colour of a [`button`] label for `variant` (for icons drawn in it).
pub fn button_text_color(t: &Theme, variant: ButtonVariant) -> gpui::Hsla {
    match variant {
        ButtonVariant::Primary => t.colors.accent_foreground,
        ButtonVariant::Secondary => t.colors.text,
        ButtonVariant::Danger => t.colors.recording_foreground,
        ButtonVariant::Ghost => t.colors.text_secondary,
    }
}

/// [`button`] with `leading` (an icon) before the label.
pub fn button_with_icon(
    t: &Theme,
    id: impl Into<ElementId>,
    leading: AnyElement,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
) -> Stateful<Div> {
    button_with(t, id, Some(leading), label, variant, None, enabled)
}

fn button_with(
    t: &Theme,
    id: impl Into<ElementId>,
    leading: Option<AnyElement>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    keys: Option<&[&'static str]>,
    enabled: bool,
) -> Stateful<Div> {
    let (bg, hover, text, border) = match variant {
        ButtonVariant::Primary => (
            t.colors.accent,
            t.colors.accent_hover,
            t.colors.accent_foreground,
            t.colors.accent,
        ),
        ButtonVariant::Secondary => (
            t.colors.surface_raised,
            t.colors.surface_hover,
            t.colors.text,
            t.colors.border_strong,
        ),
        ButtonVariant::Danger => (
            t.colors.recording,
            t.colors.recording_hover,
            t.colors.recording_foreground,
            t.colors.recording,
        ),
        ButtonVariant::Ghost => (
            transparent_black(),
            t.colors.surface_hover,
            t.colors.text_secondary,
            transparent_black(),
        ),
    };

    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h(t.metrics.control_height)
        .px(t.metrics.control_padding)
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text)
        .children(leading)
        .child(label.into())
        .children(keys.map(|k| shortcut(t, k)))
        .map(|this| {
            if enabled {
                this.cursor_pointer().hover(|style| style.bg(hover))
            } else {
                this.opacity(0.45)
            }
        })
}

/// Selectable option chip (resolution, fps, quality). Callers attach `on_click` only when enabled.
pub fn chip(
    t: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(t.metrics.control_height_small)
        .px(t.metrics.control_padding)
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .map(|this| {
            if selected {
                this.border_color(t.colors.focus_ring)
                    .bg(t.colors.accent_subtle)
                    .text_color(t.colors.text)
            } else {
                this.border_color(t.colors.border)
                    .bg(t.colors.surface_raised)
                    .text_color(t.colors.text_secondary)
            }
        })
        .map(|this| {
            if enabled {
                this.cursor_pointer().when(!selected, |this| {
                    this.hover(|style| style.border_color(t.colors.border_strong))
                })
            } else {
                this.opacity(0.45)
            }
        })
        .child(label.into())
}

/// One label/value line inside a panel. Rows after the first draw a top divider.
// shortcut: no caller since the Phase 8 Settings cards; drop the allow when a page uses it.
#[allow(dead_code)]
pub fn property_row(
    t: &Theme,
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    value: impl IntoElement,
    first: bool,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .px(t.metrics.card_padding)
        .py(t.metrics.row_padding)
        .when(!first, |this| {
            this.border_t_1().border_color(t.colors.border)
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(div().text_sm().child(label.into()))
                .children(
                    description
                        .map(|text| div().text_xs().text_color(t.colors.text_muted).child(text)),
                ),
        )
        .child(value)
}

/// Right-aligned secondary value for `property_row`.
// shortcut: no caller since the Phase 8 Settings cards; drop the allow when a page uses it.
#[allow(dead_code)]
pub fn value_text(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_sm()
        .text_color(t.colors.text_secondary)
        .child(text.into())
}

/// Label, large value and one line of context.
// shortcut: no caller since the Phase 3 dashboard; drop the allow when a page uses it.
#[allow(dead_code)]
pub fn stat_tile(
    t: &Theme,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
    panel(t)
        .flex_1()
        .min_w_0()
        .p(t.metrics.card_padding)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(t.colors.text_muted)
                .child(label.into()),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(value.into()),
        )
        .child(
            div()
                .text_xs()
                .text_color(t.colors.text_secondary)
                .truncate()
                .child(detail.into()),
        )
}

pub fn empty_state(
    t: &Theme,
    glyph: &'static str,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> Div {
    div()
        .w_full()
        .px_8()
        .py_10()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(36.))
                .mb_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_lg()
                .border_1()
                .border_color(t.colors.border_strong)
                .bg(t.colors.surface_raised)
                .text_color(t.colors.text_secondary)
                .child(glyph),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(380.))
                .text_xs()
                .text_center()
                .text_color(t.colors.text_muted)
                .child(description.into()),
        )
}

/// Selectable list row (command palette).
/// `selected` is the keyboard/active selection; hover is a lighter preview of it.
pub fn list_item(
    t: &Theme,
    id: impl Into<ElementId>,
    glyph: Icon,
    label: impl Into<SharedString>,
    keys: Option<&[&'static str]>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .h(t.metrics.row_height)
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .map(|this| {
            if selected {
                this.bg(t.colors.accent_subtle).text_color(t.colors.text)
            } else {
                this.text_color(t.colors.text_secondary)
                    .hover(|style| style.bg(t.colors.surface_hover))
            }
        })
        .child(icon(glyph, px(16.)).text_color(if selected {
            t.colors.accent
        } else {
            t.colors.text_muted
        }))
        .child(div().flex_1().truncate().child(label.into()))
        .children(keys.map(|k| shortcut(t, k)))
}

/// Button in a busy state: same look, no interaction. `…` marks the pending work.
// shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
#[allow(dead_code)]
pub fn button_loading(
    t: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
) -> Stateful<Div> {
    let label: SharedString = format!("{}…", label.into()).into();
    button(t, id, label, variant, None, false).cursor_default()
}

/// Bordered surface with an optional header row (title left, actions right).
/// Callers append body children.
// shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
#[allow(dead_code)]
pub fn card(t: &Theme, title: Option<SharedString>, actions: Option<AnyElement>) -> Div {
    let header = (title.is_some() || actions.is_some()).then(|| {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(t.metrics.gap)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.unwrap_or_default()),
            )
            .children(actions)
    });
    panel(t)
        .p(t.metrics.card_padding)
        .flex()
        .flex_col()
        .gap(t.metrics.gap)
        .children(header)
}

/// Page card with the reference header: accent icon tile, title, one-line
/// subtitle and optional trailing actions. The body goes in as children.
pub fn icon_card(
    t: &Theme,
    glyph: Icon,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    actions: Option<AnyElement>,
) -> Div {
    panel(t)
        .p(t.metrics.card_padding * 1.25)
        .flex()
        .flex_col()
        .gap(t.metrics.gap * 2.)
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(icon_tile(t, glyph, px(44.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(title.into()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(t.colors.text_muted)
                                .truncate()
                                .child(subtitle.into()),
                        ),
                )
                .children(actions),
        )
}

/// `card` lifted off the page: raised fill plus a small shadow.
// shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
#[allow(dead_code)]
pub fn elevated_card(t: &Theme, title: Option<SharedString>, actions: Option<AnyElement>) -> Div {
    card(t, title, actions)
        .bg(t.colors.surface_raised)
        .shadow_sm()
}

/// Horizontal 1px rule.
// shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
#[allow(dead_code)]
pub fn divider(t: &Theme) -> Div {
    div().w_full().h(px(1.)).flex_none().bg(t.colors.border)
}

/// Vertical 1px rule; give the parent a height or `items_stretch`.
// shortcut: no caller until the Phase 2 pages; drop this allow when one exists.
#[allow(dead_code)]
pub fn vertical_divider(t: &Theme) -> Div {
    div().h_full().w(px(1.)).flex_none().bg(t.colors.border)
}

#[cfg(test)]
mod single_line_tests {
    use super::*;
    use gpui::{Overflow, TextOverflow, WhiteSpace};

    #[test]
    fn single_line_clamps_without_nowrap() {
        let mut element = div().single_line();
        let style = element.style();
        let text = style.text.clone().expect("text style");
        assert_eq!(text.line_clamp, Some(1));
        assert!(matches!(
            text.text_overflow,
            Some(TextOverflow::Truncate(_))
        ));
        // Keeps normal wrapping: the wrap width is what GPUI caches on.
        assert_ne!(text.white_space, Some(WhiteSpace::Nowrap));
        assert_eq!(style.overflow.x, Some(Overflow::Hidden));
    }
}
