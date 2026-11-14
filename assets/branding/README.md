# clipuh Branding Assets

Selected logo: latest purple clapperboard / C / audio waveform / play concept.

## Files

- `assets/branding/clipuh-logo-original.png`: original generated transparent PNG (1254 × 1254).
- `assets/branding/clipuh-logo-master-1024.png`: centered 1024 × 1024 transparent PNG for regular use.
- `assets/branding/clipuh-icon.ico`: multi-resolution Windows ICO (16, 24, 32, 48, 64, 128, 256).
- `assets/branding/clipuh-icon-{size}.png`: PNG exports at several sizes.
- `website/public/favicon.ico`: multi-resolution website favicon.
- `website/public/favicon-16.png`, `favicon-32.png`, `favicon-48.png`: favicon PNGs.
- `website/public/apple-touch-icon.png`: opaque 180 × 180 touch icon.
- `website/public/clipuh-logo-128.png`: 128 × 128 transparent website icon.
- `website/public/clipuh-social-preview-1200x630.png`: basic OG placeholder composition; use only after visual review and metadata setup.

## Instructions for Claude/Codex

1. Inspect actual app icon/build configuration before wiring paths.
2. Use multi-resolution ICO for Windows executable/installer and suitable PNG for GPUI in-app UI where supported.
3. For Astro, copy assets from `website/public/` into the existing website public folder, update favicon tags and use actual paths.
4. Preserve existing appearance/theme settings; branding can stay purple even when user accent changes, unless deliberate brand behavior is requested.
5. Do not re-create or vectorize the logo automatically. The archive contains raster assets, **not a true SVG**.
6. Test 16/32 px visually: the detailed waveform/glow will be less readable at small sizes. A separate simplified small-size logo can be developed later.
7. Never change other app functionality just to install the logo.

This image was generated and resized, not hand-traced. Check appearance before shipping.
