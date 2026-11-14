//! Layout tokens that scale with the UI density setting. Radii and type sizes
//! stay fixed; only heights, padding and gaps change.

use clipper_storage::UiDensity;
use gpui::{Pixels, px};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Buttons, dropdown triggers, text inputs.
    pub control_height: Pixels,
    /// Horizontal padding inside controls.
    pub control_padding: Pixels,
    /// Small controls such as chips and compact toast buttons.
    pub control_height_small: Pixels,
    /// Sidebar, palette and menu rows.
    pub row_height: Pixels,
    /// Vertical padding of a settings/property row.
    pub row_padding: Pixels,
    /// Padding inside cards and panels.
    pub card_padding: Pixels,
    /// Gap between controls in a row.
    pub gap: Pixels,
    /// Gap between sections of a page.
    pub section_gap: Pixels,
}

impl Metrics {
    pub fn new(density: UiDensity) -> Self {
        match density {
            UiDensity::Compact => Self {
                control_height: px(26.),
                control_padding: px(10.),
                control_height_small: px(22.),
                row_height: px(28.),
                row_padding: px(8.),
                card_padding: px(12.),
                gap: px(6.),
                section_gap: px(18.),
            },
            UiDensity::Default => Self {
                control_height: px(30.),
                control_padding: px(12.),
                control_height_small: px(26.),
                row_height: px(32.),
                row_padding: px(12.),
                card_padding: px(16.),
                gap: px(8.),
                section_gap: px(24.),
            },
            UiDensity::Relaxed => Self {
                control_height: px(36.),
                control_padding: px(14.),
                control_height_small: px(30.),
                row_height: px(38.),
                row_padding: px(16.),
                card_padding: px(20.),
                gap: px(10.),
                section_gap: px(32.),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_scales_every_token_monotonically() {
        let [c, d, r] =
            [UiDensity::Compact, UiDensity::Default, UiDensity::Relaxed].map(Metrics::new);
        let tokens = |m: Metrics| {
            [
                m.control_height,
                m.control_padding,
                m.control_height_small,
                m.row_height,
                m.row_padding,
                m.card_padding,
                m.gap,
                m.section_gap,
            ]
        };
        for ((c, d), r) in tokens(c).into_iter().zip(tokens(d)).zip(tokens(r)) {
            assert!(c < d && d < r, "{c:?} {d:?} {r:?}");
        }
    }
}
