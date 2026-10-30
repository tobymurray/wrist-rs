#!/usr/bin/env python3
"""Resolve the [crash] line that `power` prints, against the build that wrote it.

    wrist-crash.py wrist-rs-<version>.zip "[crash] panic line=126 file_fnv1a=0xA49AF3EA build=..."

The build is a release or debug zip, its manifest.json with app.bin or app.elf beside it, or
an app.bin or app.elf of your own. A record from another build is refused.
"""
import argparse
import json
import re
import sys
import zipfile
from pathlib import Path


def fnv1a(data):
    value = 0x811C9DC5
    for byte in data:
        value = ((value ^ byte) * 0x01000193) & 0xFFFFFFFF
    return value


def as_recorded(build_id):
    """A build ID as `Build::text` in crates/wrist-crash prints it."""
    if re.fullmatch(r'[0-9a-fA-F]{12}(-dirty)?', build_id):
        return build_id.lower()
    return f'#{fnv1a(build_id.encode()):08x}'


def load(source):
    """The image's bytes, and the manifest's build ID or None."""
    if source.suffix == '.zip':
        with zipfile.ZipFile(source) as z:
            names = z.namelist()
            image = next((n for n in ['app.elf', 'app.bin'] if n in names), None)
            if image is None or 'manifest.json' not in names:
                sys.exit(f'{source}: no manifest.json and app.bin or app.elf')
            return z.read(image), json.loads(z.read('manifest.json'))['build_id']
    if source.name == 'manifest.json':
        image = next((p for p in [source.with_name('app.elf'), source.with_name('app.bin')] if p.exists()), None)
        if image is None:
            sys.exit(f'no app.bin or app.elf beside {source}')
        return image.read_bytes(), json.loads(source.read_text())['build_id']
    return source.read_bytes(), None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('source', type=Path, help='release or debug zip, manifest.json, app.bin or app.elf')
    parser.add_argument('record', help='the [crash] line')
    parser.add_argument('--build', help='the build ID of an app.bin or app.elf that is not a firmware hash')
    parser.add_argument('--unchecked', action='store_true', help="resolve an older image's record, which names no build")
    args = parser.parse_args()

    fields = dict(re.findall(r'(\w+)=(\S+)', args.record))
    image, build_id = load(args.source)
    recorded = fields.get('build')
    if recorded is None:
        if not args.unchecked:
            parser.exit(1, "an older image's record names no build; --unchecked resolves it against this one anyway\n")
    else:
        if build_id is None and args.build is not None:
            if args.build.encode() not in image:
                parser.exit(1, f'build {args.build} is not in {args.source}\n')
            build_id = args.build
        if build_id is None:
            if recorded.startswith('#'):
                parser.exit(1, f'build {recorded} is a named build; --build gives its name\n')
            if not re.search(re.escape(recorded.encode()) + rb'(?!-dirty)', image):
                parser.exit(1, f'build {recorded} is not in {args.source}; use the image from the failing build\n')
        elif as_recorded(build_id) != recorded:
            parser.exit(1, f'the record is from build {recorded}, {args.source} is {build_id}; use the failing build\n')

    if 'pc' in fields:
        print(f"fault at pc={fields['pc']}: addr2line -e app.elf {fields['pc']} names it")
        return
    if 'file_fnv1a' not in fields:
        parser.exit(2, 'no file_fnv1a or pc in the record\n')
    wanted = int(fields['file_fnv1a'], 16)
    line = fields.get('line')
    matches = set()
    for path in re.findall(rb'[A-Za-z0-9_./-]+\.rs', image):
        candidates = {path}
        for prefix in (b'/home/', b'/rustc/', b'src/', b'library/'):
            start = path.find(prefix)
            if start >= 0:
                candidates.add(path[start:])
        matches.update(value.decode() for value in candidates if fnv1a(value) == wanted)
    for value in sorted(matches):
        print(value + (f':{line}' if line else ''))
    if not matches:
        parser.exit(1, 'no source path in the image has that hash\n')


if __name__ == '__main__':
    main()
