#!/usr/bin/env python3
# Coffee CLI — Icon Pipeline (PNG-based, platform-split)
#
# The brand ships two source artworks with the SAME glyph, differing only in
# how the outer corners are handled:
#
#   * icons/icon-source-windows.png — pre-rounded corners. Windows does NOT
#     round icon corners for you, so the artwork carries its own rounding.
#
#   * icons/icon-source-unix.png   — full-bleed square. macOS, Linux, iOS and
#     Android all apply the platform's rounded mask themselves, so the source
#     stays opaque edge-to-edge or the rounding would cut into transparent art.
#
# Output split (one source per platform, never mixed):
#   * Windows  — icons/icon.ico + MS Store tiles          ← windows source
#   * macOS    — icons/icon.icns                          ← unix source
#   * Linux    — the PNG size set (32/64/128/256/512)     ← unix source
#   * iOS      — icons/ios/AppIcon-*                      ← unix source
#   * Android  — icons/android/mipmap-*/                  ← unix source
#   * Website  — Web-Home/icons/icon.ico + favicon.svg    ← windows source
#                (a rounded icon with transparent corners reads best as a
#                browser-tab favicon)
#
# Every output is resized DIRECTLY from its source with Lanczos — no cascade
# downscale — so the small 16/24/32 ICO frames stay sharp in the Windows
# taskbar (that was the old SVG pipeline's whole point, kept here).
#
# Requires Pillow. Run: python scripts/rebuild-icons.py

import base64
import io
import re
from pathlib import Path

from PIL import Image

REPO = Path(__file__).resolve().parent.parent
ICONS = REPO / "icons"
WEB = REPO / "Web-Home" / "icons"

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

# iOS AppIcon filenames are discovered by globbing icons/ios/*.png and parsing
# the base size + @Nx scale out of the name (see ios_icon_px()).
IOS_DIR = ICONS / "ios"

# Android densities → launcher-icon px (mdpi/hdpi/xhdpi/xxhdpi/xxxhdpi).
ANDROID_LAUNCHER = {
    "mdpi": 48, "hdpi": 72, "xhdpi": 96, "xxhdpi": 144, "xxxhdpi": 192,
}
# Android adaptive-icon foreground canvases. The safe zone is the inner
# 66/108 dp of the 108 dp canvas — the visible art must sit inside it or the
# launcher's round/squircle mask crops it.
ANDROID_FOREGROUND = {
    "mdpi": 108, "hdpi": 162, "xhdpi": 216, "xxhdpi": 324, "xxxhdpi": 432,
}
ANDROID_SAFE_FRACTION = 66 / 108

# Website favicon: keep it lean — a handful of frames, not the full 10-frame
# desktop set (every page load fetches it).
FAVICON_ICO_SIZES = [16, 32, 48, 64, 128]


def resize_to(src: Image.Image, size: int) -> Image.Image:
    """Lanczos downscale of the source straight to one exact square size."""
    return src.resize((size, size), Image.LANCZOS)


def build_ico(src: Image.Image, out: Path, sizes: list[int] | None = None):
    """Multi-frame ICO: base is the largest frame, the rest ride along in
    append_images at their exact pixel sizes (Pillow re-uses the matching
    sub-image instead of re-encoding from the base — no downscale loss)."""
    sizes = sizes or ICO_SIZES
    frames = [resize_to(src, s) for s in sizes]
    frames[-1].save(
        out,
        format="ICO",
        sizes=[(s, s) for s in sizes],
        append_images=frames[:-1],
    )


def build_icns(src: Image.Image, out: Path):
    """macOS .icns: Pillow scales the 1024 base down to each required size."""
    resize_to(src, 1024).save(
        out,
        format="ICNS",
        sizes=[(s, s) for s in ICNS_SIZES],
    )


def ios_icon_px(fname: str) -> int | None:
    """Parse 'AppIcon-20x20@2x-1.png' → 40 or 'AppIcon-512@2x.png' → 1024.
    Returns None for non-appicon files."""
    m = re.match(r"^AppIcon-(\d+(?:\.\d+)?)(?:x\d+(?:\.\d+)?)?@(\d+)x", fname)
    if not m:
        return None
    base = float(m.group(1))
    return round(base * int(m.group(2)))


def build_android_foreground(src: Image.Image, canvas: int, out: Path):
    """Adaptive-icon foreground: the full-bleed art shrunk into the 66/108 dp
    safe zone on a transparent canvas, centered. The launcher's mask crops the
    rest; the background color stays #fff (ic_launcher_background.xml)."""
    content = round(canvas * ANDROID_SAFE_FRACTION)
    canvas_img = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    art = resize_to(src, content)
    offset = (canvas - content) // 2
    canvas_img.paste(art, (offset, offset), art)
    canvas_img.save(out)


def build_favicon_svg(src: Image.Image, out: Path):
    """Self-contained SVG favicon: the rounded art embedded as a data-URI PNG
    so the file keeps working wherever an SVG favicon is referenced."""
    buf = io.BytesIO()
    resize_to(src, 128).save(buf, format="PNG")
    data_uri = "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()
    out.write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" '
        f'viewBox="0 0 128 128"><image href="{data_uri}" width="128" height="128"/>'
        "</svg>\n",
        encoding="utf-8",
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

    # README badge — the pre-rounded art (Windows source) reads better on the
    # light GitHub docs than the full-bleed square Linux icon.
    print("README rounded icon (from windows source)…")
    resize_to(win, 512).save(ICONS / "icon-rounded.png")
    print("  icon-rounded.png (512x512)")

    print("building .ico (Windows, from windows source)…")
    build_ico(win, ICONS / "icon.ico")
    print(f"  icon.ico {ICO_SIZES} ({len(ICO_SIZES)} frames embedded)")

    print("building .icns (macOS, from unix source)…")
    build_icns(unix, ICONS / "icon.icns")
    print(f"  icon.icns {ICNS_SIZES}")

    print("iOS AppIcon set (from unix source)…")
    for f in sorted(IOS_DIR.glob("AppIcon-*.png")):
        px = ios_icon_px(f.name)
        if px is None:
            print(f"  SKIP {f.name} (unrecognised)")
            continue
        resize_to(unix, px).save(f)
        print(f"  {f.name} ({px}x{px})")

    print("Android mipmaps (from unix source)…")
    for density, px in ANDROID_LAUNCHER.items():
        mipmap = ICONS / "android" / f"mipmap-{density}"
        for fname in ("ic_launcher.png", "ic_launcher_round.png"):
            resize_to(unix, px).save(mipmap / fname)
        build_android_foreground(unix, ANDROID_FOREGROUND[density], mipmap / "ic_launcher_foreground.png")
        print(f"  {density} launcher {px}px + adaptive foreground {ANDROID_FOREGROUND[density]}px")

    print("website favicon (from windows source)…")
    build_ico(win, WEB / "icon.ico", sizes=FAVICON_ICO_SIZES)
    build_favicon_svg(win, WEB / "favicon.svg")
    print(f"  icon.ico {FAVICON_ICO_SIZES} + favicon.svg (embedded 128px)")

    print("done.")


if __name__ == "__main__":
    main()
