//! Approved clipuh raster icons, tinted with theme colours.
//!
//! The approved icon set (`clipuh-icons-named/`) ships as decorative button
//! tiles with baked-in dark/purple backgrounds. Library controls already draw
//! their own theme-aware button surfaces, so only the glyph of each tile is
//! used: `scripts/assets/extract_icon_glyphs.py` turns the tiles into alpha
//! masks (`assets/icons/library/*.png`, 128 px). Here a mask is resampled to
//! the exact device-pixel size it is drawn at (sharp at every DPI) and tinted
//! with a theme colour, so Dark, Light and custom accents stay readable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use gpui::{Div, Hsla, ImageSource, Img, Pixels, Rgba, SharedString, Window, div, img, prelude::*};
use image::imageops::{self, FilterType};
use image::{Frame, GrayImage, RgbaImage};

use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RasterIcon {
    Play,
    Edit,
    Favorite,
    Delete,
    OpenFolder,
    More,
    Search,
    Filter,
    ChevronDown,
    GridView,
    ListView,
    CheckboxUnchecked,
    CheckboxCheckedBox,
    CheckboxCheckedTick,
}

impl RasterIcon {
    pub const ALL: [Self; 14] = [
        Self::Play,
        Self::Edit,
        Self::Favorite,
        Self::Delete,
        Self::OpenFolder,
        Self::More,
        Self::Search,
        Self::Filter,
        Self::ChevronDown,
        Self::GridView,
        Self::ListView,
        Self::CheckboxUnchecked,
        Self::CheckboxCheckedBox,
        Self::CheckboxCheckedTick,
    ];

    fn png(self) -> &'static [u8] {
        macro_rules! mask {
            ($name:literal) => {
                include_bytes!(concat!("../../../../assets/icons/library/", $name, ".png"))
            };
        }
        match self {
            Self::Play => mask!("play"),
            Self::Edit => mask!("edit"),
            Self::Favorite => mask!("favorite"),
            Self::Delete => mask!("delete"),
            Self::OpenFolder => mask!("open-folder"),
            Self::More => mask!("more-horizontal"),
            Self::Search => mask!("search"),
            Self::Filter => mask!("filter"),
            Self::ChevronDown => mask!("chevron-down"),
            Self::GridView => mask!("grid-view"),
            Self::ListView => mask!("list-view"),
            Self::CheckboxUnchecked => mask!("checkbox-unchecked"),
            Self::CheckboxCheckedBox => mask!("checkbox-checked-box"),
            Self::CheckboxCheckedTick => mask!("checkbox-checked-tick"),
        }
    }

    /// Coverage mask (alpha channel of the asset), decoded once.
    fn mask(self) -> &'static GrayImage {
        static MASKS: OnceLock<Vec<GrayImage>> = OnceLock::new();
        let masks = MASKS.get_or_init(|| {
            Self::ALL
                .iter()
                .map(|icon| {
                    image::load_from_memory_with_format(icon.png(), image::ImageFormat::Png)
                        .map(|image| {
                            let rgba = image.to_rgba8();
                            GrayImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                                image::Luma([rgba.get_pixel(x, y)[3]])
                            })
                        })
                        .unwrap_or_else(|_| GrayImage::new(1, 1))
                })
                .collect()
        });
        let index = Self::ALL.iter().position(|icon| *icon == self).unwrap_or(0);
        &masks[index]
    }
}

/// Upper bound of cached tinted bitmaps (icons x sizes x colours; a session
/// normally needs a few dozen). Reached only after many theme, accent or DPI
/// changes; the cache then starts over. Dropped bitmaps stay in the window's
/// sprite atlas (a few KB each) until the window closes, because the render
/// paths that draw icons only hold `&Window`.
const CACHE_LIMIT: usize = 256;

type Key = (RasterIcon, u32, [u8; 4]);

thread_local! {
    static CACHE: RefCell<HashMap<Key, Arc<gpui::RenderImage>>> = RefCell::new(HashMap::new());
}

fn rgba8(color: Hsla) -> [u8; 4] {
    let Rgba { r, g, b, a } = color.to_rgb();
    [r, g, b, a].map(|c| (c.clamp(0., 1.) * 255.).round() as u8)
}

/// Tinted BGRA pixels (GPUI's `RenderImage` byte order), straight alpha.
fn tint(mask: &GrayImage, side: u32, color: [u8; 4]) -> RgbaImage {
    let scaled = if mask.width() == side && mask.height() == side {
        mask.clone()
    } else {
        imageops::resize(mask, side, side, FilterType::Lanczos3)
    };
    let [r, g, b, a] = color;
    RgbaImage::from_fn(side, side, |x, y| {
        let coverage = u16::from(scaled.get_pixel(x, y)[0]);
        let alpha = (coverage * u16::from(a) + 127) / 255;
        image::Rgba([b, g, r, alpha as u8])
    })
}

/// Device pixels of a square `size` icon on this window.
fn device_side(size: Pixels, window: &Window) -> u32 {
    (f32::from(size) * window.scale_factor())
        .round()
        .clamp(1., 512.) as u32
}

fn bitmap(icon: RasterIcon, size: Pixels, color: Hsla, window: &Window) -> Arc<gpui::RenderImage> {
    let key = (icon, device_side(size, window), rgba8(color));
    if let Some(found) = CACHE.with(|cache| cache.borrow().get(&key).cloned()) {
        return found;
    }
    let image = Arc::new(gpui::RenderImage::new(vec![Frame::new(tint(
        icon.mask(),
        key.1,
        key.2,
    ))]));
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, image.clone());
    });
    image
}

/// One approved icon, `size` square, tinted `color`.
pub fn raster_icon(icon: RasterIcon, size: Pixels, color: Hsla, window: &Window) -> Img {
    img(ImageSource::Render(bitmap(icon, size, color, window)))
        .flex_none()
        .size(size)
}

/// [`raster_icon`] that switches to `hover` while the element named `group`
/// (an ancestor with `.group(group)`) is hovered: bitmaps cannot follow
/// `text_color`, so both tints are laid on top of each other.
pub fn raster_icon_hover(
    icon: RasterIcon,
    size: Pixels,
    color: Hsla,
    hover: Hsla,
    group: impl Into<SharedString>,
    window: &Window,
) -> Div {
    let group = group.into();
    div()
        .flex_none()
        .relative()
        .size(size)
        .child(
            div()
                .absolute()
                .inset_0()
                .group_hover(group.clone(), |s| s.invisible())
                .child(raster_icon(icon, size, color, window)),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .invisible()
                .group_hover(group, |s| s.visible())
                .child(raster_icon(icon, size, hover, window)),
        )
}

/// Selection checkbox from the approved checkbox icons: an `unchecked`
/// coloured outline, or an accent-filled box with a tick when checked.
pub fn raster_checkbox(
    t: &Theme,
    checked: bool,
    size: Pixels,
    unchecked: Hsla,
    window: &Window,
) -> Div {
    let layer = |child: Img| div().absolute().inset_0().child(child);
    let base = div().flex_none().relative().size(size);
    if checked {
        base.child(layer(raster_icon(
            RasterIcon::CheckboxCheckedBox,
            size,
            t.colors.accent,
            window,
        )))
        .child(layer(raster_icon(
            RasterIcon::CheckboxCheckedTick,
            size,
            t.colors.accent_foreground,
            window,
        )))
    } else {
        base.child(layer(raster_icon(
            RasterIcon::CheckboxUnchecked,
            size,
            unchecked,
            window,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mask_decodes_to_a_non_empty_square() {
        for icon in RasterIcon::ALL {
            let mask = icon.mask();
            assert_eq!((mask.width(), mask.height()), (128, 128), "{icon:?}");
            let covered = mask.pixels().filter(|p| p[0] > 128).count();
            assert!(covered > 100, "{icon:?} has no glyph");
            // Transparent outside the glyph: the tile background is gone.
            for corner in [(0, 0), (127, 0), (0, 127), (127, 127)] {
                assert_eq!(mask.get_pixel(corner.0, corner.1)[0], 0, "{icon:?}");
            }
        }
    }

    #[test]
    fn tint_writes_bgra_with_scaled_coverage() {
        let mut mask = GrayImage::new(2, 2);
        mask.put_pixel(0, 0, image::Luma([255]));
        mask.put_pixel(1, 0, image::Luma([128]));
        let tinted = tint(&mask, 2, [10, 20, 30, 255]);
        assert_eq!(tinted.get_pixel(0, 0).0, [30, 20, 10, 255]);
        assert_eq!(tinted.get_pixel(1, 0).0, [30, 20, 10, 128]);
        assert_eq!(tinted.get_pixel(0, 1).0[3], 0);
        // Colour alpha multiplies coverage.
        assert_eq!(tint(&mask, 2, [0, 0, 0, 128]).get_pixel(0, 0).0[3], 128);
    }

    #[test]
    fn tint_resamples_to_the_requested_size() {
        let mask = RasterIcon::Play.mask();
        let small = tint(mask, 27, [255, 255, 255, 255]);
        assert_eq!((small.width(), small.height()), (27, 27));
        assert!(
            small.pixels().any(|p| p[3] > 200),
            "glyph survives downscaling"
        );
    }
}
