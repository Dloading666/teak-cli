#!/usr/bin/env python3
# Coffee CLI — Icon Pipeline (PNG-based, platform-split)
#
# The brand ships two source artworks with the SAME glyph, differing only in
# how the outer corners are handled:
#
#   * icons/icon-source-windows.png — pre-rounded corners. Windows does NOT
#     round icon corners for you, so the artwork carries its own rounding.
#
#   * icons/icon-source-unix.png   — full-bleed square. macOS and Linux apply
#     the platform's rounded mask themselves, so the source stays opaque
#     edge-to-edge or the rounding would cut into transparent art.
#
# Output split (one source per platform, never mixed):
#   * Windows — icons/icon.ico (+ the MS Store tile set) ← windows source
#   * macOS   — icons/icon.icns                          ← unix source
#   * Linux   — the PNG size set (32/64/128/256/512)     ← unix source
#
# Every output is resized DIRECTLY from its source with Lanczos — no cascade
# downscale — so the small 16/24/32 ICO frames stay sharp in the Windows
# taskbar (that was the old SVG pipeline's whole point, kept here).
#
# Requires Pillow. Run: python scripts/rebuild-icons.py

from pathlib import Path

from PIL import Image

REPO = Path(__file__).resolve().parent.parent
ICONS = REPO / "icons"

WINDOWS_SOURCE = ICONS / "icon-source-windows.png"
UNIX_SOURCE = ICONS / "icon-source-unix.png"

# Sizes Tauri's bundle.icon list expects (see tauri.conf.json). Served to
# Linux desktop entries / icon themes; 512x512 `icon.png` is also the README
# logo and the app's web avatar.
PNG_SIZES = {
    "32x32.png": 32,
    "64x64.png": 64,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "256x256.png": 256,
    "512x512.png": 512,
    "icon.png": 512,
}

# Microsoft Store / UWP tile sizes (Windows-only; not in the active bundle
# targets, kept refreshed so the set never drifts back to the old art).
MS_TILE_SIZES = {
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}

# Microsoft's full recommended .ico size set. Smaller sizes (20, 24, 40) are
# NOT optional — Windows taskbar requests 24 at 100% DPI, 30 at 125%, 36 at
# 150%, 48 at 200%. If the closest match is missing, Windows downscales from
# the next-larger entry with a low-quality bilinear pass — the classic
# "blurry taskbar icon" symptom.
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
# Sizes embedded into the macOS .icns container — Pillow's writer expects
# these exact powers of two.
ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024]


def resize_to(src: Image.Image, size: int) -> Image.Image:
    """Lanczos downscale of the source straight to one exact square size."""
    return src.resize((size, size), Image.LANCZOS)


def build_ico(src: Image.Image, out: Path):
    """Multi-frame ICO: base is the largest frame, the rest ride along in
    append_images at their exact pixel sizes (Pillow re-uses the matching
    sub-image instead of re-encoding from the base — no downscale loss)."""
    frames = [resize_to(src, s) for s in ICO_SIZES]
    frames[-1].save(
        out,
        format="ICO",
        sizes=[(s, s) for s in ICO_SIZES],
        append_images=frames[:-1],
    )


def build_icns(src: Image.Image, out: Path):
    """macOS .icns: Pillow scales the 1024 base down to each required size."""
    resize_to(src, 1024).save(
        out,
        format="ICNS",
        sizes=[(s, s) for s in ICNS_SIZES],
    )


def main():
    if not WINDOWS_SOURCE.exists():
        print(f"error: missing {WINDOWS_SOURCE}", file=__import__("sys").stderr)
        raise SystemExit(1)
    if not UNIX_SOURCE.exists():
        print(f"error: missing {UNIX_SOURCE}", file=__import__("sys").stderr)
        raise SystemExit(1)

    win = Image.open(WINDOWS_SOURCE).convert("RGBA")
    unix = Image.open(UNIX_SOURCE).convert("RGBA")

    print("Linux / generic PNG sizes (from unix source)…")
    for fname, size in PNG_SIZES.items():
        resize_to(unix, size).save(ICONS / fname)
        print(f"  {fname} ({size}x{size})")

    print("MS Store tile sizes (from windows source)…")
    for fname, size in MS_TILE_SIZES.items():
        resize_to(win, size).save(ICONS / fname)
        print(f"  {fname} ({size}x{size})")

    print("building .ico (Windows, from windows source)…")
    build_ico(win, ICONS / "icon.ico")
    print(f"  icon.ico {ICO_SIZES} ({len(ICO_SIZES)} frames embedded)")

    print("building .icns (macOS, from unix source)…")
    build_icns(unix, ICONS / "icon.icns")
    print(f"  icon.icns {ICNS_SIZES}")

    print("done.")


if __name__ == "__main__":
    main()
