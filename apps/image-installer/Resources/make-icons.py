#!/usr/bin/env python3
"""Renders the launcher icons a utility app needs from the project's mark, release/images/icon.svg,
with rsvg-convert. Run from this folder."""
from pathlib import Path
import subprocess

MARK = Path(__file__).resolve().parents[3] / 'release/images/icon.svg'

for n in (60, 30):
    subprocess.run(['rsvg-convert', '-w', str(n), '-h', str(n), '-o', f'icon_{n}x{n}.png', str(MARK)], check=True)
