#!/usr/bin/env python3
"""Render Teak CLI dock icons in the same metallic-on-dark language as the
current Coffee CLI sources (full-bleed square for unix, pre-rounded for Windows).

Run from repo root: python3 brand/render_teak_icon.py
"""

from __future__ import annotations

import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = Path(__file__).resolve().parent
SIZE = 1024
# Matches Coffee CLI's windows source rounding (~0.225 of the canvas).
CORNER_R = int(SIZE * 0.225)


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def mix_rgb(c1: tuple[int, int, int], c2: tuple[int, int, int], t: float) -> tuple[int, int, int]:
    t = max(0.0, min(1.0, t))
    return (
        int(lerp(c1[0], c2[0], t)),
        int(lerp(c1[1], c2[1], t)),
        int(lerp(c1[2], c2[2], t)),
    )


def background(size: int) -> Image.Image:
    """Dark radial wash, lighter at upper-left — same lighting as the coffee icon."""
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    nx = x / size
    ny = y / size
    d = np.hypot(nx - 0.30, ny - 0.16)
    t = np.clip(d / 1.05, 0.0, 1.0) ** 0.82
    # Warm-black, not pure cool gray — teak sits on a slightly brown dark.
    light = np.array([52, 48, 44], dtype=np.float32)
    dark = np.array([10, 9, 8], dtype=np.float32)
    rgb = light * (1.0 - t[..., None]) + dark * t[..., None]
    img = np.concatenate([rgb, np.full((size, size, 1), 255.0)], axis=2).astype(np.uint8)
    return Image.fromarray(img, "RGBA")


def t_mask(size: int) -> Image.Image:
    """Chunky T-table: slab top + pedestal. Optically centered."""
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    r = int(size * 0.035)
    # Tabletop / deck
    top = (
        int(size * 0.193),
        int(size * 0.340),
        int(size * 0.807),
        int(size * 0.476),
    )
    # Pedestal — overlaps the slab so the join is solid
    leg = (
        int(size * 0.418),
        int(size * 0.445),
        int(size * 0.582),
        int(size * 0.805),
    )
    d.rounded_rectangle(top, radius=r, fill=255)
    d.rounded_rectangle(leg, radius=r, fill=255)
    return m


def metallic_fill(size: int, mask: Image.Image) -> Image.Image:
    """Left-lit brushed metal, slightly warm so it doesn't clone the coffee chrome."""
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    nx = x / (size - 1)
    ny = y / (size - 1)
    # Primary left→right falloff + a vertical sheen through the slab
    sheen = 0.55 + 0.45 * np.exp(-((nx - 0.28) ** 2) / 0.10) * (0.85 + 0.15 * np.sin(ny * math.pi))
    sheen = sheen - 0.18 * nx - 0.06 * ny
    sheen = np.clip(sheen, 0.0, 1.0)

    highlight = np.array([236, 228, 214], dtype=np.float32)  # warm silver
    mid = np.array([186, 168, 140], dtype=np.float32)        # teak-lit metal
    shadow = np.array([118, 102, 82], dtype=np.float32)

    t = 1.0 - sheen
    # two-stop: highlight→mid for the bright half, mid→shadow for the rest
    k = np.clip(t * 1.15, 0.0, 1.0)
    rgb = np.where(
        k[..., None] < 0.5,
        highlight * (1 - k[..., None] * 2) + mid * (k[..., None] * 2),
        mid * (1 - (k[..., None] - 0.5) * 2) + shadow * ((k[..., None] - 0.5) * 2),
    )
    alpha = np.array(mask, dtype=np.float32)[..., None]
    rgba = np.concatenate([rgb, alpha], axis=2).astype(np.uint8)
    return Image.fromarray(rgba, "RGBA")


def bevel(mask: Image.Image, fill: Image.Image) -> Image.Image:
    """Thin light rim + bottom-right inner shade, matching the coffee glyph."""
    # Outer rim: dilate, fill with pale metal, stamp the glyph back on top.
    rim = mask.filter(ImageFilter.MaxFilter(7))
    rim_img = Image.new("RGBA", mask.size, (220, 214, 204, 0))
    rim_px = np.array(rim_img)
    rim_px[..., 3] = np.array(rim)
    rim_img = Image.fromarray(rim_px, "RGBA")

    # Inner shade along the lower-right contour
    shifted = Image.new("L", mask.size, 0)
    shifted.paste(mask, (6, 7))
    inner = ImageChops.subtract(mask, shifted)
    inner = inner.filter(ImageFilter.GaussianBlur(1.2))
    shade = Image.new("RGBA", mask.size, (40, 32, 24, 0))
    sp = np.array(shade)
    sp[..., 3] = (np.array(inner).astype(np.float32) * 0.55).astype(np.uint8)
    shade = Image.fromarray(sp, "RGBA")

    # Top-left inner highlight
    shifted_hl = Image.new("L", mask.size, 0)
    shifted_hl.paste(mask, (-5, -6))
    hl = ImageChops.subtract(mask, shifted_hl)
    hl = hl.filter(ImageFilter.GaussianBlur(1.0))
    shine = Image.new("RGBA", mask.size, (255, 250, 240, 0))
    hp = np.array(shine)
    hp[..., 3] = (np.array(hl).astype(np.float32) * 0.40).astype(np.uint8)
    shine = Image.fromarray(hp, "RGBA")

    out = Image.alpha_composite(rim_img, fill)
    out = Image.alpha_composite(out, shade)
    out = Image.alpha_composite(out, shine)
    # Re-clip to dilated rim so bevel doesn't leak
    clipped = Image.new("RGBA", mask.size, (0, 0, 0, 0))
    clipped.paste(out, (0, 0), rim)
    return clipped


def round_corners(img: Image.Image, radius: int) -> Image.Image:
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, img.size[0] - 1, img.size[1] - 1], radius=radius, fill=255
    )
    out = img.copy()
    out.putalpha(mask)
    return out


def compose(rounded: bool) -> Image.Image:
    bg = background(SIZE)
    mask = t_mask(SIZE)
    fill = metallic_fill(SIZE, mask)
    glyph = bevel(mask, fill)
    out = Image.alpha_composite(bg, glyph)
    if rounded:
        out = round_corners(out, CORNER_R)
    return out


def main() -> None:
    unix = compose(rounded=False)
    windows = compose(rounded=True)

    unix.save(HERE / "teak-icon-unix.png")
    windows.save(HERE / "teak-icon-windows.png")
    windows.resize((512, 512), Image.LANCZOS).save(HERE / "teak-icon-512.png")
    unix.resize((32, 32), Image.LANCZOS).save(HERE / "teak-icon-32.png")
    print("wrote:")
    print(" ", HERE / "teak-icon-unix.png")
    print(" ", HERE / "teak-icon-windows.png")
    print(" ", HERE / "teak-icon-512.png")
    print(" ", HERE / "teak-icon-32.png")


if __name__ == "__main__":
    main()
