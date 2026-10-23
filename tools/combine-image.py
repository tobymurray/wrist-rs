#!/usr/bin/env python3
"""The installer's image: the loader padded to its region, then the application (apps/wrist-loader/LAYOUT.md)."""
import sys, zlib

LOADER_SIZE = 32 * 1024
# MAX_PAGES in crates/wrist-flash/src/plan.rs.
INSTALLER_MAX = 68 * 8192

def main(loader_path, app_path, out_path):
    loader = open(loader_path, 'rb').read()
    app = open(app_path, 'rb').read()
    if not loader or not app:
        sys.exit("loader and application must both be nonempty")
    if len(loader) > LOADER_SIZE:
        sys.exit(f"loader is {len(loader)} bytes, over its {LOADER_SIZE}-byte region")
    image = loader + b'\xff' * (LOADER_SIZE - len(loader)) + app
    if len(image) > INSTALLER_MAX:
        sys.exit(f"combined image is {len(image)} bytes, over the installer's {INSTALLER_MAX}")
    open(out_path, 'wb').write(image)
    print(f"loader {len(loader)} B, app {len(app)} B, image {len(image)} B, CRC-32 {zlib.crc32(image) & 0xffffffff:08X}")

if __name__ == '__main__':
    if len(sys.argv) != 4:
        sys.exit("usage: combine-image.py loader.bin app.bin image.bin")
    main(*sys.argv[1:])
