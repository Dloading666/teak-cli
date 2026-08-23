#!/usr/bin/env python3
"""Prepare and render the approved Teak CLI app icon artwork.

The canonical source is ``brand/teak-icon-master.png``: a 1024px RGBA image
with genuine transparency outside the icon plate. Normal use refreshes the
brand copies and the two platform source files consumed by
``scripts/rebuild-icons.py``::

    python3 brand/render_teak_icon.py
    python3 scripts/rebuild-icons.py

Image-generation previews sometimes contain a *painted* checkerboard instead
of alpha. Import one explicitly with::

    python3 brand/render_teak_icon.py --import-preview /path/to/approved.png

Importing removes only that near-white exterior and keeps the approved icon's
interior artwork. The cleaned result becomes the reproducible master.
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ICONS = REPO / "icons"

SIZE = 1024
MASTER = HERE / "teak-icon-master.png"

# The approved plate's median edge colour. Checkerboard-contaminated edge
# pixels are de-matted to this colour before alpha is applied, preventing a
# pale fringe after Lanczos downscaling into the 16–512px bundle frames.
EDGE_GREEN = np.array([43, 82, 70], dtype=np.uint8)  # #2B5246


def _dark_subject_mask(rgb: Image.Image) -> Image.Image:
    """Return the approved icon silhouette from a near-white preview.

    The icon (including its brightest wood highlights) stays below luma 225;
    the checkerboard sits around 240–255. A tiny close removes compression
    pinholes, then a one-pixel inset plus feather discards colour-contaminated
    boundary pixels while retaining a clean antialiased silhouette.
    """
    arr = np.asarray(rgb, dtype=np.float32)
    luma = 0.299 * arr[..., 0] + 0.587 * arr[..., 1] + 0.114 * arr[..., 2]
    subject = Image.fromarray(np.where(luma < 225.0, 255, 0).astype(np.uint8))
    subject = subject.filter(ImageFilter.MaxFilter(3)).filter(ImageFilter.MinFilter(3))

    bbox = subject.getbbox()
    if bbox is None:
        raise ValueError("approved preview does not contain a detectable icon")
    width = bbox[2] - bbox[0]
    height = bbox[3] - bbox[1]
    if width < rgb.width * 0.6 or height < rgb.height * 0.6:
        raise ValueError(f"detected subject is unexpectedly small: bbox={bbox}")

    inset = subject.filter(ImageFilter.MinFilter(3))
    return inset.filter(ImageFilter.GaussianBlur(0.7))


def import_preview(path: Path) -> None:
    """Convert an approved square preview into the transparent 1024px master."""
    preview = Image.open(path)
    if preview.width != preview.height:
        raise ValueError(f"approved preview must be square, got {preview.size}")

    rgba = preview.convert("RGBA")
    alpha = rgba.getchannel("A")
    has_real_transparency = alpha.getextrema()[0] < 250

    if not has_real_transparency:
        rgb = preview.convert("RGB")
        clean_alpha = _dark_subject_mask(rgb)
        source = np.asarray(rgb, dtype=np.uint8).copy()

        # All pixels outside the solid subject, including the feather band,
        # receive plate-coloured RGB. Their alpha controls visibility; this
        # avoids white/checkerboard RGB bleeding into small icon frames.
        solid = np.asarray(clean_alpha, dtype=np.uint8) >= 250
        source[~solid] = EDGE_GREEN
        rgba = Image.fromarray(source).convert("RGBA")
        rgba.putalpha(clean_alpha)

    rgba = rgba.resize((SIZE, SIZE), Image.Resampling.LANCZOS)
    if rgba.getpixel((0, 0))[3] != 0:
        raise ValueError("imported master does not have a transparent corner")
    rgba.save(MASTER)
    print(f"imported approved master: {MASTER}")


def render_sources() -> None:
    if not MASTER.exists():
        raise SystemExit(
            f"missing {MASTER}; import the approved artwork with --import-preview first"
        )

    master = Image.open(MASTER).convert("RGBA")
    if master.size != (SIZE, SIZE):
        raise ValueError(f"master must be {SIZE}x{SIZE}, got {master.size}")
    if master.getpixel((0, 0))[3] != 0:
        raise ValueError("master must have transparent outer corners")

    outputs = {
        HERE / "teak-icon-unix.png": master,
        HERE / "teak-icon-windows.png": master,
        HERE / "teak-icon-512.png": master.resize((512, 512), Image.Resampling.LANCZOS),
        HERE / "teak-icon-32.png": master.resize((32, 32), Image.Resampling.LANCZOS),
        ICONS / "icon-source-unix.png": master,
        ICONS / "icon-source-windows.png": master,
    }
    for path, image in outputs.items():
        image.save(path)
        print(f"wrote {path.relative_to(REPO)} ({image.width}x{image.height})")

    print("next: python3 scripts/rebuild-icons.py")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--import-preview",
        type=Path,
        metavar="PNG",
        help="clean a selected square preview into the canonical RGBA master",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.import_preview is not None:
        import_preview(args.import_preview.expanduser().resolve())
    render_sources()


if __name__ == "__main__":
    main()
