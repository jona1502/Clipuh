//! Interactive on/off toggle. `switch` stays the read-only visual.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Div, ElementId, FocusHandle, Stateful, Window, div, px};

use super::Activate;
use crate::theme::Theme;

/// Click, or Space/Enter while focused (via `Activate` in the "Toggle" key context; see
/// `components::bind_keys`), calls `on_change(!on)`. Disabled: dimmed, no handlers.
/// `focused` is `focus.is_focused(window)`, passed in because components get no `Window`;
/// it only draws the focus ring.
pub fn toggle(
    t: &Theme,
    id: impl Into<ElementId>,
    on: bool,
    enabled: bool,
    focus: &FocusHandle,
    focused: bool,
    on_change: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let knob = div()
        .size(px(12.))
        .rounded_full()
        // Off: a mid-tone knob that reads on the neutral track in both themes.
        .bg(if on {
            t.colors.accent_foreground
        } else {
            t.colors.text_secondary
        });
    let base = div()
        .id(id)
        .flex_none()
        .w(px(28.))
        .h(px(16.))
        .p(px(1.))
        .flex()
        .rounded_full()
        .border_1()
        .border_color(if focused && enabled {
            t.colors.focus_ring
        } else {
            gpui::transparent_black()
        })
        .bg(if on {
            t.colors.accent
        } else {
            t.colors.border_strong
        })
        .when(on, |this| this.justify_end())
        .child(knob);
    if !enabled {
        return base.opacity(0.45);
    }
    let on_change = Rc::new(on_change);
    let (on_click, on_key, focus_click) = (on_change.clone(), on_change, focus.clone());
    base.cursor_pointer()
        .key_context("Toggle")
        .track_focus(focus)
        .on_click(move |_, window, cx| {
            window.focus(&focus_click);
            on_click(!on, window, cx);
        })
        .on_action(move |_: &Activate, window, cx| on_key(!on, window, cx))
}
