#!/usr/bin/env python3
"""Package safety checks without firmware compilation or hardware."""
import argparse
import ast
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
from unittest import mock
import zlib
import zipfile
import warnings

spec = importlib.util.spec_from_file_location('release_build', Path(__file__).with_name('release-build.py'))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)

(LOADER, LOADER_SIZE), (APP, APP_SIZE) = r.layout()
VECTORS = r.sram_map()


def raw(addr, size=64, sp=VECTORS['STACK_START']):
    return struct.pack('<II', sp, addr + 9) + bytes(size - 8)


def tool_constant(tool, name):
    """A tool's module-level `name = <expression>`, evaluated without running the tool."""
    for node in ast.parse((r.ROOT/'tools'/tool).read_text()).body:
        if isinstance(node, ast.Assign) and [t.id for t in node.targets if isinstance(t, ast.Name)] == [name]:
            return eval(compile(ast.Expression(node.value), tool, 'eval'), {})
    raise KeyError(f'{tool}: {name}')


def uapp(version=0x010203, flags=0x01, icons=b'\x41', gui=16):
    """A utility .uapp: header, 60x60 and 30x30 icons, an 8-byte service, then the GUI."""
    b = struct.pack('<QIIII16sII', 0x854CD3EFBD7BA411, version, 3, 8, flags,
                    b'Wrist-rs', 3600, 900) + icons * 3600 + icons * 900 + b'UAPP' + bytes(4) \
        + (b'UAPP' + bytes(gui - 4) if gui else b'')
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
        l, a = raw(LOADER), raw(APP)
        image = l + b'\xff' * (LOADER_SIZE-len(l)) + a
        self.assertEqual(r.validate_image(l, a, image), f'{zlib.crc32(image):08X}')
        for loader, app, combined in [(b'', a, image), (l, b'', image),
                (l, a, image[:-1]), (l, a, image[:64]+bytes(32704)+a),
                (l, raw(LOADER), image), (bytes(LOADER_SIZE+1), a, image),
                (l, raw(APP, APP_SIZE+1), image), (l, bytes(64), image)]:
            with self.assertRaises(ValueError):
                r.validate_image(loader, app, combined)
        # Outside the window, then inside it but 4-aligned.
        for sp in [VECTORS['SP_MAX'] + 8, VECTORS['STACK_START'] - 4]:
            bad = raw(APP, sp=sp)
            with self.assertRaises(ValueError, msg=hex(sp)):
                r.validate_image(l, bad, l + b'\xff' * (LOADER_SIZE-len(l)) + bad)

    def test_uapp_identity_crc_header_and_abi(self):
        self.assertEqual(r.validate_uapp(uapp(), '1.2.3')['libc_abi'], '0.0.3')
        self.assertEqual(r.validate_uapp(uapp(), '1.2.3')['gui_bytes'], 16)
        for bad in [b'', uapp()[:-1], uapp()[:-4]+bytes(4), uapp(0x010204)]:
            with self.assertRaises(ValueError):
                r.validate_uapp(bad, '1.2.3')
        for offset in [0, 12, 16, 20, 40, 44]:
            b = bytearray(uapp()[:-4]); b[offset] ^= 1
            with self.assertRaises(ValueError):
                r.validate_uapp(bytes(b)+struct.pack('<I', zlib.crc32(b)), '1.2.3')

    def test_uapp_is_a_utility_only_the_launcher_starts(self):
        # A glance, the glance bit, autostart and the other types are all refused.
        for flags in [0x02, 0x22, 0x21, 0x09, 0x00, 0x03]:
            with self.assertRaises(ValueError, msg=hex(flags)):
                r.validate_uapp(uapp(flags=flags), '1.2.3')
        # A utility app needs its GUI and its launcher icons.
        for bad in [uapp(gui=0), uapp(icons=b'\0')]:
            with self.assertRaises(ValueError):
                r.validate_uapp(bad, '1.2.3')
        # The service's size has to land on the GUI's magic.
        b = bytearray(uapp()[:-4]); b[-16] ^= 1
        with self.assertRaises(ValueError):
            r.validate_uapp(bytes(b) + struct.pack('<I', zlib.crc32(b)), '1.2.3')

    def test_combine_refuses_missing_empty_and_oversize(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for lsize, asize in [(0, 8), (8, 0), (LOADER_SIZE+1, 8), (8, APP_SIZE+1)]:
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
                'sdk/libstdc++/GPL-3.0-or-later.txt'])
            for args in [(d/'sdk', [d/'missing'], str(gcc)), (d/'sdk', [d/'rust'], None),
                         (d/'sdk', [d/'rust'], str(d/'rust/MIT.txt')), (d/'rust', [d/'rust'], str(gcc))]:
                with self.assertRaises(ValueError):
                    r.collect_licenses(d/'again', *args)

    def test_package_licenses_include_font_licences(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for name in ['LICENSE-MIT', 'COPYRIGHT', 'README.md', 'Fonts/OFL-Poppins.txt', 'Fonts/FONTS.md',
                         'Fonts/Poppins-Regular.ttf', 'src/license.rs']:
                (d/name).parent.mkdir(parents=True, exist_ok=True); (d/name).write_text(name)
            self.assertEqual([p.as_posix() for p in r.package_licenses(d)],
                             ['COPYRIGHT', 'Fonts/OFL-Poppins.txt', 'LICENSE-MIT'])

    def test_licenses_follow_only_what_is_linked(self):
        def dep(pkg, *kinds):
            return {'pkg': pkg, 'dep_kinds': [{'kind': k, 'target': None} for k in kinds]}
        metadata = {
            'workspace_members': ['app', 'tool'],
            'packages': [{'id': i, 'name': i} for i in ['app', 'tool', 'path-crate', 'used', 'macro', 'test-only',
                                                         'build-only', 'both', 'tool-dep']],
            'resolve': {'nodes': [
                {'id': 'app', 'deps': [dep('path-crate', None), dep('test-only', 'dev'), dep('build-only', 'build'),
                                       dep('both', 'dev', None)]},
                {'id': 'path-crate', 'deps': [dep('used', None), dep('macro', None), dep('test-only', 'dev')]},
                {'id': 'tool', 'deps': [dep('tool-dep', None)]},
                *({'id': i, 'deps': []} for i in ['used', 'macro', 'test-only', 'build-only', 'both', 'tool-dep'])]}}
        self.assertEqual([p['name'] for p in r.linked_packages(metadata, ['app'])],
                         ['app', 'both', 'macro', 'path-crate', 'used'])
        with self.assertRaises(ValueError):
            r.linked_packages(metadata, ['used'])

    def test_the_installer_and_the_image_ship_their_font_licences(self):
        for label, workspace, roots, features in r.LINKED:
            if label == 'loader':
                continue
            metadata = r.cargo_metadata(workspace, features)
            incised = [p for p in r.linked_packages(metadata, roots) if p['name'] == 'incised-text']
            self.assertEqual(len(incised), 1)
            base = Path(incised[0]['manifest_path']).parent
            shipped = [p.as_posix() for p in r.package_licenses(base)]
            for font in r.FONT_LICENSES:
                self.assertIn('Fonts/' + font, shipped)

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
        screens = ['checking', 'hold', 'installing', 'no-image', 'ready', 'refused']
        self.assertEqual(images, [f'images/installer-{s}.png' for s in screens]
                         + ['images/stock-to-wrist-rs.svg', 'images/wrist-rs-to-stock.svg'])
        for image in images:
            self.assertTrue((root/'release'/image).is_file(), image)
        packaged = set(r.PACKAGED_DOCS.values()) | {'LICENSE'} | {'tools/' + t for t in r.PACKAGED_TOOLS} | set(images)
        for src, text in zip(r.PACKAGED_DOCS, texts):
            for link in r.re.findall(r'(?:\]\(|src=")([^)"#\s]+)', text):
                if not r.re.match(r'[a-z]+:', link):
                    self.assertIn(link, packaged, f'{src}: {link}')

    def test_maximum_layout(self):
        l, a = raw(LOADER, LOADER_SIZE), raw(APP, APP_SIZE)
        r.validate_image(l, a, l+a)

    def test_the_tools_use_the_linker_layout(self):
        self.assertEqual(tool_constant('wrist-update.py', 'LOADER'), (LOADER, LOADER_SIZE))
        self.assertEqual(tool_constant('wrist-update.py', 'APPLICATION'), (APP, APP_SIZE))
        self.assertEqual(tool_constant('combine-image.py', 'LOADER_SIZE'), LOADER_SIZE)
        self.assertEqual(tool_constant('combine-image.py', 'INSTALLER_MAX'), LOADER_SIZE + APP_SIZE)

    def test_the_tools_find_the_console_by_its_usb_id(self):
        import wrist_port
        console = (r.ROOT/'apps/standalone-boot/src/console.rs').read_text()
        vid, pid = (int(re.search(rf'const USB_{n}: u16 = (0x[0-9A-Fa-f]+);', console)[1], 16) for n in ('VID', 'PID'))
        self.assertEqual((wrist_port.VID, wrist_port.PID), (vid, pid))
        rules = (r.ROOT/'tools/60-wrist-rs.rules').read_text()
        self.assertIn(f'ATTRS{{idVendor}}=="{vid:04x}", ATTRS{{idProduct}}=="{pid:04x}"', rules)
        self.assertIn(f'{vid:04x}:{pid:04x}', (r.ROOT/'release/INSTALL.md').read_text())
        self.assertIn(f'{vid:04X}:{pid:04X}', (r.ROOT/'HARDWARE.md').read_text())

    def test_wrist_update_checks_vectors_as_the_watch_does(self):
        tool = ast.parse((r.ROOT/'tools/wrist-update.py').read_text())
        scope = {'struct': struct, **{n: tool_constant('wrist-update.py', n) for n in ('SP_MIN', 'SP_MAX')}}
        exec(compile(ast.Module([n for n in tool.body if getattr(n, 'name', '') == 'vectors_ok'], []), 'wrist-update.py', 'exec'), scope)
        sp, end = VECTORS['STACK_START'], APP + APP_SIZE
        for vectors, ok in [((VECTORS['SP_MIN'], APP + 1), True), ((VECTORS['SP_MAX'], APP + 1), True),
                            ((VECTORS['SP_MIN'] - 1, APP + 1), False), ((VECTORS['SP_MAX'] + 1, APP + 1), False),
                            ((sp, end - 1), True), ((sp, end + 1), False), ((sp, APP - 1), False), ((sp, APP), False)]:
            self.assertEqual(scope['vectors_ok'](struct.pack('<II', *vectors), APP, APP_SIZE), ok, [hex(v) for v in vectors])

    def test_the_tools_carry_the_images_console_service(self):
        rust = lambda path, name: re.search(rf'const {name}: [^=]+= ([^;]+);', (r.ROOT/path).read_text())[1]
        ble = 'apps/standalone-boot/src/ble.rs'
        base = [int(b, 16) for b in re.findall(r'0x([0-9A-F]{2})', rust(ble, 'NUS_BASE'))]
        for name in ('RX', 'TX'):
            b = base.copy()
            b[12] = int(rust(ble, f'NUS_{name}'), 16)
            h = bytes(reversed(b)).hex()
            self.assertEqual(tool_constant('wrist_port.py', name), f'{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}', name)
        default, header = (int(rust('crates/bluenrg2/src/lib.rs', n)) for n in ('ATT_DEFAULT_MTU', 'ATT_HEADER_LEN'))
        largest = int(rust(ble, 'MAX_MTU'))
        self.assertEqual((tool_constant('wrist-ble.py', 'DEFAULT_MTU'), tool_constant('wrist-ble.py', 'ATT_HEADER')), (default, header))
        self.assertEqual(tool_constant('wrist-ble.py', 'VALUE_LEN'), largest - header)
        self.assertEqual(rust(ble, 'NUS_VALUE_LEN'), 'MAX_MTU - ATT_HEADER_LEN')
        self.assertEqual(rust(ble, 'DEFAULT_MTU'), 'bluenrg2::ATT_DEFAULT_MTU')
        self.assertEqual(rust('crates/wrist-ble/src/lib.rs', 'ATT_MAX_MTU'), str(largest))
        self.assertIn(f'pub type Payload = Bytes<{{ ATT_MAX_MTU - {header} }}>;', (r.ROOT/'crates/wrist-ble/src/lib.rs').read_text())

    def test_a_build_id_has_the_digits_wrist_crash_packs(self):
        crash = (r.ROOT/'crates/wrist-crash/src/lib.rs').read_text()
        digits = int(re.search(r'pub const BUILD_HASH_DIGITS: usize = (\d+);', crash)[1])
        self.assertEqual(r.firmware_build.BUILD_HASH_DIGITS, digits)
        self.assertEqual(tool_constant('wrist-crash.py', 'BUILD_HASH_DIGITS'), digits)
        self.assertIn(f'hash.get(..{digits})?', (r.ROOT/'apps/standalone-boot/build.rs').read_text())

    def test_wrist_frame_reads_the_face_s_dump(self):
        glass = (r.ROOT/'crates/wrist-glass/src/lib.rs').read_text()
        for name, tool in [('WIDTH', 'W'), ('HEIGHT', 'H')]:
            self.assertEqual(tool_constant('wrist-frame.py', tool), int(re.search(rf'pub const {name}: u32 = (\d+);', glass)[1]), name)
        dump = (r.ROOT/'apps/standalone-boot/src/glass.rs').read_text()
        self.assertIn(f'" {tool_constant("wrist-frame.py", "FORMAT")} frame "', dump)

    def test_wrist_crash_py_hashes_as_wrist_crash(self):
        crash = (r.ROOT/'crates/wrist-crash/src/lib.rs').read_text()
        for name in ['FNV_OFFSET', 'FNV_PRIME']:
            value = int(re.search(rf'pub const {name}: u32 = 0x([0-9A-F_]+);', crash)[1].replace('_', ''), 16)
            self.assertEqual(tool_constant('wrist-crash.py', name), value, name)
        spec = importlib.util.spec_from_file_location('wrist_crash', r.ROOT/'tools/wrist-crash.py')
        tool = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(tool)
        pinned = re.search(r'fnv1a\(b"src/main.rs"\), 0x([0-9A-F_]+)\)', (r.ROOT/'crates/wrist-crash/tests/record.rs').read_text())[1]
        self.assertEqual(tool.fnv1a(b'src/main.rs'), int(pinned.replace('_', ''), 16))


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
        seen = {base}
        (self.d/'crates/wrist-glass/src/new.rs').write_text('\n')
        seen.add(r.firmware_build.build_id(self.d))
        (self.d/'crates/wrist-glass/src/new.rs').unlink()
        main = self.d/'apps/standalone-boot/src/main.rs'
        main.write_text('fn main() { loop {} }\n')
        seen.add(r.firmware_build.build_id(self.d))
        main.write_text('fn main() { loop { break } }\n')
        seen.add(r.firmware_build.build_id(self.d))
        main.unlink()
        seen.add(r.firmware_build.build_id(self.d))
        git(self.d, 'checkout', '--', '.')
        self.assertEqual(r.firmware_build.build_id(self.d), base)
        self.assertEqual(len(seen), 5)
        self.assertTrue(all(i.endswith('-dirty') for i in seen - {base}), seen)

    def test_an_uncommitted_tree_is_hashed_as_it_stands(self):
        """Two trees that differ by an edit have different IDs; the same edit gives the same one."""
        main = self.d/'apps/standalone-boot/src/main.rs'
        main.write_text('fn main() { loop {} }\n')
        dirty = r.firmware_build.build_id(self.d)
        self.commit('apps/standalone-boot/src/main.rs', 'fn main() { loop {} }\n', 2000)
        clean = r.firmware_build.build_id(self.d)
        git(self.d, 'reset', '-q', '--hard', 'HEAD~1')
        main.write_text('fn main() { loop {} }\n')
        self.assertEqual(r.firmware_build.build_id(self.d), dirty)
        self.assertNotEqual(dirty, clean + '-dirty')
        # Staged, still uncommitted.
        git(self.d, 'add', 'apps/standalone-boot/src/main.rs')
        staged = r.firmware_build.build_id(self.d)
        self.assertTrue(staged.endswith('-dirty'))

    def test_matches_git_alone(self):
        """The computation build.rs repeats, with git alone."""
        specs = r.firmware_build.inputs(self.d)
        listing = subprocess.check_output(['git', 'ls-files', '-s', '-z', '--', *specs], cwd=self.d)
        digest = subprocess.check_output(['git', 'hash-object', '--stdin'], cwd=self.d, input=listing).decode()
        self.assertEqual(r.firmware_build.build_id(self.d), digest[:12])

    def test_build_rs_pins_what_the_recipe_builds(self):
        """build.rs marks a build whose target or opt-level is not the recipe's; its copies agree."""
        app = r.ROOT/'apps/standalone-boot'
        build_rs = (app/'build.rs').read_text()
        pin = lambda name: re.search(rf'(?m)^const {name}: &str = "([^"]*)";$', build_rs)[1]
        config = (app/'.cargo/config.toml').read_text()
        self.assertEqual(re.search(r'(?ms)^\[build\]$.*?^target = "([^"]*)"$', config)[1], pin('TARGET'))
        self.assertEqual(pin('TARGET'), r.CONFIG['target'])
        profile = re.search(r'(?ms)^\[profile\.release\]$(.*?)^\[', (app/'Cargo.toml').read_text())[1]
        self.assertEqual(re.search(r'(?m)^opt-level = "([^"]*)"$', profile)[1], pin('OPT_LEVEL'))
        # The features are read from toolchain.json, not copied.
        self.assertIn('release/toolchain.json', build_rs)
        self.assertNotRegex(build_rs, r'(?m)^const \w*FEATURES')
        self.assertEqual(r.firmware_build.app_features(), sorted(r.CONFIG['app_features']))
        self.assertEqual(r.firmware_build.feature_args('wrist-loader'), [])


class BuildImage(unittest.TestCase):
    """CI and a local build run the recipe the same way, in the image toolchain.json pins."""

    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('release_docker', Path(__file__).with_name('release-docker.py'))
        cls.d = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.d)

    def test_toolchain_json(self):
        self.assertEqual(r.CONFIG['build_image'].split('@')[0], 'ghcr.io/tobymurray/wrist-rs-build')
        self.assertEqual(self.d.sdk_repository(r.CONFIG), 'UNAWatch/una-sdk')
        self.assertEqual(self.d.sdk_repository({'sdk_url': 'https://github.com/tobymurray/una-sdk.git'}),
                         'tobymurray/una-sdk')
        with self.assertRaises(ValueError):
            self.d.sdk_repository({'sdk_url': 'https://example.org/una-sdk'})
        dockerfile = (r.ROOT/'release/Dockerfile').read_text()
        for pin in [f'--default-toolchain {r.CONFIG["stable"]} ', f'--target {r.CONFIG["target"]}',
                    f'toolchain install {r.CONFIG["nightly"]} ']:
            self.assertIn(pin, dockerfile)
        self.assertRegex(dockerfile, r'(?m)^FROM \S+@sha256:[0-9a-f]{64}$')

    def test_placeholder_refused(self):
        digest = 'ghcr.io/tobymurray/wrist-rs-build@sha256:' + '0123456789abcdef' * 4
        self.assertEqual(self.d.pinned_image({'build_image': digest}), digest)
        for bad in ['', 'ghcr.io/tobymurray/wrist-rs-build@sha256:UNPUBLISHED', 'ghcr.io/tobymurray/wrist-rs-build:latest',
                    'wrist-rs-build:local', digest[:-1], digest + ' ']:
            with self.assertRaises(ValueError):
                self.d.pinned_image({'build_image': bad})
        pins = subprocess.run([sys.executable, str(r.ROOT/'tools/release-docker.py'), '--pins'],
                              capture_output=True, text=True)
        try:
            image = self.d.pinned_image(r.CONFIG)
        except ValueError:
            self.assertNotEqual(pins.returncode, 0)
            self.assertIn('toolchain.yml', pins.stderr)
            self.assertEqual(pins.stdout, '')
        else:
            self.assertEqual(pins.stdout.splitlines(), [f'build_image={image}', 'sdk_repository=UNAWatch/una-sdk',
                                                        f'sdk_commit={r.CONFIG["sdk_commit"]}'])

    def test_ci_runs_the_local_command(self):
        workflow = (r.ROOT/'.github/workflows/firmware.yml').read_text()
        self.assertNotIn('docker ', workflow)
        self.assertIn('release-docker.py --pins', workflow)
        self.assertIn('release-docker.py --sdk "$PWD/sdk" --out "$RUNNER_TEMP/firmware" --version "$version" \\\n'
                      '            --cargo-home "$PWD/cargo-home" "${extra[@]}"\n', workflow)
        # Only a pull request leaves the checks to the workflow's own jobs.
        self.assertIn('if [ "$EVENT" = pull_request ]; then extra=(--skip-checks); fi\n', workflow)
        image = 'ghcr.io/tobymurray/wrist-rs-build@sha256:' + 'ab' * 32
        argv = self.d.command(image, Path('/w/sdk'), Path('/runner/temp/firmware'), ['--version', '1.2.3'],
                              image, 1001, 118, root=Path('/w/source'))
        self.assertEqual(argv, [
            'docker', 'run', '--rm', '--platform', 'linux/amd64', '--user', '1001:118',
            '--env', 'HOME=/tmp', '--env', 'CARGO_HOME=/tmp/wrist-rs-cargo', '--workdir', '/src', '--entrypoint', 'bash',
            '-v', '/w/source:/src', '-v', '/w/sdk:/sdk:ro', '-v', '/runner/temp:/stage', image, '-c', self.d.SCRIPT, '--',
            '--sdk', '/sdk', '--out', '/stage/firmware', '--container-id', image, '--version', '1.2.3'])
        # A kept CARGO_HOME is mounted where a new one would be, so the recorded paths are the same.
        kept = self.d.command(image, Path('/w/sdk'), Path('/runner/temp/firmware'), ['--version', '1.2.3'],
                              image, 1001, 118, root=Path('/w/source'), cargo_home=Path('/w/cargo-home'))
        self.assertEqual(kept, argv[:argv.index(image)] + ['-v', '/w/cargo-home:/tmp/wrist-rs-cargo']
                         + argv[argv.index(image):])
        self.assertIn('umask 022;', self.d.SCRIPT)
        self.assertTrue(self.d.SCRIPT.endswith('exec python3 tools/release-build.py "$@"'))

    def test_paths_remapped(self):
        flags = r.firmware_build.environment({'CARGO_HOME': '/tmp/wrist-rs-cargo', 'RUSTUP_HOME': '/opt/rustup'},
                                             '/sdk', 'x')['RUSTFLAGS'].split()
        self.assertEqual(flags, [f'--remap-path-prefix={Path.home()}=/builder', '--remap-path-prefix=/tmp/wrist-rs-cargo=/cargo_home',
                                 '--remap-path-prefix=/opt/rustup=/rustup_home', f'--remap-path-prefix={r.ROOT}=/src'])


GATE = {'stock_version': '1.6.1', 'hardware_install_numbers': [1], 'round_trip_validated': True,
        'license_distribution_approved': True, 'notices_reviewed': True, 'github_candidate_run_verified': True,
        'image_crc32': '0A1B2C3D', 'round_trip_release': '1.2.2', 'round_trip_fingerprint': 'f' * 64}


class GatedImage(unittest.TestCase):
    """A release is the image the gate installed: its CRC is gate.json's."""

    def test_recipe(self):
        fp = 'f' * 64
        self.assertEqual(r.gate_open(GATE, '0A1B2C3D', fp), [])
        self.assertEqual(r.gate_open(GATE, '0A1B2C3E', fp), ['image CRC 0A1B2C3E is not the gated 0A1B2C3D'])
        for crc in ['', None]:
            self.assertEqual(r.gate_open(dict(GATE, image_crc32=crc), '0A1B2C3D', fp), ['no gated image CRC'])
        self.assertTrue(r.gate_open(dict(GATE, notices_reviewed=False), '0A1B2C3D', fp))
        self.assertEqual(len(r.gate_open(GATE, '0A1B2C3D', 'e' * 64)), 1)
        self.assertEqual(r.gate_open(dict(GATE, round_trip_fingerprint=''), '0A1B2C3D', fp), ['no round-trip fingerprint'])
        gate = json.loads((r.ROOT/'release/gate.json').read_text())
        self.assertIn('image_crc32', gate)
        self.assertEqual(len(gate['round_trip_fingerprint']), 64)

    def test_round_trip_fingerprint_ignores_only_the_version(self):
        loader = raw(LOADER)
        fp = r.round_trip_fingerprint(uapp(0x010203), loader)
        self.assertEqual(r.round_trip_fingerprint(uapp(0x000200), loader), fp)
        self.assertNotEqual(r.round_trip_fingerprint(uapp(0x010203), loader + b'\0'), fp)
        changed = bytearray(uapp(0x010203)); changed[60] ^= 1
        self.assertNotEqual(r.round_trip_fingerprint(bytes(changed), loader), fp)

    def publish(self, gate, crc, fingerprint='f' * 64):
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
                        'version': '1.2.3', 'image_crc32': crc, 'round_trip_fingerprint': fingerprint}
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
        self.assertFalse(self.publish(GATE, '0A1B2C3D', 'e' * 64))


spec = importlib.util.spec_from_file_location('gate', Path(__file__).with_name('gate.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

DRY_RUN = """una-rs image-installer
image      : image.bin, 164788 bytes
crc32      : 28E7EE86
target     : 08060000
plan code  : 0
first page : 48 (bank 1)
pages      : 21
padded len : 164800
quadwords  : 10300
staging    : bank 2 pages 128-195, 0 of 68 not blank now
unlock test: 3 (unlock OK, re-unlock OK)
flash test : 0000003F (passed)
dry run passed; installs only on the confirm on screen

-- flash forensics --
  image crc  : 28E7EE86
  OUTCOME 1: erase+program+verify all SUCCEEDED
witness page (bank 1, 45): ours
"""


class GateScript(unittest.TestCase):
    """tools/gate.py's reading of install.log and power."""

    def failed(self, log, crc='28E7EE86', size=164788):
        return [what for ok, what in gate.check_install_log(log, crc, size) if not ok]

    def test_a_good_dry_run(self):
        self.assertEqual(gate.plan_pages(), (48, 115))
        self.assertEqual(gate.record_page(), 45)
        self.assertEqual(gate.page_cycle(['ble']), ['face', 'battery', 'bluetooth', 'about'])
        self.assertEqual(gate.page_cycle([]), ['face', 'battery', 'about'])
        self.assertEqual(gate.unlock_passed(), 3)
        self.assertEqual(self.failed(DRY_RUN), [])
        self.assertTrue(all(ok for ok, _ in gate.witness_checks(DRY_RUN, '28E7EE86')))

    def test_only_the_watchs_drive_is_mounted(self):
        def disk(tran, *parts):
            return {'path': '/dev/x', 'label': None, 'mountpoint': None, 'type': 'disk', 'tran': tran,
                    'children': [{'path': p, 'label': label, 'mountpoint': at, 'type': 'part', 'tran': None}
                                 for p, label, at in parts]}
        tree = {'blockdevices': [
            disk('usb', ('/dev/sda1', 'UNA WATCH', None)),
            disk('usb', ('/dev/sdb1', 'UNA BOOT', None), ('/dev/sdb2', 'UNA BOOT', '/run/media/u/UNA BOOT')),
            disk('usb', ('/dev/sdc1', 'BACKUP', None)),
            disk('nvme', ('/dev/nvme0n1p1', 'UNA WATCH', None), ('/dev/nvme0n1p2', None, None)),
            {'path': '/dev/loop0', 'type': 'loop', 'tran': None}]}
        self.assertEqual(gate.unmounted_parts(tree), [
            ('/dev/sda1', 'UNA WATCH', True), ('/dev/sdb1', 'UNA BOOT', True),
            ('/dev/sdc1', 'BACKUP', False), ('/dev/nvme0n1p1', 'UNA WATCH', False)])

    def test_what_install_md_refuses(self):
        self.assertEqual(self.failed(DRY_RUN, crc='28E7EE87'), ['CRC 28E7EE87'])
        self.assertEqual(self.failed(DRY_RUN, size=1), ['image.bin, 1 bytes'])
        self.assertEqual(self.failed(DRY_RUN.replace('pages      : 21', 'pages      : 69')),
                         ['a plan within bank 1 pages 48 to 115'])
        self.assertEqual(self.failed(DRY_RUN.replace('dry run passed; installs only on the confirm on screen',
                                                     'refused: firmware isolates apps (2)')),
                         ['no refusal', 'dry run passed'])
        self.assertEqual(self.failed(DRY_RUN.replace('unlock test: 3', 'unlock test: 1')), ['unlock and re-unlock'])
        self.assertFalse(all(ok for ok, _ in gate.witness_checks(DRY_RUN, '00000000')))

    def test_version_names_the_build_and_the_app_bin(self):
        with tempfile.TemporaryDirectory() as d:
            g = gate.Gate(argparse.Namespace())
            g.log = lambda text: None
            g.pkg = Path(d)
            (g.pkg/'app.bin').write_bytes(b'\x00' * 64)
            crc = f'{zlib.crc32(bytes(64)):08X}'
            for build, line, ok in [('20b5b6c11988', f'build 20b5b6c11988 crc {crc}', True),
                                    ('20b5b6c11988', f'build 20b5b6c11988-dirty crc {crc}', False),
                                    ('20b5b6c11988', 'build 20b5b6c11988 crc 00000000', False),
                                    ('20b5b6c11988', 'build 20b5b6c11988', False)]:
                g.booted = lambda: f'[boot] state boot\n[usb] standalone-boot 0.3.0 {line}\n'
                self.assertEqual(g.version_is(4, build), ok, line)

    def test_wake_rate(self):
        power = ('[power] last on battery: 11 min, mean -560 uA over 12 readings\n'
                 '[sleep] policy=auto input=external stop1_wakes=0 stop2_wakes=432 stop_ms=705028')
        self.assertAlmostEqual(gate.wake_rate(power, 'stop2_wakes=0'), 432 / 11)
        # Wakes from before the stretch, as section E's update on battery leaves, don't count.
        self.assertAlmostEqual(gate.wake_rate(power.replace('=432', '=5827'), 'stop2_wakes=5395'), 432 / 11)
        self.assertIsNone(gate.wake_rate(power, ''))
        self.assertIsNone(gate.wake_rate('[power] no stretch on battery finished since boot', 'stop2_wakes=0'))

    def test_park_bits(self):
        bits = gate.mmio_bits()
        self.assertEqual(bits['RCC_AHB2ENR1_SDMMC1EN'], 1 << 27)
        report = ('PWR_CR2 = 0x00FF1670\nPWR_CR4 = 0x0FFF7DFF\nICACHE_CR   = 0x00000005\n'
                  'RCC_AHB1ENR = 0xC0201101\nRCC_AHB2ENR1= 0xC000C3FF\n')
        self.assertTrue(all(ok for ok, _ in gate.park_checks(report)))
        self.assertEqual([w for ok, w in gate.park_checks(report.replace('C000C3FF', 'C800C3FF')) if not ok],
                         ['RCC_AHB2ENR1_SDMMC1EN clear'])
        self.assertEqual([w for ok, w in gate.park_checks(report.replace('C0201101', 'D0201101')) if not ok],
                         ['RCC_AHB1ENR_BKPSRAMEN clear'])
        self.assertEqual([w for ok, w in gate.park_checks(report.replace('00FF1670', '00FF1600')) if not ok],
                         ["PWR_CR2 holds mmio.rs's SRAM bits"])

    def test_presses(self):
        log = ('[button] short press SW3/Down: face lit for 3 s\n[page] battery\n'
               "[button] short press SW4/Back: the console's light off\n")
        self.assertRegex(log, gate.press('Down', '[page] battery'))
        self.assertRegex(log, gate.press('Back', "the console's light off"))
        self.assertNotRegex(log, gate.press('Up', '[page] battery'))
        self.assertNotRegex('[button] injected short press SW3/Down: face lit for 3 s\n[page] battery\n',
                            gate.press('Down', '[page] battery'))

    def test_steps(self):
        self.assertEqual(gate.steps('4-6,11'), {4, 5, 6, 11})
        self.assertEqual(gate.run_order(gate.steps('1-22')), [4, 5, 6, 7, 8, 12, *range(13, 23), 9, 10, 11])
        self.assertEqual(gate.run_order(gate.steps('9-11')), [9, 10, 11])

    def test_modules(self):
        # 0.3.1's gate, step 7.
        text = ('[mod] view I2C1: older label "view_LS012-1_3": LS012 rev 1_3; expects LS012: expected (22155 us); drives the panel (lcd.rs)\n'
                '[mod] link1 I2C3: MI v1 BLUENRG2 rev 1.3, no serial; expects BLUENRG2: expected (21972 us); drives nothing\n'
                '[mod] link2 I2C4: MI v1 AG3335M rev 1.2, no serial; expects AG3335M: expected (6195 us); drives the IMU\n')
        self.assertTrue(all(ok for ok, _ in gate.module_checks(text)))
        other = text.replace('expects AG3335M: expected', 'expects AG3335M: another module')
        self.assertEqual([w for ok, w in gate.module_checks(other) if not ok], ['link2 record expected'])
        self.assertEqual([w for ok, w in gate.module_checks(text.split('\n', 1)[1]) if not ok], ['view record expected'])

    def test_handoff_reset_flags(self):
        # 0.3.1's gate, step 8, after the Back hold: the handoff's RCC_CSR, then the image's.
        report = ('-- RCC at handoff, before this image touched it --\nRCC_CR      = 0x00003035\n'
                  'RCC_CSR     = 0x14004400\n-- RCC now --\nRCC_CSR     = 0x00004400\n')
        self.assertEqual(gate.handoff_reset_flags(report), ['SFT', 'PIN'])
        self.assertEqual(gate.handoff_reset_flags(report.replace('0x14004400', '0x34004400')), ['IWDG', 'SFT', 'PIN'])
        self.assertIsNone(gate.handoff_reset_flags('-- RCC now --\nRCC_CSR     = 0x00004400\n'))

    def test_haptic(self):
        pin = '     HAPTIC_RST_N (PF2) low, output: the haptic driver in reset between patterns\n'
        self.assertEqual(gate.haptic_pin(pin), ('low', True))
        self.assertEqual(gate.haptic_pin(pin.replace('low, output', 'high, NOT an output')), ('high', False))
        self.assertIsNone(gate.haptic_pin('[step] park\n'))
        self.assertEqual(gate.haptic_pin_check(pin)[0], True)
        self.assertEqual(gate.haptic_pin_check(pin.replace('low, output', 'low, NOT an output')),
                         (False, 'HAPTIC_RST_N (PF2) low, an output: the haptic driver in reset (read low, NOT an output)'))
        self.assertIn('(read no PF2 line)', gate.haptic_pin_check('[step] park\n')[1])
        probe = ('[haptic] played 2, stopped 1, failed 0; full scale 2698 mV, ERM open loop; last bring-up at 139 s\n'
                 '[haptic] last error: none\n'
                 + pin +
                 '[haptic] probe: CHIP_ID 13 (DRV2625 rev 3), 07=40, 08=40, 1F=7B, 20=7D, STATUS 00; PF2 low\n')
        self.assertTrue(all(ok for ok, _ in gate.haptic_probe(probe)))
        self.assertEqual([w for ok, w in gate.haptic_probe(probe.replace('1F=7B', '1F=89(wrote 7B)')) if not ok],
                         ['probe: each register reads back as written'])
        failed = '[haptic] probe: CHIP_ID --, 07=--, 08=--, 1F=--, 20=--; failed at chip ID: no ACK; PF2 low\n'
        self.assertEqual([w for ok, w in gate.haptic_probe(failed) if not ok],
                         ['probe: CHIP_ID 13, a DRV2625', 'probe: each register reads back as written',
                          'probe: no failure, PF2 low after'])
        self.assertFalse(any(ok for ok, _ in gate.haptic_probe('[haptic] a pattern is playing; the probe follows it\n')))
        stopped = '[haptic] stopping\n[haptic] alarm stopped after 4018 ms, VDD 4853 mV; PF2 low\n'
        self.assertEqual(gate.haptic_stopped(stopped), 4018)
        # Rung out before the stop, or a tap: not what the step asks.
        self.assertIsNone(gate.haptic_stopped('[haptic] nothing playing\n'
                                              '[haptic] alarm played in 59549 ms, VDD 4853 mV, STATUS 08; PF2 low\n'))
        self.assertIsNone(gate.haptic_stopped('[haptic] tap played in 48 ms, VDD 4941 mV, STATUS 08; PF2 low\n'))

    def test_alarm(self):
        answer = '[alarm] 19:41 on (BKP4R 0x414C0C9D); rings in 2 min on the clock\n'
        self.assertTrue(gate.alarm_set(answer, '19:41'))
        self.assertTrue(gate.alarm_set(answer.replace('2 min', '3 min'), '19:41'))
        self.assertFalse(gate.alarm_set(answer, '19:42'))
        self.assertFalse(gate.alarm_set(answer.replace('2 min', '23 h 59 min'), '19:41'))
        self.assertFalse(gate.alarm_set('[alarm] usage: alarm [HH:MM|on|off|test|wake-test <s>] (24-hour, local)\n', '19:41'))
        before = ('[alarm] 09:08 on (BKP4R 0x414C0A24); rings in 2 min on the clock\n'
                  '[alarm] alarm A armed for 09:08, RTC interrupt on; 0 RTC interrupts this boot\n')
        after = ('[alarm] 09:08 on, spent by its last ring (BKP4R 0x414C1A24); rings in 23 h 59 min on the clock\n'
                 '[alarm] alarm A armed for 22:08, RTC interrupt on; 1 RTC interrupts this boot\n'
                 '[crash] no retained failure\n')
        self.assertEqual(gate.rtc_interrupts(before), 0)
        self.assertEqual(gate.rtc_interrupts(after), 1)
        self.assertTrue(all(ok for ok, _ in gate.alarm_rang(before, after, '09:08')))
        unrung = after.replace(', spent by its last ring', '').replace('; 1 RTC', '; 0 RTC')
        self.assertEqual([w for ok, w in gate.alarm_rang(before, unrung, '09:08') if not ok],
                         ['alarm 09:08 spent by its ring', 'RTC interrupts this boot 0 before, 0 after'])
        self.assertFalse(gate.alarm_rang(before, after.replace('no retained failure', 'panic line=126'), '09:08')[2][0])
        self.assertFalse(all(ok for ok, _ in gate.alarm_rang('', after, '09:08')))

    def test_charge(self):
        line = '[hb] 12  uptime 13 s\n[pwr] 4407 mV  soc 100.0%  30 C  now -1406 uA  avg 468 uA  rails all good\n'
        self.assertEqual(gate.charge(line), 100.0)
        self.assertEqual(gate.charge(line.replace('100.0%', '42.5%')), 42.5)
        self.assertIsNone(gate.charge('[power] input external (USB)\n'))

    def test_retry_waits_for_a_mount_to_take_a_write(self):
        clock = [0.0]
        tries = []

        def copy(fails):
            tries.append(clock[0])
            if len(tries) <= fails:
                raise PermissionError(13, 'Permission denied')
            return 'copied'
        sleep = lambda s: clock.__setitem__(0, clock[0] + s)
        now = lambda: clock[0]
        self.assertEqual(gate.retry(lambda: copy(3), 10, now=now, sleep=sleep), ('copied', None))
        self.assertEqual(len(tries), 4)
        tries.clear()
        value, error = gate.retry(lambda: copy(99), 10, now=now, sleep=sleep)
        self.assertIsNone(value)
        self.assertIsInstance(error, PermissionError)
        self.assertLessEqual(tries[-1] - tries[0], 10.5)

    def test_writable(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertTrue(gate.writable(Path(d)))
            self.assertEqual(list(Path(d).iterdir()), [])
            self.assertFalse(gate.writable(Path(d)/'gone'))

    def test_ota_problem(self):
        with tempfile.TemporaryDirectory() as d:
            ota = Path(d)/'UnaWatch-Kernel_1.6.2.ota'
            ota.write_bytes(b'ota')
            self.assertIsNone(gate.ota_problem(ota, '1.6.2'))
            self.assertIn('pass --ota', gate.ota_problem(None, '1.6.2'))
            gone = Path(d)/'deleted'/ota.name
            self.assertEqual(gate.ota_problem(gone, '1.6.2'), f'--ota {gone} not found')
            self.assertIn('is not stock 1.6.1', gate.ota_problem(ota, '1.6.1'))

    def test_steps_13_and_17_ask_only_for_their_holds(self):
        with tempfile.TemporaryDirectory() as d:
            boot = Path(d)/'UNA BOOT'
            boot.mkdir()
            ota = Path(d)/'UnaWatch-Kernel_1.6.2.ota'
            ota.write_bytes(b'ota')
            g = gate.Gate(argparse.Namespace(ota=ota))
            g.stock = '1.6.2'
            asked = []
            g.log = lambda text: None
            g.do = lambda step, what: asked.append((step, what))
            g.ask = lambda step, what: asked.append((step, what)) or True
            g.check = lambda step, ok, what: ok
            g.drive = lambda step, want, what: boot
            g.eject = lambda step, path: None
            g.step13()
            self.assertTrue(g.power_reset)
            self.assertEqual(asked[0][1], gate.POWER_RESET)
            self.assertIn(gate.INTO_BOOTLOADER, [w for _, w in asked])
            self.assertEqual((boot/ota.name).read_bytes(), b'ota')
            asked.clear()
            g.to_stock(17)
            self.assertNotIn(gate.POWER_RESET, [w for _, w in asked])
            self.assertIn(gate.INTO_BOOTLOADER, [w for _, w in asked])

    def test_step_16_sets_the_clock_before_the_face(self):
        g = gate.Gate(argparse.Namespace())
        order = []
        g.log = g.save = lambda *a: None
        g.ask = lambda step, what: order.append('face' if what.startswith('the face') else 'ask')
        g.check = lambda step, ok, what: ok
        g.tool = lambda *a, **k: order.append(a[0]) or (0, '[rtc] set')
        g.cmd = lambda *a, **k: ''
        g.booted = lambda: ''
        with mock.patch.object(gate.time, 'sleep'):
            for kwargs, first in [({}, ['face', 'wrist-time.py']), ({'set_first': True}, ['wrist-time.py', 'face'])]:
                order.clear()
                g.step5(16, **kwargs)
                self.assertEqual(order[:2], first)


if __name__ == '__main__':
    unittest.main()
