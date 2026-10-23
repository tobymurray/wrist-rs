#!/usr/bin/env python3
"""Package safety checks without firmware compilation or hardware."""
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
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
        self.assertEqual(r.package_links(text, 'abc'),
                         '[notes](RELEASE-NOTES.md) [guide](START-HERE.md#the-tools) [t](tools/wrist-time.py) '
                         '![x](images/logo.svg) [l](LICENSE) '
                         '[h](https://github.com/tobymurray/wrist-rs/blob/abc/HARDWARE.md#stop-modes) '
                         '[s](https://github.com/tobymurray/wrist-rs/blob/abc/tools/extract_pinmap.py) '
                         '[w](https://example.org/a.md) [a](#updating)')
        root = Path(__file__).resolve().parent.parent
        packaged = set(r.PACKAGED_DOCS.values()) | {'LICENSE'} | {'tools/' + t for t in r.PACKAGED_TOOLS} \
            | {'images/' + i.name for i in (root/'release/images').glob('*.svg')}
        for src in r.PACKAGED_DOCS:
            text = r.package_links((root/'release'/src).read_text(), 'abc')
            for link in r.re.findall(r'\]\(([^)#\s]+)', text):
                if not r.re.match(r'[a-z]+:', link):
                    self.assertIn(link, packaged, f'{src}: {link}')

    def test_maximum_layout(self):
        l, a = raw(0x08060000, 32768), raw(0x08068000, 524288)
        r.validate_image(l, a, l+a)


if __name__ == '__main__':
    unittest.main()
