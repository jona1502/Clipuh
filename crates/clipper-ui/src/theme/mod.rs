//! Design system: semantic color tokens, density-scaled layout tokens and the
//! active [`Theme`], stored as a GPUI global.
//!
//! Views read the theme once per render (`let t = *cx.theme();`, a cheap
//! copy) and pass `&t` to components. Changing appearance replaces the global
//! and refreshes the windows; nothing polls.

mod density;
mod palette;

pub use density::Metrics;
pub use palette::{Brightness, Palette};

use clipper_storage::{AppearanceSettings, DEFAULT_ACCENT, Theme as ThemeMode, parse_hex_color};
use gpui::{App, Global, WindowAppearance};

/// The resolved appearance every view and component styles itself from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub brightness: Brightness,
    pub colors: Palette,
    pub metrics: Metrics,
}

impl Global for Theme {}

impl Theme {
    /// Resolves `System` with the OS appearance; an invalid accent falls back
    /// to the default (settings validation normally prevents that).
    pub fn new(settings: &AppearanceSettings, system: WindowAppearance) -> Self {
        let brightness = match settings.mode {
            ThemeMode::Dark => Brightness::Dark,
            ThemeMode::Light => Brightness::Light,
            ThemeMode::System => match system {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Brightness::Light,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Brightness::Dark,
            },
        };
        let accent = parse_hex_color(&settings.accent_color)
            .or_else(|| parse_hex_color(DEFAULT_ACCENT))
            .unwrap_or(0x9b5cf6);
        Self {
            brightness,
            colors: Palette::new(brightness, accent),
            metrics: Metrics::new(settings.density),
        }
    }
}

/// Installs or updates the global theme. Windows re-render only when the
/// resolved theme actually changed, e.g. a System-mode OS switch that keeps
/// the same brightness costs nothing.
pub fn apply(settings: &AppearanceSettings, cx: &mut App) {
    let theme = Theme::new(settings, cx.window_appearance());
    if cx.try_global::<Theme>() != Some(&theme) {
        cx.set_global(theme);
        cx.refresh_windows();
    }
}

/// Access to the active theme from any GPUI context.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}
