//! Timeline view math shared by every timeline element: zoom limits, the
//! cursor-anchored zoom, ruler ticks and track heights.
//!
//! The timeline content is `viewport_width * zoom` pixels wide and every
//! element (ruler, thumbnails, waveforms, segments, trim handles, playhead,
//! scrubbing) maps time with [`crate::time_to_pixel`] /
//! [`crate::pixel_to_time`] over that one content width, so zooming,
//! scrolling or resizing tracks can never misalign them. Zoom changes only the
//! view; media timestamps and exports are untouched.

use std::time::Duration;

/// Zoom 1.0 shows the whole source in the viewport ("fit").
pub const MIN_ZOOM: f32 = 1.0;
/// Highest zoom at least this much, so short clips can still zoom in.
pub const MIN_MAX_ZOOM: f32 = 8.0;
/// Closest view: one second spans this many pixels (10 px per frame at
/// 60 fps), enough for frame-level placement.
pub const MAX_PIXELS_PER_SECOND: f32 = 600.0;
/// Upper bound for very long recordings (content width stays reasonable).
pub const ZOOM_CEILING: f32 = 4096.0;
/// Zoom factor of one button press or one Ctrl+wheel notch.
pub const ZOOM_STEP: f32 = 1.25;

/// Largest useful zoom for `duration` shown in `viewport_width` pixels.
pub fn max_zoom(duration: Duration, viewport_width: f32) -> f32 {
    if viewport_width <= 0. || duration.is_zero() {
        return MIN_MAX_ZOOM;
    }
    (MAX_PIXELS_PER_SECOND * duration.as_secs_f32() / viewport_width)
        .clamp(MIN_MAX_ZOOM, ZOOM_CEILING)
}

pub fn clamp_zoom(zoom: f32, duration: Duration, viewport_width: f32) -> f32 {
    if !zoom.is_finite() {
        return MIN_ZOOM;
    }
    zoom.clamp(MIN_ZOOM, max_zoom(duration, viewport_width))
}

/// Slider position (0..=1, logarithmic) of `zoom`.
pub fn zoom_to_slider(zoom: f32, max: f32) -> f32 {
    if max <= MIN_ZOOM {
        return 0.;
    }
    (zoom.max(MIN_ZOOM).ln() / max.ln()).clamp(0., 1.)
}

/// Zoom of slider position `value` (0..=1, logarithmic).
pub fn slider_to_zoom(value: f32, max: f32) -> f32 {
    max.max(MIN_ZOOM).powf(value.clamp(0., 1.))
}

/// Horizontal scroll offset (pixels scrolled out to the left, >= 0) that
/// keeps the instant under `anchor_x` (pixels from the viewport's left edge)
/// in place when the timeline's time axis grows from `old_width` to
/// `new_width` pixels. `extra` is fixed content after the axis (padding);
/// the result never scrolls past the content end.
pub fn anchored_scroll(
    old_scroll: f32,
    anchor_x: f32,
    old_width: f32,
    new_width: f32,
    viewport_width: f32,
    extra: f32,
) -> f32 {
    if viewport_width <= 0. || old_width <= 0. || new_width <= 0. {
        return 0.;
    }
    let fraction = ((old_scroll + anchor_x) / old_width).clamp(0., 1.);
    let max_scroll = (new_width + extra - viewport_width).max(0.);
    (fraction * new_width - anchor_x).clamp(0., max_scroll)
}

/// Spacing of ruler labels for `pixels_per_second`: the smallest step in the
/// ladder that keeps labels at least `min_gap` pixels apart.
pub fn tick_interval(pixels_per_second: f32, min_gap: f32) -> Duration {
    const LADDER_MS: [u64; 16] = [
        100, 250, 500, 1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000,
        600_000, 900_000, 1_800_000, 3_600_000,
    ];
    if pixels_per_second <= 0. || !pixels_per_second.is_finite() {
        return Duration::from_millis(LADDER_MS[LADDER_MS.len() - 1]);
    }
    let needed = min_gap / pixels_per_second;
    LADDER_MS
        .iter()
        .map(|ms| Duration::from_millis(*ms))
        .find(|step| step.as_secs_f32() >= needed)
        .unwrap_or(Duration::from_millis(LADDER_MS[LADDER_MS.len() - 1]))
}

/// Ruler tick times inside the visible content range `[from_px, to_px]` of a
/// `content_width` timeline of `duration`, one interval of margin each side.
pub fn visible_ticks(
    duration: Duration,
    content_width: f32,
    from_px: f32,
    to_px: f32,
    min_gap: f32,
) -> Vec<Duration> {
    if duration.is_zero() || content_width <= 0. {
        return Vec::new();
    }
    let pps = content_width / duration.as_secs_f32();
    let step = tick_interval(pps, min_gap);
    let first = crate::pixel_to_time(from_px.max(0.), duration, content_width);
    let last = crate::pixel_to_time(to_px.min(content_width), duration, content_width);
    let step_ms = step.as_millis().max(1) as u64;
    let start = (first.as_millis() as u64 / step_ms).saturating_sub(1) * step_ms;
    (0..)
        .map(|i| Duration::from_millis(start + i * step_ms))
        .take_while(|at| *at <= last + step && *at <= duration)
        .take(512)
        .collect()
}

/// Which timeline row a height belongs to. Audio tracks are identified by
/// their source stream, which never changes for a file (unlike row order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackId {
    Video,
    Audio(u32),
}

/// Height limits of a timeline row, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeightLimits {
    pub default: f32,
    pub min: f32,
    pub max: f32,
}

impl HeightLimits {
    pub const VIDEO: Self = Self {
        default: 52.,
        min: 36.,
        max: 180.,
    };

    /// Audio rows never get shorter than their header controls need.
    pub fn audio(controls_height: f32) -> Self {
        Self {
            default: controls_height,
            min: controls_height,
            max: (controls_height * 4.).max(240.),
        }
    }

    pub fn clamp(self, height: f32) -> f32 {
        if height.is_finite() {
            height.clamp(self.min, self.max)
        } else {
            self.default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: f32) -> Duration {
        Duration::from_secs_f32(v)
    }

    #[test]
    fn zoom_limits_follow_duration_and_viewport() {
        // Short clip: at least MIN_MAX_ZOOM.
        assert_eq!(max_zoom(s(5.), 1000.), MIN_MAX_ZOOM);
        // 60 s in 1000 px: 600 px/s needs 36x.
        assert!((max_zoom(s(60.), 1000.) - 36.).abs() < 0.01);
        // Hours-long recordings are capped.
        assert_eq!(max_zoom(s(36_000.), 800.), ZOOM_CEILING);
        assert_eq!(clamp_zoom(0.2, s(60.), 1000.), MIN_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN, s(60.), 1000.), MIN_ZOOM);
        assert!((clamp_zoom(500., s(60.), 1000.) - 36.).abs() < 0.01);
    }

    #[test]
    fn slider_mapping_round_trips() {
        let max = 36.;
        for zoom in [1., 1.5, 4., 12., 36.] {
            let back = slider_to_zoom(zoom_to_slider(zoom, max), max);
            assert!((back - zoom).abs() < 0.01, "{zoom} -> {back}");
        }
        assert_eq!(zoom_to_slider(1., max), 0.);
        assert!((zoom_to_slider(36., max) - 1.).abs() < 1e-6);
    }

    #[test]
    fn anchored_zoom_keeps_the_instant_under_the_cursor() {
        let viewport = 1000.;
        let duration = s(60.);
        for (scroll, anchor, old, new) in [
            (0., 250., 1., 2.),
            (1500., 400., 4., 6.),
            (3000., 999., 8., 2.),
            (500., 0., 2., 2.5),
        ] {
            let before = crate::pixel_to_time(scroll + anchor, duration, viewport * old);
            let after_scroll =
                anchored_scroll(scroll, anchor, viewport * old, viewport * new, viewport, 0.);
            let after = crate::pixel_to_time(after_scroll + anchor, duration, viewport * new);
            let drift = before.as_secs_f64() - after.as_secs_f64();
            assert!(
                drift.abs() < 0.02,
                "{scroll} {anchor} {old}->{new}: {drift}"
            );
        }
    }

    #[test]
    fn anchored_zoom_never_scrolls_out_of_range() {
        // Zooming out to fit forces scroll 0.
        assert_eq!(anchored_scroll(4000., 500., 8000., 1000., 1000., 0.), 0.);
        // Right edge stays reachable but not beyond (48 px of padding).
        let scroll = anchored_scroll(7000., 1000., 8000., 8000., 1000., 48.);
        assert!(scroll <= 7048.);
    }

    #[test]
    fn playhead_pixel_maps_back_to_the_same_time_at_any_zoom() {
        let duration = s(26.3);
        let at = s(13.37);
        for zoom in [1., 3.375, 15.8, 120.] {
            let width = 1000. * zoom;
            let x = crate::time_to_pixel(at, duration, width);
            let back = crate::pixel_to_time(x, duration, width);
            assert!(back.abs_diff(at) < Duration::from_millis(2), "zoom {zoom}");
        }
    }

    #[test]
    fn ticks_stay_readable_and_only_cover_the_visible_range() {
        assert_eq!(tick_interval(600., 80.), Duration::from_millis(250));
        assert_eq!(tick_interval(40., 80.), Duration::from_secs(2));
        assert_eq!(tick_interval(1., 80.), Duration::from_secs(120));
        let duration = s(600.);
        let ticks = visible_ticks(duration, 60_000., 30_000., 31_000., 80.);
        assert!(!ticks.is_empty() && ticks.len() < 40, "{}", ticks.len());
        assert!(ticks.iter().all(|t| *t <= duration));
        assert!(ticks.windows(2).all(|w| w[0] < w[1]));
        // Fit view of a short clip labels the whole range.
        let all = visible_ticks(s(26.), 1000., 0., 1000., 80.);
        assert_eq!(all.first(), Some(&Duration::ZERO));
        assert!(all.last().unwrap().as_secs_f32() >= 24.);
    }

    #[test]
    fn track_heights_are_clamped_independently() {
        let video = HeightLimits::VIDEO;
        assert_eq!(video.clamp(10.), video.min);
        assert_eq!(video.clamp(1000.), video.max);
        assert_eq!(video.clamp(90.), 90.);
        assert_eq!(video.clamp(f32::NAN), video.default);
        let audio = HeightLimits::audio(56.);
        assert_eq!(audio.clamp(20.), 56.);
        assert_eq!(audio.clamp(300.), 240.);
        assert_ne!(TrackId::Audio(1), TrackId::Audio(2));
    }
}
