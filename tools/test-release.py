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

    def test_package_licenses_include_font_licences(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            for name in ['LICENSE-MIT', 'COPYRIGHT', 'README.md', 'Fonts/OFL-Poppins.txt', 'Fonts/FONTS.md',
                         'Fonts/Poppins-Regular.ttf', 'src/license.rs']:
                (d/name).parent.mkdir(parents=True, exist_ok=True); (d/name).write_text(name)
            self.assertEqual([p.as_posix() for p in r.package_licenses(d)],
                             ['COPYRIGHT', 'Fonts/OFL-Poppins.txt', 'LICENSE-MIT'])

    def test_the_installer_ships_its_font_licences(self):
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version=1'],
                                                      cwd=r.ROOT))
        incised = [p for p in metadata['packages'] if p['name'] == 'incised-text']
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
        self.assertIn('release-docker.py --sdk "$PWD/sdk" --out "$RUNNER_TEMP/firmware" --version "$version"\n',
                      workflow)
        image = 'ghcr.io/tobymurray/wrist-rs-build@sha256:' + 'ab' * 32
        argv = self.d.command(image, Path('/w/sdk'), Path('/runner/temp/firmware'), ['--version', '1.2.3'],
                              image, 1001, 118, root=Path('/w/source'))
        self.assertEqual(argv, [
            'docker', 'run', '--rm', '--platform', 'linux/amd64', '--user', '1001:118',
            '--env', 'HOME=/tmp', '--env', 'CARGO_HOME=/tmp/wrist-rs-cargo', '--workdir', '/src', '--entrypoint', 'bash',
            '-v', '/w/source:/src', '-v', '/w/sdk:/sdk:ro', '-v', '/runner/temp:/stage', image, '-c', self.d.SCRIPT, '--',
            '--sdk', '/sdk', '--out', '/stage/firmware', '--container-id', image, '--version', '1.2.3'])
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
        loader = raw(0x08060000)
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
        self.assertEqual(self.failed(DRY_RUN), [])
        self.assertTrue(all(ok for ok, _ in gate.witness_checks(DRY_RUN, '28E7EE86')))

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

    def test_wake_rate(self):
        power = ('[power] last on battery: 11 min, mean -560 uA over 12 readings\n'
                 '[sleep] policy=auto input=external stop1_wakes=0 stop2_wakes=432 stop_ms=705028')
        self.assertAlmostEqual(gate.wake_rate(power), 432 / 11)
        self.assertIsNone(gate.wake_rate('[power] no stretch on battery finished since boot'))

    def test_park_bits(self):
        bits = gate.mmio_bits()
        self.assertEqual(bits['RCC_AHB2ENR1_SDMMC1EN'], 1 << 27)
        report = ('PWR_CR2 = 0x00FF1670\nPWR_CR4 = 0x0FFF7DFF\nICACHE_CR   = 0x00000005\n'
                  'RCC_AHB2ENR1= 0xC000C3FF\n')
        self.assertTrue(all(ok for ok, _ in gate.park_checks(report)))
        self.assertEqual([w for ok, w in gate.park_checks(report.replace('C000C3FF', 'C800C3FF')) if not ok],
                         ['RCC_AHB2ENR1_SDMMC1EN clear'])
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


if __name__ == '__main__':
    unittest.main()
