use gpui::prelude::*;
use gpui::{App, ClickEvent, Div, FontWeight, IntoElement, Window, div, px, rgb};

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

pub fn settings_row(title: &'static str, description: &'static str, value: &'static str) -> Div {
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
                .child(value),
        )
}
