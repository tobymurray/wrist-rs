#!/usr/bin/env python3
"""Draws the launcher icons a utility app needs: an arrow into a tray. Run from this folder."""
from PIL import Image, ImageDraw

TEAL = (0, 192, 192, 255)
SCALE = 8


def icon(size):
    s = size * SCALE
    img = Image.new('RGBA', (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    w = s // 10
    # Tray: a U along the bottom.
    d.line([(s * 0.18, s * 0.58), (s * 0.18, s * 0.82), (s * 0.82, s * 0.82), (s * 0.82, s * 0.58)],
           fill=TEAL, width=w, joint='curve')
    # Arrow: a shaft and a head pointing into the tray.
    d.line([(s * 0.5, s * 0.14), (s * 0.5, s * 0.6)], fill=TEAL, width=w)
    d.polygon([(s * 0.3, s * 0.46), (s * 0.7, s * 0.46), (s * 0.5, s * 0.68)], fill=TEAL)
    return img.resize((size, size), Image.LANCZOS)


for n in (60, 30):
    icon(n).save(f'icon_{n}x{n}.png', optimize=True)
