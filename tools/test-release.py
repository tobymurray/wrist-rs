#!/usr/bin/env python3
"""Package safety checks without firmware compilation or hardware."""
import importlib.util
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import textwrap
import unittest
import zlib
import zipfile
import warnings

spec = importlib.util.spec_from_file_location('release_build', Path(__file__).with_name('release-build.py'))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


def raw(addr, size=64):
    return struct.pack('<II', 0x2026FFE0, addr + 9) + bytes(size - 8)


def uapp(version=0x010203):
    b = struct.pack('<QIIII16sII', 0x854CD3EFBD7BA411, version, 3, 8, 0x22,
                    b'Install', 3600, 900) + bytes(4508)
    return b + struct.pack('<I', zlib.crc32(b))


class Packaging(unittest.TestCase):
    def test_archive_metadata_path_normalization(self):
        replacements = r.path_replacements(
            ('/build/checkout', '/src'), ('/build/stage/candidate', '/out'),
            ('/build/cargo-home', '$CARGO_HOME'))
        metadata = {
            'command': ['/build/checkout/tools/build.py', '--out', '/build/stage/candidate'],
            'dependency': {'manifest_path': '/build/cargo-home/registry/pkg/Cargo.toml'},
            'map': ' object /build/checkout/target/app.o output /build/stage/candidate/app.map',
        }
        normalized = r.normalize_paths(metadata, replacements)
        self.assertEqual(normalized['command'], ['/src/tools/build.py', '--out', '/out'])
        self.assertEqual(normalized['dependency']['manifest_path'], '$CARGO_HOME/registry/pkg/Cargo.toml')
        self.assertEqual(normalized['map'], ' object /src/target/app.o output /out/app.map')

    def test_sizes_vectors_and_padding(self):
        l, a = raw(0x08060000), raw(0x08068000)
        image = l + b'\xff' * (32768-len(l)) + a
        self.assertEqual(r.validate_image(l, a, image), f'{zlib.crc32(image):08X}')
        for loader, app, combined in [(b'', a, image), (l, b'', image),
                (l, a, image[:-1]), (l, a, image[:64]+bytes(32704)+a),
                (l, raw(0x08060000), image), (bytes(32769), a, image),
                (l, raw(0x08068000, 524289), image), (l, bytes(64), image)]:
            with self.assertRaises(ValueError):
                r.validate_image(loader, app, combined)

    def test_uapp_identity_crc_header_and_abi(self):
        self.assertEqual(r.validate_uapp(uapp(), '1.2.3')['libc_abi'], '0.0.3')
        for bad in [b'', uapp()[:-1], uapp()[:-4]+bytes(4), uapp(0x010204)]:
            with self.assertRaises(ValueError):
                r.validate_uapp(bad, '1.2.3')
        for offset in [0, 12, 16, 20, 40, 44]:
            b = bytearray(uapp()[:-4]); b[offset] ^= 1
            with self.assertRaises(ValueError):
                r.validate_uapp(bytes(b)+struct.pack('<I', zlib.crc32(b)), '1.2.3')

    def test_combine_refuses_missing_empty_and_oversize(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for lsize, asize in [(0, 8), (8, 0), (32769, 8), (8, 524289)]:
                (d/'l').write_bytes(bytes(lsize)); (d/'a').write_bytes(bytes(asize))
                result = subprocess.run([sys.executable, str(r.ROOT/'tools/combine-image.py'),
                                         str(d/'l'), str(d/'a'), str(d/'out')], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((d/'out').exists())
            (d/'a').unlink()
            self.assertNotEqual(subprocess.run([sys.executable, str(r.ROOT/'tools/combine-image.py'),
                str(d/'l'), str(d/'a'), str(d/'out')], capture_output=True).returncode, 0)

    def test_archive_rejects_arm_paths_and_missing_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            for names in [['ARM_INSTALL'], ['../escape'], ['app.bin'], ['vendor.ota'], ['manifest.json', 'manifest.json']]:
                path = Path(tmp)/'bad.zip'
                with warnings.catch_warnings(), zipfile.ZipFile(path, 'w') as z:
                    warnings.simplefilter('ignore', UserWarning)
                    for name in names:
                        z.writestr(name, b'{}')
                with self.assertRaises((ValueError, KeyError)):
                    r.verify_archive(path)

    def test_licenses_cover_what_is_linked(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for name in r.SDK_LICENSES + ['ThirdParty/touchgfx/LICENSE.txt',
                                          'ThirdParty/touchgfx/framework/include/touchgfx/canvas_widget_renderer/license.txt']:
                (d/'sdk'/name).parent.mkdir(parents=True, exist_ok=True); (d/'sdk'/name).write_text(name)
            (d/'rust').mkdir()
            for name in ['GPL-3.0-or-later.txt', 'GCC-exception-3.1.txt']:
                (d/'rust'/name).write_text(name)
            gcc = d/'arm/bin/arm-none-eabi-gcc'
            (d/'arm/share/doc/gcc-arm-none-eabi').mkdir(parents=True); gcc.parent.mkdir()
            gcc.write_text(''); (d/'arm/share/doc/gcc-arm-none-eabi/license.txt').write_text('gcc')
            r.collect_licenses(d/'out', d/'sdk', [d/'missing', d/'rust'], str(gcc))
            self.assertEqual(sorted(p.relative_to(d/'out').as_posix() for p in (d/'out').rglob('*') if p.is_file()), [
                'gnu-arm-toolchain/license.txt', 'sdk/LICENSE', 'sdk/THIRD-PARTY-LICENSES.md',
                'sdk/ThirdParty/coreJSON/LICENSE', 'sdk/libstdc++/GCC-exception-3.1.txt',
                'sdk/libstdc++/GPL-3.0-or-later.txt', 'wrist-glass-font/LICENSE'])
            for args in [(d/'sdk', [d/'missing'], str(gcc)), (d/'sdk', [d/'rust'], None),
                         (d/'sdk', [d/'rust'], str(d/'rust/MIT.txt')), (d/'rust', [d/'rust'], str(gcc))]:
                with self.assertRaises(ValueError):
                    r.collect_licenses(d/'again', *args)

    def test_tag_matches_app_versions(self):
        r.require_app_versions(r.app_version((r.ROOT/'apps/standalone-boot/Cargo.toml').read_text()))
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for app, version in [('standalone-boot', '1.2.3'), ('wrist-loader', '1.2.3')]:
                (d/'apps'/app).mkdir(parents=True)
                (d/'apps'/app/'Cargo.toml').write_text(
                    f'[package]\nname = "{app}"\nversion = "{version}"\n\n[dependencies]\nx = {{ version = "9.9.9" }}\n')
            r.require_app_versions('1.2.3', d)
            r.require_app_versions('1.2.3-rc.1', d)
            for bad in ['1.2.4', '0.1.0']:
                with self.assertRaises(ValueError):
                    r.require_app_versions(bad, d)
            (d/'apps/wrist-loader/Cargo.toml').write_text('[package]\nname = "wrist-loader"\nversion = "1.2.2"\n')
            with self.assertRaises(ValueError):
                r.require_app_versions('1.2.3', d)
            (d/'apps/wrist-loader/Cargo.toml').write_text('[package]\nname = "wrist-loader"\n\n[dependencies]\nx = { version = "1.2.3" }\n')
            with self.assertRaises(ValueError):
                r.require_app_versions('1.2.3', d)

    def test_packaged_doc_links(self):
        text = ('[notes](NOTES.md) [guide](INSTALL.md#the-tools) [t](../tools/wrist-time.py) '
                '![x](images/logo.svg) [l](../LICENSE) [h](../HARDWARE.md#stop-modes) '
                '[s](../tools/extract_pinmap.py) [w](https://example.org/a.md) [a](#updating)')
        self.assertEqual(r.packaged_images(['![a](images/a.svg) <img src="images/b.svg" width="9">', 'x']),
                         ['images/a.svg', 'images/b.svg'])
        self.assertEqual(r.package_links(text, 'abc'),
                         '[notes](RELEASE-NOTES.md) [guide](START-HERE.md#the-tools) [t](tools/wrist-time.py) '
                         '![x](images/logo.svg) [l](LICENSE) '
                         '[h](https://github.com/tobymurray/wrist-rs/blob/abc/HARDWARE.md#stop-modes) '
                         '[s](https://github.com/tobymurray/wrist-rs/blob/abc/tools/extract_pinmap.py) '
                         '[w](https://example.org/a.md) [a](#updating)')
        root = Path(__file__).resolve().parent.parent
        texts = [r.package_links((root/'release'/src).read_text(), 'abc') for src in r.PACKAGED_DOCS]
        images = r.packaged_images(texts)
        self.assertEqual(images, ['images/stock-to-wrist-rs.svg', 'images/wrist-rs-to-stock.svg'])
        for image in images:
            self.assertTrue((root/'release'/image).is_file(), image)
        packaged = set(r.PACKAGED_DOCS.values()) | {'LICENSE'} | {'tools/' + t for t in r.PACKAGED_TOOLS} | set(images)
        for src, text in zip(r.PACKAGED_DOCS, texts):
            for link in r.re.findall(r'(?:\]\(|src=")([^)"#\s]+)', text):
                if not r.re.match(r'[a-z]+:', link):
                    self.assertIn(link, packaged, f'{src}: {link}')

    def test_maximum_layout(self):
        l, a = raw(0x08060000, 32768), raw(0x08068000, 524288)
        r.validate_image(l, a, l+a)


def git(d, *args, when=None):
    env = dict(os.environ, GIT_AUTHOR_DATE=f'@{when} +0000', GIT_COMMITTER_DATE=f'@{when} +0000') if when else None
    subprocess.run(['git', '-c', 'user.name=t', '-c', 'user.email=t@example.org', '-c', 'commit.gpgsign=false',
                    *args], cwd=d, env=env, check=True, capture_output=True)


class BuildId(unittest.TestCase):
    """The build ID names the binaries' inputs, so the gate's commit rebuilds the gated bytes."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.d = Path(self.tmp.name)
        files = {'release/build-inputs.txt': (r.ROOT/'release/build-inputs.txt').read_text(),
                 'apps/standalone-boot/src/main.rs': 'fn main() {}\n', 'apps/wrist-loader/LAYOUT.md': 'map\n',
                 'crates/wrist-glass/src/lib.rs': '\n', 'crates/wrist-glass/tests/face.rs': '\n',
                 'rust-toolchain.toml': '[toolchain]\n', 'release/gate.json': '{}\n', 'release/GATE.md': 'gate\n',
                 'README.md': 'readme\n', 'site/index.html': '<p>\n', '.github/workflows/firmware.yml': 'on: push\n',
                 'tools/release-build.py': '\n', 'tools/firmware_build.py': '\n'}
        for name, text in files.items():
            (self.d/name).parent.mkdir(parents=True, exist_ok=True)
            (self.d/name).write_text(text)
        git(self.d, 'init', '-q')
        git(self.d, 'add', '.')
        git(self.d, 'commit', '-qm', 'base', when=1000)

    def tearDown(self):
        self.tmp.cleanup()

    def commit(self, name, text, when):
        (self.d/name).write_text(text)
        git(self.d, 'commit', '-qam', name, when=when)

    def epoch(self):
        return r.firmware_build.environment({}, '/sdk', '', self.d)['SOURCE_DATE_EPOCH']

    def test_other_commits_keep_it(self):
        base = r.firmware_build.build_id(self.d)
        self.assertRegex(base, r'^[0-9a-f]{12}$')
        for when, name in enumerate(['release/gate.json', 'release/GATE.md', 'README.md', 'site/index.html',
                                     'apps/wrist-loader/LAYOUT.md', 'crates/wrist-glass/tests/face.rs',
                                     '.github/workflows/firmware.yml', 'tools/release-build.py'], 2000):
            self.commit(name, 'changed\n', when)
            self.assertEqual(r.firmware_build.build_id(self.d), base, name)
        self.assertEqual(self.epoch(), '1000')

    def test_inputs_change_it(self):
        seen = {r.firmware_build.build_id(self.d)}
        for when, name in enumerate(['apps/standalone-boot/src/main.rs', 'crates/wrist-glass/src/lib.rs',
                                     'rust-toolchain.toml', 'tools/firmware_build.py'], 2000):
            self.commit(name, 'changed\n', when)
            seen.add(r.firmware_build.build_id(self.d))
            self.assertEqual(self.epoch(), str(when))
        self.assertEqual(len(seen), 5)

    def test_uncommitted_inputs_are_dirty(self):
        base = r.firmware_build.build_id(self.d)
        (self.d/'README.md').write_text('edited\n')
        (self.d/'site/new.html').write_text('\n')
        self.assertEqual(r.firmware_build.build_id(self.d), base)
        (self.d/'crates/wrist-glass/src/new.rs').write_text('\n')
        self.assertEqual(r.firmware_build.build_id(self.d), base + '-dirty')
        (self.d/'crates/wrist-glass/src/new.rs').unlink()
        (self.d/'apps/standalone-boot/src/main.rs').write_text('fn main() { loop {} }\n')
        self.assertEqual(r.firmware_build.build_id(self.d), base + '-dirty')

    def test_matches_git_alone(self):
        """The computation build.rs repeats, with git alone."""
        specs = r.firmware_build.inputs(self.d)
        listing = subprocess.check_output(['git', 'ls-files', '-s', '-z', '--', *specs], cwd=self.d)
        digest = subprocess.check_output(['git', 'hash-object', '--stdin'], cwd=self.d, input=listing).decode()
        self.assertEqual(r.firmware_build.build_id(self.d), digest[:12])


GATE = {'stock_version': '1.6.1', 'hardware_install_numbers': [1], 'round_trip_validated': True,
        'license_distribution_approved': True, 'notices_reviewed': True, 'github_candidate_run_verified': True,
        'image_crc32': '0A1B2C3D'}


class GatedImage(unittest.TestCase):
    """A release is the image the gate installed: its CRC is gate.json's."""

    def test_recipe(self):
        self.assertEqual(r.gate_open(GATE, '0A1B2C3D'), [])
        self.assertEqual(r.gate_open(GATE, '0A1B2C3E'), ['image CRC 0A1B2C3E is not the gated 0A1B2C3D'])
        for crc in ['', None]:
            self.assertEqual(r.gate_open(dict(GATE, image_crc32=crc), '0A1B2C3D'), ['no gated image CRC'])
        self.assertTrue(r.gate_open(dict(GATE, notices_reviewed=False), '0A1B2C3D'))
        self.assertIn('image_crc32', json.loads((r.ROOT/'release/gate.json').read_text()))

    def publish(self, gate, crc):
        """Run firmware.yml's publish checks on one archive built with `crc`; True when they pass."""
        workflow = (r.ROOT/'.github/workflows/firmware.yml').read_text()
        job = workflow[workflow.index('\n  publish:'):]
        scripts = re.findall(r"python3 - <<'PY'\n(.*?)\n *PY\n", job, re.S)
        self.assertEqual(len(scripts), 2)
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d/'release').mkdir(); (d/'assets').mkdir()
            (d/'release/gate.json').write_text(json.dumps(gate))
            manifest = {'release_ready': True, 'checks_skipped': False, 'dirty': False, 'source_commit': 'abc',
                        'version': '1.2.3', 'image_crc32': crc}
            with zipfile.ZipFile(d/'assets/wrist-rs-1.2.3.zip', 'w') as z:
                z.writestr('manifest.json', json.dumps(manifest))
            env = dict(os.environ, SOURCE_SHA='abc', RELEASE_TAG='v1.2.3')
            return all(subprocess.run([sys.executable, '-c', textwrap.dedent(s)], cwd=d, env=env,
                                      capture_output=True).returncode == 0 for s in scripts)

    def test_publish_job(self):
        self.assertTrue(self.publish(GATE, '0A1B2C3D'))
        self.assertFalse(self.publish(GATE, '0A1B2C3E'))
        self.assertFalse(self.publish(dict(GATE, image_crc32=''), '0A1B2C3D'))
        self.assertFalse(self.publish(dict(GATE, image_crc32=''), ''))


if __name__ == '__main__':
    unittest.main()
