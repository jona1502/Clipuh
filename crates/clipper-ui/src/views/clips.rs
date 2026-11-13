use gpui::prelude::*;
use gpui::{Div, div, px, rgb};

use crate::components::{ButtonVariant, button, empty_state, panel};
use crate::theme;

const COLUMNS: [&str; 5] = ["Name", "Game", "Duration", "Size", "Saved"];

pub(super) fn content() -> Div {
    let search = panel()
        .flex_1()
        .h(px(32.))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(rgb(theme::TEXT_TERTIARY))
        .child("⌕")
        .child("Search clips");

    let toolbar = div()
        .flex()
        .items_center()
        .gap_2()
        .child(search)
        .child(button(
            "sort",
            "Newest",
            ButtonVariant::Secondary,
            None,
            false,
        ))
        .child(button(
            "filter",
            "All games",
            ButtonVariant::Ghost,
            None,
            false,
        ))
        .child(
            div()
                .px_2()
                .text_xs()
                .text_color(rgb(theme::TEXT_TERTIARY))
                .child("0 clips"),
        );

    let header = div()
        .flex()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(rgb(theme::BORDER))
        .text_xs()
        .text_color(rgb(theme::TEXT_TERTIARY))
        .children(COLUMNS.map(|name| div().flex_1().child(name)));

    div().flex().flex_col().gap_3().child(toolbar).child(
        panel().child(header).child(empty_state(
            "▤",
            "No clips yet",
            "Saved replays will appear here. Save one with your replay hotkey once capture is connected.",
        )),
    )
}
