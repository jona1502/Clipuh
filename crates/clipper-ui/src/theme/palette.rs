//! Semantic color tokens for the dark and light themes, plus the colors
//! derived from the user's accent.

use gpui::{Hsla, Rgba, rgb};

/// Every color a view or component may use. Views never hardcode hex values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    // Surfaces, from the window background up.
    pub background: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub surface_raised: Hsla,
    pub surface_hover: Hsla,
    pub surface_active: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    // Text hierarchy.
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_disabled: Hsla,
    // Accent, used sparingly: primary actions, selection, focus, progress.
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_subtle: Hsla,
    pub accent_foreground: Hsla,
    pub focus_ring: Hsla,
    // Status.
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub neutral: Hsla,
    /// Solid fill of the live-recording dot and the stop button.
    pub recording: Hsla,
    pub recording_hover: Hsla,
    /// Text on `recording` fills (stop/danger buttons); independent of the accent.
    pub recording_foreground: Hsla,
    /// Dims the shell behind overlays.
    pub scrim: Hsla,
    /// Video and image letterboxing; black in both themes.
    pub media_background: Hsla,
}

/// Whether the resolved theme is light or dark (System resolves to one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brightness {
    Dark,
    Light,
}

impl Palette {
    pub fn new(brightness: Brightness, accent: u32) -> Self {
        let base = match brightness {
            Brightness::Dark => Base::DARK,
            Brightness::Light => Base::LIGHT,
        };
        let accent_rgb = rgb(accent);
        // Hover moves toward the text color: lighter on dark, darker on light.
        let toward = match brightness {
            Brightness::Dark => rgb(0xffffff),
            Brightness::Light => rgb(0x000000),
        };
        Self {
            background: rgb(base.background).into(),
            sidebar: rgb(base.sidebar).into(),
            surface: rgb(base.surface).into(),
            surface_raised: rgb(base.surface_raised).into(),
            surface_hover: rgb(base.surface_hover).into(),
            surface_active: rgb(base.surface_active).into(),
            border: rgb(base.border).into(),
            border_strong: rgb(base.border_strong).into(),
            text: rgb(base.text).into(),
            text_secondary: rgb(base.text_secondary).into(),
            text_muted: rgb(base.text_muted).into(),
            text_disabled: rgb(base.text_disabled).into(),
            accent: accent_rgb.into(),
            accent_hover: mix(accent_rgb, toward, 0.12).into(),
            accent_subtle: with_alpha(accent_rgb, base.accent_subtle_alpha).into(),
            accent_foreground: foreground_for(accent_rgb).into(),
            focus_ring: with_alpha(accent_rgb, 0.7).into(),
            success: rgb(base.success).into(),
            warning: rgb(base.warning).into(),
            danger: rgb(base.danger).into(),
            neutral: rgb(base.neutral).into(),
            recording: rgb(0xe5484d).into(),
            recording_hover: rgb(0xee5a5f).into(),
            recording_foreground: rgb(0xffffff).into(),
            scrim: with_alpha(rgb(0x000000), base.scrim_alpha).into(),
            media_background: rgb(0x000000).into(),
        }
    }
}

/// The non-accent colors of one brightness.
struct Base {
    background: u32,
    sidebar: u32,
    surface: u32,
    surface_raised: u32,
    surface_hover: u32,
    surface_active: u32,
    border: u32,
    border_strong: u32,
    text: u32,
    text_secondary: u32,
    text_muted: u32,
    text_disabled: u32,
    success: u32,
    warning: u32,
    danger: u32,
    neutral: u32,
    accent_subtle_alpha: f32,
    scrim_alpha: f32,
}

impl Base {
    /// Deep charcoal with a faint cool tint; never pure black.
    const DARK: Self = Self {
        background: 0x0d0e14,
        sidebar: 0x111219,
        surface: 0x171821,
        surface_raised: 0x1d1e2a,
        surface_hover: 0x252636,
        surface_active: 0x2c2d3f,
        border: 0x262735,
        border_strong: 0x303142,
        text: 0xf5f5f7,
        text_secondary: 0xa1a1b0,
        text_muted: 0x868699,
        text_disabled: 0x55556a,
        success: 0x4ade80,
        warning: 0xfbbf24,
        danger: 0xf87171,
        neutral: 0x7a7a8c,
        accent_subtle_alpha: 0.16,
        scrim_alpha: 0.5,
    };

    /// Off-white canvas with white cards; borders and muted text stay
    /// readable (muted text is >= 4:1 on every surface).
    const LIGHT: Self = Self {
        background: 0xf3f3f6,
        sidebar: 0xebebf0,
        surface: 0xffffff,
        surface_raised: 0xf6f6f9,
        surface_hover: 0xececf2,
        surface_active: 0xe3e3eb,
        border: 0xdcdce4,
        border_strong: 0xc6c6d2,
        text: 0x16161d,
        text_secondary: 0x4f4f60,
        text_muted: 0x676779,
        text_disabled: 0xa0a0ae,
        success: 0x15803d,
        warning: 0xb45309,
        danger: 0xdc2626,
        neutral: 0x6b6b7b,
        accent_subtle_alpha: 0.12,
        scrim_alpha: 0.3,
    };
}

fn with_alpha(color: Rgba, a: f32) -> Rgba {
    Rgba { a, ..color }
}

fn mix(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let lerp = |a: f32, b: f32| a + (b - a) * amount;
    Rgba {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: 1.0,
    }
}

/// WCAG relative luminance.
pub(crate) fn luminance(color: Rgba) -> f32 {
    let channel = |c: f32| {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// WCAG contrast ratio between two opaque colors (1.0..=21.0).
pub(crate) fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// White or near-black text on an accent fill, whichever reads better.
fn foreground_for(accent: Rgba) -> Rgba {
    let (white, ink) = (rgb(0xffffff), rgb(0x111118));
    if contrast(accent, white) >= contrast(accent, ink) {
        white
    } else {
        ink
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(color: Hsla) -> Rgba {
        color.to_rgb()
    }

    #[test]
    fn text_contrast_meets_wcag_aa_on_every_surface() {
        for brightness in [Brightness::Dark, Brightness::Light] {
            let p = Palette::new(brightness, 0x9b5cf6);
            for surface in [
                p.background,
                p.sidebar,
                p.surface,
                p.surface_raised,
                p.surface_hover,
            ] {
                for (name, text, min) in [
                    ("text", p.text, 7.0),
                    ("secondary", p.text_secondary, 4.5),
                    ("muted", p.text_muted, 4.0),
                ] {
                    let ratio = contrast(opaque(text), opaque(surface));
                    assert!(ratio >= min, "{brightness:?} {name} {ratio:.2} < {min}");
                }
            }
        }
    }

    #[test]
    fn borders_are_visible_against_surfaces() {
        for brightness in [Brightness::Dark, Brightness::Light] {
            let p = Palette::new(brightness, 0x9b5cf6);
            let ratio = contrast(opaque(p.border_strong), opaque(p.surface));
            assert!(ratio >= 1.3, "{brightness:?} border_strong {ratio:.2}");
        }
    }

    #[test]
    fn accent_foreground_stays_readable_for_presets() {
        for accent in [
            0x9b5cf6, 0x3b82f6, 0x06b6d4, 0x22c55e, 0xf97316, 0xec4899, 0xef4444, 0xffffff,
            0x000000, 0xffff00,
        ] {
            let p = Palette::new(Brightness::Dark, accent);
            let ratio = contrast(opaque(p.accent_foreground), opaque(p.accent));
            assert!(ratio >= 3.0, "{accent:06x}: {ratio:.2}");
        }
    }

    #[test]
    fn accent_derivatives_follow_the_accent() {
        let p = Palette::new(Brightness::Dark, 0x22c55e);
        // Hsla round-trips can move a channel by one step.
        let (got, want) = (opaque(p.accent), rgb(0x22c55e));
        assert!((got.r - want.r).abs() < 0.01 && (got.g - want.g).abs() < 0.01);
        assert!(p.accent_subtle.a < 0.2);
        assert!(luminance(opaque(p.accent_hover)) > luminance(opaque(p.accent)));
        let light = Palette::new(Brightness::Light, 0x22c55e);
        assert!(luminance(opaque(light.accent_hover)) < luminance(opaque(light.accent)));
    }
}
