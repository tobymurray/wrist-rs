#!/usr/bin/env python3
"""Build the image in a clone of this checkout's HEAD, several ways, and check that each build
that differs has its own ID, and that the image hashes what app.bin holds; needs the image's
toolchain, an objcopy, pyelftools and a few minutes, and no SDK or watch.
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

from elftools.elf.elffile import ELFFile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'tools'))
import firmware_build  # noqa: E402

ELF = 'target/thumbv8m.main-none-eabihf/release/standalone-boot'


def build(app, *features, env=None):
    """The image's ELF with `features`, and the environment build.rs gave it."""
    args = ['--features', ','.join(features)] if features else []
    out = subprocess.run(['cargo', 'build', '--locked', '--release', '--message-format=json', *args],
                         cwd=app, env=env, capture_output=True, text=True)
    if out.returncode:
        raise subprocess.CalledProcessError(out.returncode, args, out.stdout, out.stderr)
    found = {}
    for line in out.stdout.splitlines():
        m = json.loads(line)
        if m.get('reason') == 'build-script-executed' and m['package_id'].find('standalone-boot') >= 0:
            found = dict(m['env'])
    return (app/ELF).read_bytes(), found


def variant(features, pinned, recipe=False):
    """The `+...` build.rs gives `features` against the recipe's `pinned`."""
    shown = [f for f in features if f not in pinned] + [f'no-{f}' for f in pinned if f not in features]
    shown += [] if recipe else ['local']
    return '+' + '.'.join(shown) if shown else ''


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
        cls.pinned = firmware_build.app_features(cls.root)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_each_feature_set_has_its_own_id(self):
        source = firmware_build.build_id(self.root)
        version = re.search(r'(?m)^version = "(.*)"', (self.app/'Cargo.toml').read_text())[1]
        seen = {}
        for features in [[], ['ble'], ['usb-log']]:
            elf, env = build(self.app, *features, env=self.env)
            self.assertEqual(env['WRIST_BUILD_VARIANT'], variant(features, self.pinned))
            self.assertEqual(env['WRIST_SOURCE_ID'], source)
            self.assertRegex(env['WRIST_BUILD_ID'], r'^[0-9a-f]{12}$')
            self.assertIn(env['WRIST_BUILD_ID'].encode(), elf)
            self.assertIn((version + variant(features, self.pinned)).encode(), elf)
            seen[env['WRIST_BUILD_ID']] = elf
        self.assertEqual(len(seen), 3)
        self.assertNotIn(source, seen)
        # The same build again: the same ID and bytes.
        again, env = build(self.app, 'ble', env=self.env)
        self.assertEqual(seen.get(env['WRIST_BUILD_ID']), again)

    def test_the_release_id_is_for_the_recipe_s_features_only(self):
        env = dict(self.env, WRIST_RELEASE_BUILD_ID='gate-test')
        _, found = build(self.app, *self.pinned, env=env)
        self.assertEqual((found['WRIST_BUILD_ID'], found['WRIST_BUILD_VARIANT']), ('gate-test', ''))
        with self.assertRaises(subprocess.CalledProcessError) as e:
            build(self.app, *(['usb-log'] if 'usb-log' not in self.pinned else []), *self.pinned, env=env)
        self.assertIn('WRIST_RELEASE_BUILD_ID names the release recipe', e.exception.stderr)

    def test_a_pinned_feature_is_the_recipe_s(self):
        """With `ble` pinned, as a release that ships it would, the `ble` build is the recipe's."""
        toolchain = self.root/'release/toolchain.json'
        text = toolchain.read_text()
        try:
            toolchain.write_text(re.sub(r'"app_features": \[[^]]*\]', '"app_features": ["ble"]', text))
            self.assertEqual(firmware_build.app_features(self.root), ['ble'])
            self.assertEqual(firmware_build.feature_args('standalone-boot', self.root), ['--features', 'ble'])
            _, found = build(self.app, 'ble', env=self.env)
            self.assertEqual(found['WRIST_BUILD_VARIANT'], '+local')
            _, found = build(self.app, env=self.env)
            self.assertEqual(found['WRIST_BUILD_VARIANT'], '+no-ble.local')
            env = dict(self.env, WRIST_RELEASE_BUILD_ID='gate-test')
            _, found = build(self.app, 'ble', env=env)
            self.assertEqual((found['WRIST_BUILD_ID'], found['WRIST_BUILD_VARIANT']), ('gate-test', ''))
            with self.assertRaises(subprocess.CalledProcessError) as e:
                build(self.app, env=env)
            self.assertIn('(+no-ble)', e.exception.stderr)
        finally:
            toolchain.write_text(text)

    def test_the_image_hashes_what_app_bin_holds(self):
        """version.rs's `image()` ends where objcopy ends app.bin, so its CRC is wrist-update.py's."""
        build(self.app, env=self.env)
        objcopy = next(filter(None, map(shutil.which, ['arm-none-eabi-objcopy', 'llvm-objcopy', 'rust-objcopy'])))
        app_bin = Path(self.tmp.name)/'app.bin'
        subprocess.run([objcopy, '-O', 'binary', self.app/ELF, app_bin], check=True)
        with open(self.app/ELF, 'rb') as f:
            symbols = {s.name: s['st_value'] for s in ELFFile(f).get_section_by_name('.symtab').iter_symbols()}
        end = symbols['__sidata'] + symbols['__edata'] - symbols['__sdata']
        self.assertEqual(app_bin.stat().st_size, end - symbols['__vector_table'])

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
