//! Design tokens. Colors are `0xRRGGBB`; translucent overlays are `0xRRGGBBAA` (use `rgba`).
//! Spacing and radii use GPUI's built-in scale (`gap_2`, `rounded_lg`, ...) so there is one scale.

// Surfaces, darkest to lightest. Neutral with a faint cool tint; never pure black.
pub const BACKGROUND: u32 = 0x0e0e11;
pub const SIDEBAR: u32 = 0x121216;
pub const SURFACE: u32 = 0x17171c;
pub const SURFACE_RAISED: u32 = 0x1e1e24;
pub const SURFACE_HOVER: u32 = 0x24242b;

pub const BORDER: u32 = 0x25252c;
pub const BORDER_STRONG: u32 = 0x33333b;

// Text hierarchy.
pub const TEXT: u32 = 0xececf1;
pub const TEXT_SECONDARY: u32 = 0xa0a0ab;
pub const TEXT_TERTIARY: u32 = 0x6a6a75;

// Brand accent, used sparingly: active indicators, focus rings, primary actions.
pub const ACCENT: u32 = 0x8c7bff;
pub const ACCENT_HOVER: u32 = 0x9d8eff;
pub const TEXT_ON_ACCENT: u32 = 0xffffff;
pub const ACCENT_SUBTLE: u32 = 0x8c7bff24;
pub const FOCUS_RING: u32 = 0x8c7bffaa;
// Dims the shell behind overlays.
pub const SCRIM: u32 = 0x00000080;

// Status colors.
pub const SUCCESS: u32 = 0x4ade80;
pub const WARNING: u32 = 0xfbbf24;
pub const DANGER: u32 = 0xf87171;
pub const NEUTRAL: u32 = 0x7a7a86;
// Solid fill for the live-recording dot and the stop button.
pub const RECORDING: u32 = 0xe5484d;
pub const RECORDING_HOVER: u32 = 0xee5a5f;
