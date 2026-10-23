#!/usr/bin/env python3
"""Resolve a retained panic's file_fnv1a using the failing build's ELF or raw image.

    wrist-crash.py app.bin 25086F54 --line 125
"""
import argparse
import re
from pathlib import Path


def fnv1a(data):
    value = 0x811C9DC5
    for byte in data:
        value = ((value ^ byte) * 0x01000193) & 0xFFFFFFFF
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("file_fnv1a", type=lambda text: int(text, 16))
    parser.add_argument("--line", type=int)
    args = parser.parse_args()
    matches = set()
    for path in re.findall(rb"[A-Za-z0-9_./-]+\.rs", args.image.read_bytes()):
        candidates = {path}
        for prefix in (b"/home/", b"/rustc/", b"src/", b"library/"):
            start = path.find(prefix)
            if start >= 0:
                candidates.add(path[start:])
        matches.update(value.decode() for value in candidates if fnv1a(value) == args.file_fnv1a)
    for value in sorted(matches):
        print(value + (f":{args.line}" if args.line is not None else ""))
    if not matches:
        parser.exit(1, "no match; use the image from the failing build\n")


if __name__ == "__main__":
    main()
