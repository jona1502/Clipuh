use gpui::prelude::*;
use gpui::{Div, div};

use crate::components::empty_state;

pub(super) fn content() -> Div {
    div().child(empty_state(
        "Choose a clip to get started",
        "The editor will be ready when saved clips are available.",
        "✦",
    ))
}
