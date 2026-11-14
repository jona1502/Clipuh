"""Derives tintable glyph masks from the approved clipuh icon tiles.

The approved icons (clipuh-icons-named/icons/*.png) are 250x250 decorative
button tiles: a dark (or purple) rounded square with a white line glyph.
The Library already draws its own theme-aware button backgrounds, so the
tile background would double up and its baked colours cannot follow the
Light theme or a custom accent. This script keeps the approved artwork's
glyph only: it un-blends the white strokes from the tile colour into an
alpha mask (white RGB, alpha = coverage), crops it to the glyph with a
small margin and centres it in a square. The app tints the mask with theme
colours at runtime.

Checkbox-checked yields two masks: the filled box (`checkbox-checked-box`)
and the tick on top of it (`checkbox-checked-tick`), so the box can follow
the accent colour.

Usage (from the repository root):
    python -I scripts/assets/extract_icon_glyphs.py
Requires Pillow. Output: assets/icons/library/*.png (128x128 RGBA).
"""

import os
import sys

from PIL import Image

SOURCE = os.path.join("clipuh-icons-named", "icons")
TARGET = os.path.join("assets", "icons", "library")
SIZE = 128
# One scale for every icon so relative sizes and stroke weights stay as in
# the approved sheet: REFERENCE source pixels (the largest glyphs are about
# 106 px) span FILL of the output square.
REFERENCE = 110
FILL = 0.86
ICONS = [
    "play", "edit", "favorite", "delete", "open-folder", "more-horizontal",
    "search", "filter", "chevron-down", "grid-view", "list-view",
    "checkbox-unchecked", "checkbox-checked",
]
GLYPH_MIN = 235  # min(R,G,B) of solid glyph pixels (excludes the tile rim)
REGION_MARGIN = 6
KNEE = 10


def whiteness(px):
    return min(px[0], px[1], px[2]) if px[3] > 0 else 0


def purple(px):
    # Blue/red over green: high on the accent box, low on the dark tile and
    # on white.
    return max(0, (px[0] + px[2]) // 2 - px[1]) if px[3] > 0 else 0


def bbox(img, test):
    w, h = img.size
    pix = img.load()
    hits = [(x, y) for x in range(w) for y in range(h) if pix[x, y][3] > 200 and test(pix[x, y])]
    xs = [x for x, _ in hits]
    ys = [y for _, y in hits]
    if not xs:
        raise SystemExit("no glyph found")
    return min(xs), min(ys), max(xs) + 1, max(ys) + 1


def brightness(px):
    return max(px[0], px[1], px[2]) if px[3] > 0 else 0


def percentile(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * len(values)))]


def border_level(img, box, measure):
    x0, y0, x1, y1 = box
    pix = img.load()
    values = sorted(
        measure(pix[x, y])
        for x in range(x0, x1)
        for y in range(y0, y1)
        if x in (x0, x1 - 1) or y in (y0, y1 - 1)
    )
    return values[len(values) // 2]


def mask(img, box, measure, low, high):
    x0, y0, x1, y1 = box
    out = Image.new("L", (x1 - x0, y1 - y0), 0)
    pix, dst = img.load(), out.load()
    # A small knee above the background level removes the faint gradient of
    # the tile behind the glyph.
    low = low + KNEE
    span = max(1, high - low)
    for x in range(x0, x1):
        for y in range(y0, y1):
            a = (measure(pix[x, y]) - low) * 255 // span
            dst[x - x0, y - y0] = max(0, min(255, a))
    return out


def save(alpha, name):
    """Centres `alpha` in a square at the common scale."""
    scale = SIZE * FILL / REFERENCE
    w, h = alpha.size
    scaled = alpha.resize(
        (max(1, round(w * scale)), max(1, round(h * scale))), Image.LANCZOS
    )
    canvas = Image.new("L", (SIZE, SIZE), 0)
    canvas.paste(scaled, ((SIZE - scaled.width) // 2, (SIZE - scaled.height) // 2))
    rgba = Image.new("RGBA", (SIZE, SIZE), (255, 255, 255, 0))
    rgba.putalpha(canvas)
    rgba.save(os.path.join(TARGET, name + ".png"), optimize=True)


def grow(box, margin, size):
    x0, y0, x1, y1 = box
    w, h = size
    return max(0, x0 - margin), max(0, y0 - margin), min(w, x1 + margin), min(h, y1 + margin)


def main():
    os.makedirs(TARGET, exist_ok=True)
    for name in ICONS:
        img = Image.open(os.path.join(SOURCE, name + ".png")).convert("RGBA")
        if name == "checkbox-checked":
            # Box: the accent-coloured square; tick: white strokes on it.
            box = grow(bbox(img, lambda p: purple(p) > 60), REGION_MARGIN, img.size)
            pixels = list(img.crop(box).get_flattened_data())
            on_box = [p for p in pixels if purple(p) > 60 and whiteness(p) < 200]
            # Box coverage: brightest channel, tile background -> box colour.
            low = border_level(img, box, brightness)
            solid = percentile([brightness(p) for p in on_box], 0.5)
            box_alpha = mask(img, box, brightness, low, solid)
            # Tick: white above the box colour.
            tick = mask(img, box, whiteness, percentile([whiteness(p) for p in on_box], 0.95), 255)
            # The tick sits on the box: the box is opaque under it.
            filled = Image.new("L", box_alpha.size)
            filled.putdata(
                [max(a, b) for a, b in zip(box_alpha.get_flattened_data(), tick.get_flattened_data())]
            )
            save(filled, "checkbox-checked-box")
            save(tick, "checkbox-checked-tick")
            continue
        region = grow(bbox(img, lambda p: whiteness(p) > GLYPH_MIN), REGION_MARGIN, img.size)
        low = border_level(img, region, whiteness)
        save(mask(img, region, whiteness, low, 255), name)
    print("wrote", len(ICONS) + 1, "masks to", TARGET, file=sys.stderr)


if __name__ == "__main__":
    main()
