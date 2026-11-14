//! Smooth mouse-wheel scrolling for GPUI scroll containers.
//!
//! GPUI applies a mouse-wheel notch as one instant jump (three lines, about
//! 60 px), so wheel scrolling shows one new position per notch: a few
//! updates per second while spinning, which reads as stutter. This helper
//! keeps GPUI's own scroll handling (bounds, nesting, touchpad) and only
//! re-times line-based wheel deltas: the jump is undone and the offset eases
//! to the same target over the next display refreshes. Fine deltas pass
//! through untouched: pixel deltas, and line deltas below one line, which is
//! how Windows precision touchpads and free-spinning wheels report
//! (WM_MOUSEWHEEL with a fraction of WHEEL_DELTA); they are already smooth
//! and must not gain latency. Programmatic offset changes (anchor navigation) cancel a
//! running animation.

use std::time::{Duration, Instant};

use gpui::{
    App, Div, ElementId, Entity, Pixels, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent, Size,
    Stateful, Window, point, prelude::*, px,
};

/// Time constant of the exponential ease. A 60 px notch is 95 % done after
/// three time constants (~135 ms).
const TAU: Duration = Duration::from_millis(45);
/// Remaining distance below which the animation lands on the target.
const SETTLE: f32 = 0.5;

/// Animation model of one scroll container (pure; unit tested).
#[derive(Debug, Default)]
pub struct SmoothScrollState {
    /// Offset this model last set (or observed while idle).
    visual: Point<Pixels>,
    target: Point<Pixels>,
    last_tick: Option<Instant>,
}

impl SmoothScrollState {
    pub fn is_animating(&self) -> bool {
        self.last_tick.is_some()
    }

    /// Adopts the container's current offset while idle (programmatic
    /// scrolling, layout clamping).
    pub fn observe(&mut self, offset: Point<Pixels>) {
        if !self.is_animating() {
            self.visual = offset;
            self.target = offset;
        }
    }

    /// A wheel event that GPUI already applied: `applied` is the container
    /// offset after GPUI's handler, `lines` whether it was a line-based (mouse
    /// wheel) delta, `max` the scrollable extent. Returns the offset the
    /// container must show now.
    pub fn wheel(
        &mut self,
        applied: Point<Pixels>,
        lines: bool,
        max: Size<Pixels>,
        now: Instant,
    ) -> Point<Pixels> {
        let delta = applied - self.visual;
        if delta == Point::default() {
            return applied;
        }
        if !lines {
            // Touchpad: already smooth; stop any wheel animation in place.
            self.visual = applied;
            self.target = applied;
            self.last_tick = None;
            return applied;
        }
        let clamp = |value: Pixels, extent: Pixels| value.min(px(0.)).max(-extent.max(px(0.)));
        self.target = point(
            clamp(self.target.x + delta.x, max.width),
            clamp(self.target.y + delta.y, max.height),
        );
        if self.target == self.visual {
            // Already at the edge (or nothing to scroll): no frames needed.
            self.last_tick = None;
        } else if self.last_tick.is_none() {
            self.last_tick = Some(now);
        }
        self.visual
    }

    /// Advances the animation to `now`. Returns the offset to show and
    /// whether the animation continues.
    pub fn step(&mut self, now: Instant) -> (Point<Pixels>, bool) {
        let Some(last) = self.last_tick else {
            return (self.visual, false);
        };
        let dt = now.saturating_duration_since(last).as_secs_f32();
        let k = 1. - (-dt / TAU.as_secs_f32()).exp();
        let remaining = self.target - self.visual;
        let next = self.visual + point(remaining.x * k, remaining.y * k);
        let left = self.target - next;
        if f32::from(left.x).abs() < SETTLE && f32::from(left.y).abs() < SETTLE {
            self.visual = self.target;
            self.last_tick = None;
            return (self.visual, false);
        }
        self.visual = next;
        self.last_tick = Some(now);
        (next, true)
    }

    /// Someone else moved the container: stop and keep their offset.
    pub fn cancel(&mut self, offset: Point<Pixels>) {
        self.visual = offset;
        self.target = offset;
        self.last_tick = None;
    }
}

/// Makes `element` a smoothly wheel-scrolled container tracked by `handle`.
/// Call during render; `key` must be unique among the view's scroll areas.
/// The element still needs its `overflow_*_scroll()` style.
pub fn smooth_scroll(
    element: Stateful<Div>,
    key: impl Into<ElementId>,
    handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let state = window.use_keyed_state(key, cx, |_, _| SmoothScrollState::default());
    let offset = handle.offset();
    state.update(cx, |state, _| state.observe(offset));
    let handle_for_wheel = handle.clone();
    element
        .track_scroll(handle)
        .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
            let handle = handle_for_wheel.clone();
            let lines = is_notch(&event.delta);
            let applied = handle.offset();
            let max = handle.max_offset();
            let (show, start) = state.update(cx, |state, _| {
                let was_animating = state.is_animating();
                let show = state.wheel(applied, lines, max, Instant::now());
                (show, !was_animating && state.is_animating())
            });
            handle.set_offset(show);
            if start {
                schedule(state.clone(), handle, window);
            }
        })
}

/// A coarse, line-based step (a mouse-wheel notch is three lines).
fn is_notch(delta: &ScrollDelta) -> bool {
    match delta {
        ScrollDelta::Lines(lines) => lines.x.abs() >= 1. || lines.y.abs() >= 1.,
        ScrollDelta::Pixels(_) => false,
    }
}

fn schedule(state: Entity<SmoothScrollState>, handle: ScrollHandle, window: &mut Window) {
    window.on_next_frame(move |window, cx| tick(state, handle, window, cx));
}

fn tick(state: Entity<SmoothScrollState>, handle: ScrollHandle, window: &mut Window, cx: &mut App) {
    let current = handle.offset();
    let continues = state.update(cx, |state, cx| {
        if current != state.visual {
            state.cancel(current);
            return false;
        }
        let (offset, continues) = state.step(Instant::now());
        handle.set_offset(offset);
        // Redraws the view that owns the container (use_keyed_state
        // observes this entity).
        cx.notify();
        continues
    });
    if continues {
        schedule(state, handle, window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(y: f32) -> Point<Pixels> {
        point(px(0.), px(y))
    }

    const MAX: Size<Pixels> = Size {
        width: px(0.),
        height: px(1000.),
    };

    #[test]
    fn wheel_notch_is_spread_over_frames_and_lands_on_the_target() {
        let mut s = SmoothScrollState::default();
        s.observe(p(0.));
        let t0 = Instant::now();
        // GPUI applied a 60 px notch downwards.
        let shown = s.wheel(p(-60.), true, MAX, t0);
        assert_eq!(shown, p(0.), "the jump is undone");
        let mut now = t0;
        let mut previous = 0.;
        let mut frames = 0;
        loop {
            now += Duration::from_micros(5_556); // 180 Hz
            let (offset, more) = s.step(now);
            let y = f32::from(offset.y);
            assert!(y <= previous, "moves monotonically");
            previous = y;
            frames += 1;
            if !more {
                break;
            }
        }
        assert_eq!(s.step(now).0, p(-60.));
        assert!((10..=60).contains(&frames), "{frames} frames");
    }

    #[test]
    fn consecutive_notches_accumulate() {
        let mut s = SmoothScrollState::default();
        let t0 = Instant::now();
        s.observe(p(0.));
        let shown = s.wheel(p(-60.), true, MAX, t0);
        // Second notch arrives before the first finished; GPUI applied it
        // relative to the offset we restored.
        let shown = s.wheel(shown + p(-60.), true, MAX, t0 + Duration::from_millis(10));
        assert_eq!(shown, p(0.));
        let mut now = t0;
        loop {
            now += Duration::from_millis(8);
            if !s.step(now).1 {
                break;
            }
        }
        assert_eq!(s.visual, p(-120.));
    }

    #[test]
    fn target_is_clamped_to_the_scroll_range() {
        let mut s = SmoothScrollState::default();
        s.observe(p(-990.));
        let t0 = Instant::now();
        s.wheel(p(-1050.), true, MAX, t0);
        assert_eq!(s.target, p(-1000.));
        s.observe(p(0.));
        let mut s = SmoothScrollState::default();
        s.observe(p(0.));
        s.wheel(p(60.), true, MAX, t0);
        assert_eq!(s.target, p(0.));
    }

    #[test]
    fn wheel_at_the_edge_does_not_animate() {
        let mut s = SmoothScrollState::default();
        s.observe(p(0.));
        // Content fits (max 0): GPUI applied a jump that layout will undo.
        let none = Size {
            width: px(0.),
            height: px(0.),
        };
        assert_eq!(s.wheel(p(-60.), true, none, Instant::now()), p(0.));
        assert!(!s.is_animating());
        // At the top, scrolling further up.
        s.observe(p(0.));
        s.wheel(p(60.), true, MAX, Instant::now());
        assert!(!s.is_animating());
    }

    #[test]
    fn touchpad_pixel_deltas_pass_through_and_stop_animation() {
        let mut s = SmoothScrollState::default();
        let t0 = Instant::now();
        s.observe(p(0.));
        s.wheel(p(-60.), true, MAX, t0);
        assert!(s.is_animating());
        let shown = s.wheel(p(-3.), false, MAX, t0 + Duration::from_millis(5));
        assert_eq!(shown, p(-3.));
        assert!(!s.is_animating());
    }

    #[test]
    fn only_coarse_line_steps_are_animated() {
        assert!(is_notch(&ScrollDelta::Lines(point(0., -3.))));
        assert!(is_notch(&ScrollDelta::Lines(point(1., 0.))));
        // 20/120 of a notch from a precision touchpad.
        assert!(!is_notch(&ScrollDelta::Lines(point(0., -0.5))));
        assert!(!is_notch(&ScrollDelta::Pixels(point(px(0.), px(-40.)))));
    }

    #[test]
    fn idle_observe_follows_programmatic_scrolling() {
        let mut s = SmoothScrollState::default();
        s.observe(p(-400.));
        assert_eq!(s.visual, p(-400.));
        // While animating, observe does not fight the animation.
        s.wheel(p(-460.), true, MAX, Instant::now());
        s.observe(p(-123.));
        assert_eq!(s.visual, p(-400.));
        s.cancel(p(-123.));
        assert!(!s.is_animating());
        assert_eq!(s.visual, p(-123.));
    }

    #[test]
    fn events_that_did_not_move_this_container_are_ignored() {
        let mut s = SmoothScrollState::default();
        s.observe(p(-50.));
        let shown = s.wheel(p(-50.), true, MAX, Instant::now());
        assert_eq!(shown, p(-50.));
        assert!(!s.is_animating());
    }
}
