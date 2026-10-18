#!/usr/bin/env python3
"""Rounds a screenshot's corners, as the desktop draws the window's, and
gives it a soft shadow, so a light window still has an edge on a light
page.

    scripts/round.py IN.png OUT.png            a whole window
    scripts/round.py --dialog IN.png OUT.png   just the dialog on it

shot.sh captures windows square (nested sway draws no corners). With
--dialog, the dialog is cropped out of a window dimmed behind it, found as
the bright surface around the middle, and rounded to the dialog's corners.
Needs Pillow.
"""

import sys

from PIL import Image, ImageDraw, ImageFilter

WINDOW_RADIUS = 12
DIALOG_RADIUS = 18
# Drawn this many times larger and scaled down, so the corners are smooth.
SUPERSAMPLE = 4
# The shadow: how far it spreads, how far down it falls, and how dark.
SHADOW_BLUR = 14
SHADOW_DROP = 4
SHADOW_ALPHA = 0.28


def rounded(im: Image.Image, radius: int) -> Image.Image:
    im = im.convert("RGBA")
    w, h = im.size
    mask = Image.new("L", (w * SUPERSAMPLE, h * SUPERSAMPLE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, w * SUPERSAMPLE - 1, h * SUPERSAMPLE - 1),
        radius * SUPERSAMPLE,
        fill=255,
    )
    im.putalpha(mask.resize((w, h), Image.LANCZOS))
    return im


def shadowed(im: Image.Image) -> Image.Image:
    pad = SHADOW_BLUR * 2
    w, h = im.size
    out = Image.new("RGBA", (w + 2 * pad, h + 2 * pad), (0, 0, 0, 0))
    shadow = Image.new("RGBA", out.size, (0, 0, 0, 0))
    alpha = im.getchannel("A").point(lambda a: int(a * SHADOW_ALPHA))
    black = Image.new("RGBA", im.size, (0, 0, 0, 255))
    black.putalpha(alpha)
    shadow.alpha_composite(black, (pad, pad + SHADOW_DROP))
    out.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(SHADOW_BLUR / 2)))
    out.alpha_composite(im, (pad, pad))
    return out


def dialog_box(im: Image.Image) -> tuple[int, int, int, int]:
    """The dialog's surface: the bright run through the middle of the
    window, which is dimmed everywhere else."""
    rgb = im.convert("RGB")
    w, h = rgb.size

    def bright(p) -> bool:
        return sum(p) / 3 >= 240

    def run(points):
        hits = [i for i, p in enumerate(points) if bright(p)]
        return hits[0], hits[-1]

    left, right = run([rgb.getpixel((x, h // 2)) for x in range(w)])
    # A column just inside the edge, clear of the dialog's content.
    top, bottom = run([rgb.getpixel((left + 8, y)) for y in range(h)])
    return left, top, right + 1, bottom + 1


def main() -> None:
    args = sys.argv[1:]
    dialog = args[:1] == ["--dialog"]
    if dialog:
        args = args[1:]
    if len(args) != 2:
        sys.exit(__doc__)
    src, dst = args
    im = Image.open(src)
    if dialog:
        im = rounded(im.crop(dialog_box(im)), DIALOG_RADIUS)
    else:
        im = rounded(im, WINDOW_RADIUS)
    shadowed(im).save(dst, optimize=True)


if __name__ == "__main__":
    main()
