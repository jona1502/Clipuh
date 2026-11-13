use gpui::prelude::*;
use gpui::{Div, div};

use crate::components::empty_state;

pub(super) fn content() -> Div {
    div().child(empty_state(
        "No clips saved yet",
        "Saved highlights will appear here once capture is connected.",
        "▤",
    ))
}
