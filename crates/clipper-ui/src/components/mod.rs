use gpui::prelude::*;
use gpui::{
    App, ClickEvent, Div, ElementId, FontWeight, IntoElement, SharedString, Stateful, Window, div,
    px, rgb,
};

use crate::theme;

pub fn navigation_item(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .px_3()
        .py_3()
        .rounded_lg()
        .cursor_pointer()
        .bg(rgb(if active {
            theme::SURFACE_RAISED
        } else {
            theme::SIDEBAR
        }))
        .text_color(rgb(if active { theme::TEXT } else { theme::MUTED }))
        .on_click(on_click)
        .child(
            div()
                .w(px(22.))
                .text_color(rgb(if active { theme::ACCENT } else { theme::MUTED }))
                .child(glyph),
        )
        .child(
            div()
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .child(label),
        )
}

pub fn empty_state(title: &'static str, description: &'static str, glyph: &'static str) -> Div {
    div()
        .w_full()
        .rounded_2xl()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
        .px_8()
        .py_10()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            div()
                .size(px(52.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_2xl()
                .bg(rgb(theme::SURFACE_RAISED))
                .text_color(rgb(theme::ACCENT))
                .text_xl()
                .child(glyph),
        )
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .max_w(px(440.))
                .text_sm()
                .text_color(rgb(theme::MUTED))
                .text_center()
                .child(description),
        )
}

pub fn settings_row(
    title: &'static str,
    description: &'static str,
    value: impl Into<SharedString>,
) -> Div {
    div()
        .w_full()
        .rounded_xl()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
        .px_5()
        .py_4()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::MUTED))
                        .child(description),
                ),
        )
        .child(
            div()
                .px_3()
                .py_2()
                .rounded_full()
                .bg(rgb(theme::SURFACE_RAISED))
                .text_xs()
                .text_color(rgb(theme::MUTED))
                .child(value.into()),
        )
}

/// Selectable option chip. Disabled pills are muted and ignore clicks.
pub fn pill(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_full()
        .border_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .border_color(rgb(if selected && enabled {
            theme::ACCENT
        } else {
            theme::BORDER
        }))
        .bg(rgb(if selected {
            theme::SURFACE_RAISED
        } else {
            theme::SURFACE
        }))
        .text_color(rgb(if !enabled {
            theme::MUTED
        } else if selected {
            theme::TEXT
        } else {
            theme::MUTED
        }))
        .child(label.into());
    if enabled {
        base.cursor_pointer().on_click(on_click)
    } else {
        base.opacity(0.55)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Danger,
    Subtle,
}

/// Push button. Disabled buttons are muted and ignore clicks.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, fg) = match kind {
        ButtonKind::Primary => (theme::ACCENT, 0xffffff),
        ButtonKind::Danger => (0xd64545, 0xffffff),
        ButtonKind::Subtle => (theme::SURFACE_RAISED, theme::TEXT),
    };
    let base = div()
        .id(id)
        .px_5()
        .py_2()
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .child(label.into());
    if enabled {
        base.cursor_pointer().on_click(on_click)
    } else {
        base.opacity(0.4)
    }
}

/// Clickable list row used by the source picker.
pub fn list_row(
    id: impl Into<ElementId>,
    primary: impl Into<SharedString>,
    secondary: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .w_full()
        .px_3()
        .py_2()
        .rounded_lg()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .text_sm()
        .bg(rgb(if selected {
            theme::SURFACE_RAISED
        } else {
            theme::SURFACE
        }))
        .border_1()
        .border_color(rgb(if selected {
            theme::ACCENT
        } else {
            theme::SURFACE
        }))
        .text_color(rgb(if selected { theme::TEXT } else { theme::MUTED }))
        .child(div().flex_1().truncate().child(primary.into()))
        .child(div().text_xs().child(secondary.into()));
    if enabled {
        base.cursor_pointer().on_click(on_click)
    } else {
        base.opacity(0.55)
    }
}
