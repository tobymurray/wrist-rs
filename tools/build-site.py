#!/usr/bin/env python3
"""Build the download page from site/ and GitHub's `releases/latest` answer, {} when there is none.

    tools/build-site.py latest.json _site
"""
import html
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPO = 'https://github.com/tobymurray/wrist-rs'


def fill(release):
    """The page's {{href}}, {{label}} and {{meta}}, from a release or none."""
    zips = [a for a in release.get('assets', [])
            if a['name'].endswith('.zip') and not a['name'].endswith('-debug.zip')]
    if not zips:
        return {'href': REPO + '/releases', 'label': 'Releases',
                'meta': 'No release yet. Each one is listed on GitHub with its checksums.'}
    asset = zips[0]
    meta = [html.escape(asset['name']), f"{asset['size'] / 1e6:.1f} MB"]
    digest = asset.get('digest') or ''
    if digest.startswith('sha256:') and all(c in '0123456789abcdef' for c in digest[7:]):
        hexdigest = digest[7:]
        meta.append(f'SHA-256 <code title="{hexdigest}">{hexdigest[:8]}…{hexdigest[-8:]}</code>')
    meta.append(f'<a href="{REPO}/attestations">provenance</a>')
    return {'href': html.escape(asset['browser_download_url']),
            'label': 'Download ' + html.escape(release['tag_name']),
            'meta': ' · '.join(meta)}


def main():
    latest, out = Path(sys.argv[1]), Path(sys.argv[2])
    release = json.loads(latest.read_text() or '{}')
    page = (ROOT / 'site/index.html').read_text()
    for key, value in fill(release).items():
        page = page.replace('{{' + key + '}}', value)
    if '{{' in page:
        sys.exit('unfilled placeholder in site/index.html')
    out.mkdir(parents=True)
    (out / 'index.html').write_text(page)
    shutil.copy2(ROOT / 'site/face.png', out / 'face.png')
    shutil.copy2(ROOT / 'release/images/icon.svg', out / 'favicon.svg')
    for name in ['favicon.png', 'apple-touch-icon.png']:
        shutil.copy2(ROOT / 'site' / name, out / name)


if __name__ == '__main__':
    main()
