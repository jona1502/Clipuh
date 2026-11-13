use gpui::prelude::*;
use gpui::{Div, div};

use crate::components::settings_row;

pub(super) fn content() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(settings_row(
            "Capture",
            "Capture controls will appear when the capture service is connected.",
            "Not connected",
        ))
        .child(settings_row(
            "Storage",
            "Clip storage preferences will appear when the clip library is connected.",
            "Not connected",
        ))
        .child(settings_row(
            "About",
            "Clipper desktop workspace",
            "Version 0.1.0",
        ))
}
