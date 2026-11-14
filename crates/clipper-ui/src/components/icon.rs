//! Line icons from the Windows icon font: Segoe Fluent Icons (Windows 11), with
//! Segoe MDL2 Assets (Windows 10, same codepoints) as fallback. Font glyphs take
//! the text color, so icons follow the theme and accent without image assets.

use gpui::prelude::*;
use gpui::{Div, FontFallbacks, Pixels, div, font};

const ICON_FONT: &str = "Segoe Fluent Icons";
const ICON_FONT_FALLBACK: &str = "Segoe MDL2 Assets";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Camera,
    ChevronDown,
    Cut,
    Drive,
    Folder,
    FolderOpen,
    Filter,
    Game,
    History,
    Keyboard,
    Microphone,
    Monitor,
    More,
    Palette,
    Settings,
    StarFilled,
    Video,
    Volume,
    VolumeOff,
}

impl Icon {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Camera => "\u{E722}",
            Self::ChevronDown => "\u{E70D}",
            Self::Cut => "\u{E8C6}",
            Self::Drive => "\u{EDA2}",
            Self::Folder => "\u{E8B7}",
            Self::FolderOpen => "\u{E838}",
            Self::Filter => "\u{E71C}",
            Self::Game => "\u{E7FC}",
            Self::History => "\u{E81C}",
            Self::Keyboard => "\u{E765}",
            Self::Microphone => "\u{E720}",
            Self::Monitor => "\u{E7F4}",
            Self::More => "\u{E712}",
            Self::Palette => "\u{E790}",
            Self::Settings => "\u{E713}",
            Self::StarFilled => "\u{E735}",
            Self::Video => "\u{E714}",
            Self::Volume => "\u{E767}",
            Self::VolumeOff => "\u{E74F}",
        }
    }
}

/// `icon` at `size`, in the inherited text color (set `text_color` to tint it).
pub fn icon(icon: Icon, size: Pixels) -> Div {
    let mut family = font(ICON_FONT);
    family.fallbacks = Some(FontFallbacks::from_fonts(vec![ICON_FONT_FALLBACK.into()]));
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(size)
        .font(family)
        .text_size(size)
        .line_height(size)
        .child(icon.glyph())
}

/// Accent-tinted square holding a card or row icon (card headers, mixer rows).
pub fn icon_tile(t: &crate::theme::Theme, glyph: Icon, size: Pixels) -> Div {
    div()
        .flex_none()
        .size(size)
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .bg(t.colors.accent_subtle)
        .text_color(t.colors.accent)
        .child(icon(glyph, size * 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_single_private_use_codepoints() {
        for icon in [
            Icon::Camera,
            Icon::Folder,
            Icon::Cut,
            Icon::Settings,
            Icon::Drive,
        ] {
            let mut chars = icon.glyph().chars();
            let c = chars.next().unwrap();
            assert!(chars.next().is_none() && ('\u{E000}'..='\u{F8FF}').contains(&c));
        }
    }
}
