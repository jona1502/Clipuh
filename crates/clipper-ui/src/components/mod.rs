//! Shared building blocks. Views compose these instead of styling raw divs, so surfaces,
//! spacing and states stay consistent across pages.

mod dropdown;
pub use dropdown::{DropdownOption, dropdown, step};

use gpui::prelude::*;
use gpui::{Div, ElementId, FontWeight, IntoElement, SharedString, Stateful, div, px, rgb, rgba};

use crate::theme;

/// Semantic color for badges and status dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
}

impl Tone {
    fn color(self) -> u32 {
        match self {
            Self::Neutral => theme::NEUTRAL,
            Self::Success => theme::SUCCESS,
            Self::Warning => theme::WARNING,
            Self::Danger => theme::DANGER,
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
pub fn panel() -> Div {
    div()
        .rounded_xl()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
}

/// Small uppercase-free section label with optional trailing note.
pub fn section_header(title: impl Into<SharedString>, trailing: Option<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(title.into()),
        )
        .children(trailing.map(|note| {
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child(note)
        }))
}

/// A keycap. Pass one key per call ("Ctrl", "K").
pub fn kbd(key: impl Into<SharedString>) -> Div {
    div()
        .min_w(px(18.))
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER_STRONG))
        .bg(rgb(theme::SURFACE_RAISED))
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(key.into())
}

/// Key combination such as `&["Ctrl", "K"]`.
pub fn shortcut(keys: &[&'static str]) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(3.))
        .children(keys.iter().map(|key| kbd(*key)))
}

pub fn status_dot(tone: Tone) -> Div {
    div().size(px(6.)).rounded_full().bg(rgb(tone.color()))
}

/// Minimal pill: colored dot plus muted label.
pub fn badge(label: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(22.))
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE_RAISED))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(status_dot(tone))
        .child(label.into())
}

/// Compact button. Disabled buttons keep their layout but drop hover and pointer.
/// Callers attach `on_click` to the returned element.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    keys: Option<&[&'static str]>,
    enabled: bool,
) -> Stateful<Div> {
    let (bg, hover, text, border) = match variant {
        ButtonVariant::Primary => (
            theme::ACCENT,
            theme::ACCENT_HOVER,
            theme::TEXT_ON_ACCENT,
            theme::ACCENT,
        ),
        ButtonVariant::Secondary => (
            theme::SURFACE_RAISED,
            theme::SURFACE_HOVER,
            theme::TEXT,
            theme::BORDER_STRONG,
        ),
        ButtonVariant::Danger => (
            theme::RECORDING,
            theme::RECORDING_HOVER,
            theme::TEXT_ON_ACCENT,
            theme::RECORDING,
        ),
        ButtonVariant::Ghost => (
            theme::SURFACE,
            theme::SURFACE_HOVER,
            theme::TEXT_SECONDARY,
            theme::SURFACE,
        ),
    };

    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h(px(30.))
        .px_3()
        .rounded_md()
        .border_1()
        .border_color(rgb(border))
        .bg(rgb(bg))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text))
        .child(label.into())
        .children(keys.map(shortcut))
        .map(|this| {
            if enabled {
                this.cursor_pointer().hover(|style| style.bg(rgb(hover)))
            } else {
                this.opacity(0.45)
            }
        })
}

/// Selectable option chip (resolution, fps, quality). Callers attach `on_click` only when enabled.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .map(|this| {
            if selected {
                this.border_color(rgba(theme::FOCUS_RING))
                    .bg(rgba(theme::ACCENT_SUBTLE))
                    .text_color(rgb(theme::TEXT))
            } else {
                this.border_color(rgb(theme::BORDER))
                    .bg(rgb(theme::SURFACE_RAISED))
                    .text_color(rgb(theme::TEXT_SECONDARY))
            }
        })
        .map(|this| {
            if enabled {
                this.cursor_pointer().when(!selected, |this| {
                    this.hover(|style| style.border_color(rgb(theme::BORDER_STRONG)))
                })
            } else {
                this.opacity(0.45)
            }
        })
        .child(label.into())
}

/// One label/value line inside a panel. Rows after the first draw a top divider.
pub fn property_row(
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
        .px_4()
        .py_3()
        .when(!first, |this| {
            this.border_t_1().border_color(rgb(theme::BORDER))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(div().text_sm().child(label.into()))
                .children(description.map(|text| {
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(text)
                })),
        )
        .child(value)
}

/// Right-aligned secondary value for `property_row`.
pub fn value_text(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_sm()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(text.into())
}

/// Read-only switch visual. Settings are not editable from the UI yet.
pub fn switch(on: bool) -> Div {
    div()
        .flex_none()
        .w(px(28.))
        .h(px(16.))
        .p(px(2.))
        .flex()
        .rounded_full()
        .bg(rgb(if on {
            theme::ACCENT
        } else {
            theme::BORDER_STRONG
        }))
        .when(on, |this| this.justify_end())
        .child(
            div()
                .size(px(12.))
                .rounded_full()
                .bg(rgb(theme::TEXT_ON_ACCENT)),
        )
}

/// Label, large value and one line of context.
pub fn stat_tile(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
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
                .text_color(rgb(theme::TEXT_SECONDARY))
                .truncate()
                .child(detail.into()),
        )
}

pub fn empty_state(
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
                .border_color(rgb(theme::BORDER_STRONG))
                .bg(rgb(theme::SURFACE_RAISED))
                .text_color(rgb(theme::TEXT_SECONDARY))
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
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child(description.into()),
        )
}

/// Selectable list row shared by the sidebar and the command palette.
/// `selected` is the keyboard/active selection; hover is a lighter preview of it.
pub fn list_item(
    id: impl Into<ElementId>,
    glyph: &'static str,
    label: impl Into<SharedString>,
    keys: Option<&[&'static str]>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .h(px(32.))
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .map(|this| {
            if selected {
                this.bg(rgba(theme::ACCENT_SUBTLE))
                    .text_color(rgb(theme::TEXT))
            } else {
                this.text_color(rgb(theme::TEXT_SECONDARY))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            }
        })
        .child(
            div()
                .w(px(16.))
                .flex()
                .justify_center()
                .text_color(rgb(if selected {
                    theme::ACCENT
                } else {
                    theme::TEXT_TERTIARY
                }))
                .child(glyph),
        )
        .child(div().flex_1().truncate().child(label.into()))
        .children(keys.map(shortcut))
}
