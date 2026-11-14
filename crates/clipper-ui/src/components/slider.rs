//! Horizontal slider: click or drag the track, or Left/Right while focused.

use std::{cell::Cell, rc::Rc};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Div, ElementId, FocusHandle, MouseButton, Pixels, Stateful, Window, actions,
    canvas, div, px, relative,
};

use crate::theme::Theme;

actions!(slider, [SlideDown, SlideUp]);

/// Keyboard step as a fraction of the whole range.
const KEY_STEP: f32 = 0.05;

/// Value under pointer x (window coordinates) for a track at `left` with `width`.
pub fn value_at(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 {
        return 0.0;
    }
    ((x - left) / width).clamp(0.0, 1.0)
}

/// `value` moved by `delta`, clamped to 0..=1.
pub fn stepped(value: f32, delta: f32) -> f32 {
    (value + delta).clamp(0.0, 1.0)
}

/// `value` is 0..=1. Dragging or key steps call `on_change` with the new value.
/// Disabled: dimmed, no handlers. `focused` only draws the focus ring.
pub fn slider(
    t: &Theme,
    id: impl Into<ElementId>,
    value: f32,
    enabled: bool,
    focus: &FocusHandle,
    focused: bool,
    on_change: impl Fn(f32, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let value = value.clamp(0.0, 1.0);
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let bounds_out = bounds.clone();
    let on_change = Rc::new(on_change);
    let pick = {
        let on_change = on_change.clone();
        move |x: Pixels, window: &mut Window, cx: &mut App| {
            let b = bounds.get();
            on_change(
                value_at(f32::from(x), f32::from(b.origin.x), f32::from(b.size.width)),
                window,
                cx,
            );
        }
    };
    let (down, drag) = (pick.clone(), pick);
    let (key_down, key_up) = (on_change.clone(), on_change);
    let focus_click = focus.clone();

    div()
        .id(id)
        .key_context("Slider")
        .track_focus(focus)
        .relative()
        .w_full()
        .h(px(20.))
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(if focused {
            t.colors.focus_ring
        } else {
            gpui::transparent_black()
        })
        .child(
            div()
                .relative()
                .w_full()
                .h(px(4.))
                .rounded_full()
                .bg(t.colors.border_strong)
                .child(
                    canvas(move |b, _, _| bounds_out.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .child(
                    div()
                        .h_full()
                        .w(relative(value))
                        .rounded_full()
                        .bg(t.colors.accent),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .left(relative(value))
                        .ml(px(-6.))
                        .size(px(12.))
                        .rounded_full()
                        .bg(t.colors.accent)
                        .border_2()
                        .border_color(t.colors.surface),
                ),
        )
        .when(enabled, |this| {
            this.cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    window.focus(&focus_click);
                    down(event.position.x, window, cx);
                })
                .on_mouse_move(move |event, window, cx| {
                    if event.dragging() {
                        drag(event.position.x, window, cx);
                    }
                })
                .on_action(move |_: &SlideDown, window, cx| {
                    key_down(stepped(value, -KEY_STEP), window, cx)
                })
                .on_action(move |_: &SlideUp, window, cx| {
                    key_up(stepped(value, KEY_STEP), window, cx)
                })
        })
        .when(!enabled, |this| this.opacity(0.45))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_maps_and_clamps() {
        assert_eq!(value_at(150.0, 100.0, 100.0), 0.5);
        assert_eq!(value_at(50.0, 100.0, 100.0), 0.0);
        assert_eq!(value_at(500.0, 100.0, 100.0), 1.0);
        assert_eq!(value_at(10.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn steps_clamp() {
        assert!((stepped(0.5, KEY_STEP) - 0.55).abs() < 1e-6);
        assert_eq!(stepped(0.98, KEY_STEP), 1.0);
        assert_eq!(stepped(0.02, -KEY_STEP), 0.0);
    }
}
