use gpui::prelude::*;
use gpui::{Div, div, px, rgb};

use crate::components::{
    ButtonVariant, button, panel, property_row, section_header, shortcut, value_text,
};
use crate::theme;

fn muted() -> Div {
    div().text_xs().text_color(rgb(theme::TEXT_TERTIARY))
}

fn rows(rows: &[(&'static str, &'static str)]) -> Div {
    panel().children(
        rows.iter()
            .enumerate()
            .map(|(i, (label, value))| property_row(*label, None, value_text(*value), i == 0)),
    )
}

pub(super) fn content() -> Div {
    let preview = div()
        .w_full()
        .h(px(300.))
        .rounded_lg()
        .bg(rgb(theme::BACKGROUND))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child("No clip selected"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(muted().child("Choose a clip in Clips"))
                .child(shortcut(&["Ctrl", "2"])),
        );

    let timeline = div()
        .flex()
        .items_center()
        .gap_3()
        .child(muted().child("00:00"))
        .child(
            div()
                .flex_1()
                .h(px(40.))
                .rounded_md()
                .bg(rgb(theme::SURFACE_RAISED))
                .border_1()
                .border_color(rgb(theme::BORDER)),
        )
        .child(muted().child("--:--"));

    let left = div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_6()
        .child(panel().p_3().child(preview))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("Timeline", None))
                .child(panel().p_3().child(timeline)),
        );

    let right = div()
        .w(px(260.))
        .flex()
        .flex_col()
        .gap_6()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("Trim", None))
                .child(rows(&[
                    ("Start", "--:--"),
                    ("End", "--:--"),
                    ("Length", "--:--"),
                ])),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("Export", None))
                .child(rows(&[("Format", "MP4 · H.264"), ("Quality", "Source")]))
                .child(
                    button("export", "Export clip", ButtonVariant::Primary, None, false).w_full(),
                ),
        );

    div().flex().gap_6().child(left).child(right)
}
