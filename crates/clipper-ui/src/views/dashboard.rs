use gpui::prelude::*;
use gpui::{Div, FontWeight, div, px, rgb};

use crate::components::empty_state;
use crate::theme;

pub(super) fn content() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            div()
                .w_full()
                .rounded_2xl()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .bg(rgb(theme::SURFACE))
                .p_7()
                .flex()
                .items_center()
                .justify_between()
                .gap_8()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .max_w(px(540.))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(theme::ACCENT))
                                .child("YOUR LOCAL WORKSPACE"),
                        )
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Keep your best moments close."),
                        )
                        .child(div().text_sm().text_color(rgb(theme::MUTED)).child(
                            "Capture is not connected yet. No clips are being recorded or stored.",
                        )),
                )
                .child(
                    div()
                        .size(px(88.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_2xl()
                        .bg(rgb(0x211f3c))
                        .text_color(rgb(theme::ACCENT))
                        .text_2xl()
                        .child("✦"),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Recent clips"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::MUTED))
                        .child("Your library is empty"),
                ),
        )
        .child(empty_state(
            "Nothing to replay yet",
            "When clips are available, your latest highlights will appear here.",
            "◌",
        ))
}
