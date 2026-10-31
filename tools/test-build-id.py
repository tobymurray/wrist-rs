#!/usr/bin/env python3
"""Build the image in a clone of this checkout's HEAD, several ways, and check that each build
that differs has its own ID; needs the image's toolchain and a few minutes, and no SDK or watch.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'tools'))
import firmware_build  # noqa: E402


def build(app, *args, env=None):
    """The image's ELF, and the environment build.rs gave it."""
    out = subprocess.run(['cargo', 'build', '--locked', '--release', '--message-format=json', *args],
                         cwd=app, env=env, capture_output=True, text=True)
    if out.returncode:
        raise subprocess.CalledProcessError(out.returncode, args, out.stdout, out.stderr)
    found = {}
    for line in out.stdout.splitlines():
        m = json.loads(line)
        if m.get('reason') == 'build-script-executed' and m['package_id'].find('standalone-boot') >= 0:
            found = dict(m['env'])
    elf = app/'target/thumbv8m.main-none-eabihf/release/standalone-boot'
    return elf.read_bytes(), found


class BuildId(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.tmp.name)/'wrist-rs'
        subprocess.run(['git', 'clone', '-q', '--no-hardlinks', ROOT, cls.root], check=True)
        subprocess.run(['git', 'checkout', '-q', subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()],
                       cwd=cls.root, check=True)
        cls.app = cls.root/'apps/standalone-boot'
        cls.env = {k: v for k, v in os.environ.items() if k not in ['WRIST_RELEASE_BUILD_ID', 'RUSTFLAGS', 'CARGO_TARGET_DIR']}

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_each_feature_set_has_its_own_id(self):
        source = firmware_build.build_id(self.root)
        version = re.search(r'(?m)^version = "(.*)"', (self.app/'Cargo.toml').read_text())[1]
        seen = {}
        for features in [[], ['ble'], ['usb-log'], ['ble', 'usb-log']]:
            elf, env = build(self.app, *(['--features', ','.join(features)] if features else []), env=self.env)
            variant = '+' + '.'.join(features + ['local'])
            self.assertEqual(env['WRIST_BUILD_VARIANT'], variant)
            self.assertEqual(env['WRIST_SOURCE_ID'], source)
            self.assertRegex(env['WRIST_BUILD_ID'], r'^[0-9a-f]{12}$')
            self.assertIn(env['WRIST_BUILD_ID'].encode(), elf)
            self.assertIn((version + variant).encode(), elf)
            seen[env['WRIST_BUILD_ID']] = elf
        self.assertEqual(len(seen), 4)
        self.assertNotIn(source, seen)
        # The same build again: the same ID and bytes.
        again, env = build(self.app, '--features', 'ble', env=self.env)
        self.assertEqual(seen.get(env['WRIST_BUILD_ID']), again)

    def test_the_release_id_is_for_the_default_build_only(self):
        env = dict(self.env, WRIST_RELEASE_BUILD_ID='gate-test')
        _, found = build(self.app, env=env)
        self.assertEqual((found['WRIST_BUILD_ID'], found['WRIST_BUILD_VARIANT']), ('gate-test', ''))
        with self.assertRaises(subprocess.CalledProcessError) as e:
            build(self.app, '--features', 'ble', env=env)
        self.assertIn('WRIST_RELEASE_BUILD_ID names the release recipe', e.exception.stderr)

    def test_an_edit_is_dirty_and_hashed(self):
        console = self.app/'src/console.rs'
        text = console.read_text()
        ids = set()
        try:
            for reply in ['pong A', 'pong B']:
                console.write_text(re.sub(r'"\[usb\] pong"', f'"[usb] {reply}"', text))
                elf, env = build(self.app, env=self.env)
                self.assertEqual(env['WRIST_SOURCE_ID'], firmware_build.build_id(self.root))
                self.assertTrue(env['WRIST_BUILD_ID'].endswith('-dirty'))
                self.assertIn(f'[usb] {reply}'.encode(), elf)
                ids.add(env['WRIST_BUILD_ID'])
        finally:
            console.write_text(text)
        self.assertEqual(len(ids), 2)


if __name__ == '__main__':
    unittest.main()
