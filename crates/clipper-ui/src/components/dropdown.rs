//! Stateless dropdown: the caller view owns open/highlight state and keyboard actions.

use std::{cell::Cell, rc::Rc};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Div, ElementId, FocusHandle, MouseButton, Pixels, SharedString, Stateful, Window,
    anchored, canvas, deferred, div, px, rgb, rgba,
};

use crate::theme;

#[derive(Clone, Debug)]
pub struct DropdownOption {
    pub label: SharedString,
    /// Secondary text on the right, e.g. "1920 × 1080".
    pub detail: Option<SharedString>,
    /// Disabled options are shown muted with `reason` and cannot be chosen.
    pub enabled: bool,
    pub reason: Option<SharedString>,
}

/// Next enabled index from `from` moving by `delta` (wraps, skips disabled).
/// Returns `from` when no option is enabled.
pub fn step(options: &[DropdownOption], from: usize, delta: isize) -> usize {
    let n = options.len() as isize;
    if n == 0 || delta == 0 {
        return from;
    }
    let dir = delta.signum();
    let mut i = from as isize;
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if options[i as usize].enabled {
            return i as usize;
        }
    }
    from
}

/// `highlighted: Some(i)` means the menu is open with row i highlighted.
/// The menu is a `deferred(anchored())` child of an absolutely positioned slot placed just
/// below the trigger (anchored uses its layout position), so it floats above siblings.
/// The trigger toggles on mouse-down and the menu's outside-click ignores presses over the
/// trigger, so clicking the trigger while open closes instead of dismiss+reopen.
#[allow(clippy::too_many_arguments)] // fixed API agreed with the caller view
pub fn dropdown(
    id: impl Into<ElementId>,
    options: &[DropdownOption],
    selected: usize,
    highlighted: Option<usize>,
    focus: &FocusHandle,
    focused: bool,
    enabled: bool,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let open = highlighted.is_some();
    let label = options
        .get(selected)
        .map(|o| o.label.clone())
        .unwrap_or_default();
    let trigger_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
    let bounds_out = trigger_bounds.clone();
    let focus_click = focus.clone();

    let trigger = div()
        .id((id.clone(), "trigger"))
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .h(px(32.))
        .px_3()
        .rounded_md()
        .border_1()
        .border_color(rgb(if focused || open {
            theme::FOCUS_RING
        } else {
            theme::BORDER
        }))
        .bg(rgb(theme::SURFACE_RAISED))
        .text_sm()
        .text_color(rgb(theme::TEXT))
        .track_focus(focus)
        .key_context("Dropdown")
        .child(
            canvas(move |b, _, _| bounds_out.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(div().child(label))
        .child(div().text_color(rgb(theme::TEXT_TERTIARY)).child("▾"))
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(|s| s.bg(rgb(theme::SURFACE_HOVER)))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.focus(&focus_click);
                    on_toggle(window, cx);
                })
        })
        .when(!enabled, |this| this.opacity(0.45));

    let on_select = Rc::new(on_select);
    let menu = highlighted.filter(|_| enabled).map(|hi| {
        let rows = options.iter().enumerate().map(|(i, o)| {
            let on_select = on_select.clone();
            let row = div()
                .id((id.clone(), format!("row-{i}")))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .child(
                    div()
                        .w(px(14.))
                        .text_color(rgb(theme::ACCENT))
                        .child(if i == selected { "✓" } else { "" }),
                )
                .child(div().flex_1().child(o.label.clone()))
                .children(o.detail.clone().map(|d| {
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_TERTIARY))
                        .child(d)
                }));
            if o.enabled {
                row.text_color(rgb(theme::TEXT))
                    .cursor_pointer()
                    .when(i == hi, |r| r.bg(rgba(theme::ACCENT_SUBTLE)))
                    .when(i != hi, |r| r.hover(|s| s.bg(rgb(theme::SURFACE_HOVER))))
                    .on_click(move |_, window, cx| on_select(i, window, cx))
            } else {
                row.text_color(rgb(theme::TEXT_TERTIARY))
                    .children(o.reason.clone().map(|r| div().text_xs().child(r)))
            }
        });
        let on_dismiss = Rc::new(on_dismiss);
        deferred(
            anchored().snap_to_window_with_margin(px(8.)).child(
                div()
                    .id((id.clone(), "menu"))
                    .occlude()
                    .flex()
                    .flex_col()
                    .min_w(px(200.))
                    .p_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(theme::BORDER_STRONG))
                    .bg(rgb(theme::SURFACE))
                    .shadow_lg()
                    .children(rows)
                    .on_mouse_down_out(move |_, window, cx| {
                        if !trigger_bounds.get().contains(&window.mouse_position()) {
                            on_dismiss(window, cx);
                        }
                    }),
            ),
        )
    });

    div()
        .id(id)
        .relative()
        .w_full()
        .child(trigger)
        .children(menu.map(|m| {
            // zero-size slot just below the trigger; anchored sits at its layout position
            div().absolute().top(px(36.)).left_0().child(m)
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(enabled: &[bool]) -> Vec<DropdownOption> {
        enabled
            .iter()
            .map(|&e| DropdownOption {
                label: "x".into(),
                detail: None,
                enabled: e,
                reason: None,
            })
            .collect()
    }

    #[test]
    fn step_skips_disabled_and_wraps() {
        let o = opts(&[true, false, true]);
        assert_eq!(step(&o, 0, 1), 2);
        assert_eq!(step(&o, 2, 1), 0);
        assert_eq!(step(&o, 0, -1), 2);
        assert_eq!(step(&opts(&[false, false]), 1, 1), 1);
        assert_eq!(step(&[], 0, 1), 0);
    }
}
