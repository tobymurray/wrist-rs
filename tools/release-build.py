#!/usr/bin/env python3
"""Build, check and package a release, natively or in release/Dockerfile; it never flashes or publishes."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
from elftools.elf.elffile import ELFFile
import re
import shutil
import struct
import subprocess
import sys
import zipfile
import zlib

import firmware_build

ROOT = Path(__file__).resolve().parents[1]
CONFIG = json.loads((ROOT / 'release/toolchain.json').read_text())
TARGET = CONFIG['target']
# firmware.yml's publish job checks the same list.
GATE_FLAGS = ['round_trip_validated', 'license_distribution_approved', 'notices_reviewed',
              'github_candidate_run_verified']


def round_trip_fingerprint(uapp, loader):
    """SHA-256 of what the stock round trip runs besides the application: the installer's
    `.uapp` without its version and CRC, and the loader. Equal fingerprints let a release
    carry an earlier release's round trip (GATE.md, section D)."""
    unversioned = uapp[:8] + bytes(4) + uapp[12:-4]
    return hashlib.sha256(unversioned + loader).hexdigest()


def gate_open(gate, crc, fingerprint):
    """Why `gate` does not release the image whose CRC is `crc`; empty when it does. The gate
    tested one image, and the build ID makes a later commit of anything else rebuild it
    byte for byte, so the CRC must be the gate's. The round trip may be an earlier
    release's, run on the same installer and loader."""
    reasons = [f'release gate open: {k}' for k in GATE_FLAGS if gate.get(k) is not True]
    if not gate.get('round_trip_fingerprint'):
        reasons.append('no round-trip fingerprint')
    elif gate['round_trip_fingerprint'] != fingerprint:
        reasons.append(f'installer or loader {fingerprint[:12]} is not the round trip\'s '
                       f'{gate["round_trip_fingerprint"][:12]}: run GATE.md section D')
    if not gate.get('hardware_install_numbers'):
        reasons.append('the round trip has no numbered hardware evidence')
    if not gate.get('stock_version'):
        reasons.append('no stock version validated for the installer')
    if not gate.get('image_crc32'):
        reasons.append('no gated image CRC')
    elif gate['image_crc32'] != crc:
        reasons.append(f'image CRC {crc} is not the gated {gate["image_crc32"]}')
    return reasons


def run(*args, cwd=ROOT, env=None):
    print('+', *map(str, args), flush=True)
    subprocess.run(list(map(str, args)), cwd=cwd, env=env, check=True)


def output(*args, cwd=ROOT):
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def app_version(manifest):
    m = re.search(r'(?m)^\[package\]\n(?:(?!\[).*\n)*?version\s*=\s*"([^"]*)"', manifest)
    require(m, 'no [package] version')
    return m[1]


def require_app_versions(version, root=ROOT):
    """The tag's A.B.C is both apps' Cargo version, which the firmware's version command prints."""
    for app in ['standalone-boot', 'wrist-loader']:
        found = app_version((root/'apps'/app/'Cargo.toml').read_text())
        require(found == version.split('-')[0], f'apps/{app} is version {found}, not {version}')


def layout():
    """((loader address, size), (app address, size)) from the linker scripts, checked against MAX_PAGES."""
    text = (ROOT / 'apps/wrist-loader/memory.x').read_text()
    regions = {n: (int(a, 16), int(k) * 1024) for n, a, k in
               re.findall(r'(FLASH|ACTIVE)\s*:\s*ORIGIN = (0x[0-9A-Fa-f]+), LENGTH = (\d+)K', text)}
    loader, app = regions['FLASH'], regions['ACTIVE']
    plan = (ROOT / 'crates/wrist-flash/src/plan.rs').read_text()
    pages = int(re.search(r'pub const MAX_PAGES: \w+ = (\d+)', plan)[1])
    require(loader[0] + loader[1] == app[0], 'noncontiguous linker layout')
    require(loader[1] + app[1] == pages * 8192, 'installer/linker size mismatch')
    app_text = (ROOT / 'apps/standalone-boot/memory.x').read_text()
    require(f'0x{app[0]:08X}' in app_text and f'LENGTH = {app[1] // 1024}K' in app_text,
            'application linker mismatch')
    return loader, app


def validate_raw(data, address, limit):
    require(8 <= len(data) <= limit, f'invalid firmware size {len(data)} (limit {limit})')
    sp, reset = struct.unpack_from('<II', data)
    require(0x20000000 <= sp <= 0x20270000 - 16 and sp % 8 == 0, 'invalid stack vector')
    require(reset & 1 and address <= reset - 1 < address + len(data), 'invalid reset/load address')


def validate_image(loader, app, image):
    lreg, areg = layout()
    validate_raw(loader, *lreg)
    validate_raw(app, *areg)
    expected = loader + b'\xff' * (lreg[1] - len(loader)) + app
    require(image == expected, 'combined image/padding/components mismatch')
    return f'{zlib.crc32(image):08X}'


# The header's flags, as the SDK's app_merging.py packs them; only a bare utility app passes.
UAPP_TYPE_UTILITY = 0x01


def validate_uapp(data, version):
    """The installer's .uapp: a utility app with launcher icons, a service and a GUI, and no flag
    that lets anything but the launcher start it."""
    require(len(data) >= 52, 'truncated uapp')
    require(zlib.crc32(data[:-4]) == struct.unpack_from('<I', data, len(data)-4)[0], 'uapp CRC mismatch')
    appid, ver, abi, service, flags, name, large, small = struct.unpack_from('<QIIII16sII', data)
    major, minor, patch = map(int, version.split('-')[0].split('.'))
    require(appid == 0x854CD3EFBD7BA411 and ver == major << 16 | minor << 8 | patch,
            'uapp identity/version mismatch')
    require(abi == 3 and flags == UAPP_TYPE_UTILITY and name.rstrip(b'\0') == b'Wrist-rs',
            'uapp ABI/type mismatch')
    require(large == 3600 and small == 900, 'uapp icon sizes mismatch')
    require(any(data[48:48 + large]) and any(data[48 + large:48 + large + small]), 'uapp without icons')
    # Each process is packed by the SDK's app_packer.py, whose image starts with this magic.
    first = 48 + large + small
    gui = len(data) - 4 - first - service
    require(service > 0 and gui > 0 and data[first:first + 4] == b'UAPP'
            and data[first + service:first + service + 4] == b'UAPP', 'uapp payload/header mismatch')
    return {'type': 'Utility', 'libc_abi': '0.0.3', 'service_bytes': service, 'gui_bytes': gui,
            'crc32': f'{zlib.crc32(data[:-4]):08X}'}


def validate_elf(path, address, limit):
    with path.open('rb') as f:
        elf = ELFFile(f)
        require(elf['e_machine'] == 'EM_ARM', 'not an ARM ELF')
        vectors = elf.get_section_by_name('.vector_table')
        require(vectors is not None and vectors['sh_addr'] == address, 'ELF vector address mismatch')
        require(address <= elf['e_entry'] - 1 < address + limit, 'ELF entry address mismatch')
        flash = [seg for seg in elf.iter_segments() if seg['p_type'] == 'PT_LOAD'
                 and seg['p_filesz'] and address <= seg['p_paddr'] < address + limit]
        require(flash and min(seg['p_paddr'] for seg in flash) == address, 'ELF load base mismatch')
        require(all(seg['p_paddr'] + seg['p_filesz'] <= address + limit for seg in flash), 'ELF load overflow')


def audit_commit(elf):
    dis = output('arm-none-eabi-objdump', '-d', str(elf))
    functions = re.split(r'(?m)^[0-9a-f]+ <([^>]+)>:\n', dis)
    bodies = dict(zip(functions[1::2], functions[2::2]))
    require('wrist_flash_install' in bodies, 'missing RAM commit symbol')
    pending = ['wrist_flash_install']
    seen = set()
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        require(name in bodies, 'unresolved commit branch: ' + name)
        for line in bodies[name].splitlines():
            # bx lr is the return.
            m = re.search(r'\s(bx|blx)\s+(\w+)', line)
            if m and not re.fullmatch(r'[0-9a-f]+', m[2]):
                require(m[1] == 'bx' and m[2] == 'lr', 'indirect commit branch: ' + line)
            target = re.search(r'\s(?:b(?:[a-z]{2})?|bl|blx)(?:\.[nw])?\s+([0-9a-f]+) <([^>]+)>', line)
            if target:
                require(0x20000000 <= int(target[1], 16) < 0x20270000, 'commit branch outside RAM: ' + line)
                child = target[2].split('+')[0]
                if child != name:
                    pending.append(child)
    return dis, sorted(seen)


def audit_gui(elf):
    """The GUI only shows the service's verdict and sends the confirm: no flash code in it."""
    with elf.open('rb') as f:
        symbols = ELFFile(f).get_section_by_name('.symtab')
        require(symbols is not None, 'GUI ELF has no symbol table')
        flash = sorted({s.name for s in symbols.iter_symbols() if s.name.startswith('wrist_flash')})
    require(not flash, 'flash code in the GUI: ' + ', '.join(flash))


def copy(src, dest):
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)


# What the installer links from the SDK, besides UNA's own code: coreJSON and libstdc++.
SDK_LICENSES = ['LICENSE', 'THIRD-PARTY-LICENSES.md', 'ThirdParty/coreJSON/LICENSE']

# The installer's screen draws IncisedText's glyphs, rendered from these fonts under the SIL OFL 1.1.
FONT_LICENSES = ['OFL-Poppins.txt', 'OFL-AtkinsonHyperlegibleNext.txt']


# The docs in release/ as the zip holds them, under these names; the tools beside them.
PACKAGED_DOCS = {'INSTALL.md': 'START-HERE.md', 'NOTES.md': 'RELEASE-NOTES.md', 'NOTICES.md': 'NOTICES.md'}
PACKAGED_TOOLS = ['wrist-update.py', 'wrist-cmd.py', 'wrist-time.py', 'wrist-crash.py', 'wrist_port.py',
                  '60-wrist-rs.rules', 'requirements.txt']
REPOSITORY = 'https://github.com/tobymurray/wrist-rs'


def package_links(text, commit):
    """A release/ doc's relative links as the zip resolves them: to the packaged docs, tools,
    images and licence, or else to the source at `commit`."""
    def target(m):
        link, _, anchor = m[2].partition('#')
        if re.match(r'[a-z]+:', link) or not link:
            return m[0]
        path = os.path.normpath('release/' + link)
        if path.startswith('release/') and path[len('release/'):] in PACKAGED_DOCS:
            new = PACKAGED_DOCS[path[len('release/'):]]
        elif path.startswith('tools/') and path[len('tools/'):] in PACKAGED_TOOLS:
            new = path
        elif path.startswith('release/images/'):
            new = path[len('release/'):]
        elif path == 'LICENSE':
            new = path
        else:
            new = f'{REPOSITORY}/blob/{commit}/{path}'
        return f'{m[1]}({new}{"#" + anchor if anchor else ""})'
    return re.sub(r'(!?\[[^\]]*\])\(([^)\s]+)\)', target, text)


def packaged_images(texts):
    """The `images/` files the packaged docs show, by Markdown link or `<img src>`; only these ship."""
    return sorted({m for t in texts for m in re.findall(r'(?:\]\(|src=")(images/[^)"#\s]+)', t)})


def collect_licenses(dest, sdk, rust_licenses, gcc):
    for name in SDK_LICENSES:
        require((sdk/name).is_file(), 'missing SDK licence: ' + name)
        copy(sdk/name, dest/'sdk'/name)
    for name in ['GPL-3.0-or-later.txt', 'GCC-exception-3.1.txt']:
        found = [d/name for d in rust_licenses if (d/name).is_file()]
        require(found, 'missing libstdc++ licence: ' + name)
        copy(found[0], dest/'sdk/libstdc++'/name)
    require(gcc, 'missing arm-none-eabi-gcc')
    toolchain = Path(gcc).resolve().parents[1]/'share/doc/gcc-arm-none-eabi/license.txt'
    require(toolchain.is_file(), 'missing GNU Arm toolchain licence: ' + str(toolchain))
    copy(toolchain, dest/'gnu-arm-toolchain/license.txt')
    copy(ROOT/'crates/wrist-glass/LICENSE-FONT', dest/'wrist-glass-font/LICENSE')


def package_licenses(base):
    """A Cargo package's licence and notice files, relative to its directory `base`: those at its
    top, and the font licences of a package that ships glyphs, such as incised-text's
    `Fonts/OFL-*.txt`."""
    top = [p for p in base.iterdir()
           if p.is_file() and re.search(r'(?i)(license|licence|copying|notice|copyright)', p.name)]
    return sorted(p.relative_to(base) for p in top + list(base.glob('Fonts/OFL*.txt')))


def path_replacements(*paths):
    return sorted(((str(Path(path).resolve()), token) for path, token in paths if path),
                  key=lambda pair: len(pair[0]), reverse=True)


def normalize_paths(value, replacements):
    if isinstance(value, str):
        for path, token in replacements:
            value = value.replace(path, token)
        return value
    if isinstance(value, list):
        return [normalize_paths(item, replacements) for item in value]
    if isinstance(value, dict):
        return {key: normalize_paths(item, replacements) for key, item in value.items()}
    return value


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def archive(tree, dest):
    with zipfile.ZipFile(dest, 'w', zipfile.ZIP_DEFLATED) as z:
        for p in sorted(tree.rglob('*')):
            if p.is_file():
                info = zipfile.ZipInfo(p.relative_to(tree).as_posix(), (2026, 1, 1, 0, 0, 0))
                info.external_attr = 0o100644 << 16
                z.writestr(info, p.read_bytes(), compress_type=zipfile.ZIP_DEFLATED)


def verify_archive(path, debug=False):
    with zipfile.ZipFile(path) as z:
        names = z.namelist()
        require(len(names) == len(set(names)), 'duplicate ZIP entries')
        require(all(not n.startswith('/') and '..' not in Path(n).parts for n in names), 'unsafe ZIP path')
        require(not any(Path(n).name == 'ARM_INSTALL' or n.endswith('.ota') for n in names), 'arm file/vendor package')
        manifest = json.loads(z.read('manifest.json'))
        prefix = 'debug/' if debug else 'package/'
        expected = {name[len(prefix):] for name in manifest['files'] if name.startswith(prefix)}
        require(set(names) == expected | {'manifest.json', 'SHA256SUMS'}, 'unexpected or missing archive files')
        for name, entry in manifest['files'].items():
            if name.startswith(prefix):
                data = z.read(name[len(prefix):])
                require(len(data) == entry['bytes'] and hashlib.sha256(data).hexdigest() == entry['sha256'], 'archive/manifest mismatch: ' + name)
        for line in z.read('SHA256SUMS').decode().splitlines():
            digest, name = line.split('  ', 1)
            require(hashlib.sha256(z.read(name)).hexdigest() == digest, 'archive checksum mismatch')
        if debug:
            for name in ['app.elf', 'loader.elf', 'app.map', 'loader.map', 'ImageInstallerService.elf',
                         'ImageInstallerGUI.elf', 'installer-disassembly.txt']:
                require(name in names, 'missing debug asset: ' + name)
        else:
            first = 'first-install/Apps/ImageInstaller/'
            crc = validate_image(z.read('loader.bin'), z.read('app.bin'), z.read(first+'image.bin'))
            require(z.read('INSTALL-CRC.txt').decode() == crc + '\n', 'arm CRC text mismatch')
            uapp = z.read(first+'wrist-rs-installer_'+manifest['version']+'.uapp')
            validate_uapp(uapp, manifest['version'])
            require(manifest['round_trip_fingerprint'] == round_trip_fingerprint(uapp, z.read('loader.bin')),
                    'round-trip fingerprint mismatch')
            for name in ['START-HERE.md', 'LICENSE', 'NOTICES.md', 'RELEASE-NOTES.md', 'tools/requirements.txt',
                         'tools/wrist-update.py', 'tools/wrist-cmd.py', 'tools/wrist-time.py', 'tools/wrist-crash.py',
                         'tools/wrist_port.py', 'tools/60-wrist-rs.rules', 'licenses/wrist-glass-font/LICENSE',
                         'licenses/sdk/libstdc++/GPL-3.0-or-later.txt', 'licenses/sdk/libstdc++/GCC-exception-3.1.txt',
                         'licenses/gnu-arm-toolchain/license.txt']:
                require(name in names, 'missing user asset: ' + name)
            for font in FONT_LICENSES:
                require(any(re.fullmatch(r'licenses/root/incised-text-[^/]+/Fonts/' + re.escape(font), n)
                            for n in names), 'missing user asset: the installer\'s font licence ' + font)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--sdk', type=Path, required=True)
    ap.add_argument('--version', required=True, help='A.B.C[-suffix], each number <=255')
    ap.add_argument('--out', type=Path, required=True, help='new empty staging directory')
    ap.add_argument('--container-id', default='native-uncontainerized')
    ap.add_argument('--allow-dirty', action='store_true', help='local candidates only')
    ap.add_argument('--skip-checks', action='store_true', help='local iteration only; recorded')
    a = ap.parse_args()
    require(re.fullmatch(r'\d{1,3}\.\d{1,3}\.\d{1,3}(?:-[A-Za-z0-9][A-Za-z0-9.-]*)?', a.version), 'unsafe version')
    require(all(int(x) <= 255 for x in a.version.split('-')[0].split('.')), 'version byte overflow')
    a.sdk = a.sdk.resolve()
    a.out = a.out.resolve()
    require(not a.out.exists(), 'output must not exist (never replace versioned bytes)')
    require(a.out != ROOT and ROOT not in a.out.parents, 'stage outside source tree')
    require(output('rustc', '--version').split()[1] == CONFIG['stable'], 'wrong stable compiler')
    commit = output('git', 'rev-parse', 'HEAD')
    dirty = bool(output('git', 'status', '--porcelain', '--untracked-files=normal'))
    require(a.allow_dirty or not dirty, 'release requires clean source tree')
    require(output('git', 'rev-parse', 'HEAD', cwd=a.sdk) == CONFIG['sdk_commit'], 'wrong SDK revision')
    require(not output('git', 'status', '--porcelain', cwd=a.sdk), 'dirty SDK')
    tags = output('git', 'tag', '--points-at', 'HEAD').splitlines()
    release = not a.allow_dirty and a.version != '0.0.0-candidate.' + commit[:12]
    if release:
        require('v' + a.version in tags, 'version must match a tag on HEAD')
        require_app_versions(a.version)
    build_id = firmware_build.build_id()
    env = firmware_build.environment(os.environ, a.sdk, build_id)
    commands = []
    map_dir = Path('/tmp/wrist-rs-release-maps')
    map_dir.mkdir(exist_ok=True)
    # A fixed map path (firmware_build.with_map); builds sharing it take turns.
    map_lock = (map_dir / 'build.lock').open('w')
    fcntl.flock(map_lock, fcntl.LOCK_EX)
    replacements = path_replacements(
        (ROOT, '/src'), (a.sdk, '/sdk'), (a.out, '/out'),
        (os.environ.get('CARGO_HOME'), '$CARGO_HOME'),
        (os.environ.get('RUSTUP_HOME'), '$RUSTUP_HOME'),
        (Path.home(), '$HOME'), (sys.executable, 'python3'))

    def execute(*args, cwd=ROOT, env=env):
        commands.append(normalize_paths(
            {'cwd': str(cwd.relative_to(ROOT)), 'argv': list(map(str, args))}, replacements))
        run(*args, cwd=cwd, env=env)

    a.out.mkdir(parents=True)
    user = a.out / 'package'
    debug = a.out / 'debug'
    user.mkdir()
    debug.mkdir()
    if not a.skip_checks:
        execute(sys.executable, ROOT/'tools/test-release.py')
        execute('cargo', 'test', '--locked', '--workspace')
        execute('cargo', 'clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings')
    for app, bin_name in firmware_build.APPS:
        map_path = map_dir / (bin_name + '.map')
        app_env = firmware_build.with_map(env, map_path)
        elf = firmware_build.build_app(execute, app_env, app, user / (bin_name + '.bin'), target=TARGET)
        if not a.skip_checks:
            execute('cargo', 'clippy', '--locked', '--release', '--', '-D', 'warnings', cwd=ROOT/'apps'/app, env=app_env)
        validate_elf(elf, *(layout()[0] if bin_name == 'loader' else layout()[1]))
        copy(elf, debug / (bin_name + '.elf'))
        copy(map_path, debug / (bin_name + '.map'))
    if not a.skip_checks:
        # The Bluetooth build is not packaged, but stays clean.
        execute('cargo', 'clippy', '--locked', '--release', '--features', 'ble', '--', '-D', 'warnings',
                cwd=ROOT/'apps'/'standalone-boot', env=env)
    first = user / 'first-install/Apps/ImageInstaller'
    first.mkdir(parents=True)
    firmware_build.combine(execute, env, user / 'loader.bin', user / 'app.bin', first / 'image.bin')
    crc = validate_image((user/'loader.bin').read_bytes(), (user/'app.bin').read_bytes(), (first/'image.bin').read_bytes())
    (user/'INSTALL-CRC.txt').write_text(crc + '\n')
    appdir = ROOT / 'apps/image-installer/App'
    uapp = firmware_build.build_installer(execute, env, a.sdk, a.version, CONFIG['nightly'], a.out / 'installer-build')
    abi = validate_uapp(uapp.read_bytes(), a.version)
    copy(uapp, first/uapp.name)
    for process in ['Service', 'GUI']:
        copy(appdir/f'build/ImageInstaller{process}.elf', debug/f'ImageInstaller{process}.elf')
        copy(appdir.parent/f'Output/ImageInstaller{process}.elf.elf.map', debug/f'ImageInstaller{process}.map')
    # Keep build paths out of the published maps.
    for map_file in debug.glob('*.map'):
        map_file.write_text(normalize_paths(map_file.read_text(), replacements))
    require((debug/'ImageInstallerService.elf').exists(), 'missing installer ELF')
    dis, commit_functions = audit_commit(debug/'ImageInstallerService.elf')
    audit_gui(debug/'ImageInstallerGUI.elf')
    (debug/'installer-disassembly.txt').write_text(normalize_paths(dis, replacements))
    for name in PACKAGED_TOOLS:
        copy(ROOT/'tools'/name, user/'tools'/name)
    for src, dst in PACKAGED_DOCS.items():
        (user/dst).write_text(package_links((ROOT/'release'/src).read_text(), commit))
    for image in packaged_images((user/dst).read_text() for dst in PACKAGED_DOCS.values()):
        require((ROOT/'release'/image).is_file(), 'missing image: release/' + image)
        copy(ROOT/'release'/image, user/image)
    copy(ROOT/'LICENSE', user/'LICENSE')
    licenses = user/'licenses'
    for label, cwd in [('root', ROOT), ('app', ROOT/'apps/standalone-boot'), ('loader', ROOT/'apps/wrist-loader')]:
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version=1'], cwd=cwd, env=env))
        normalized_metadata = normalize_paths(metadata, replacements)
        (debug / (label + '-dependencies.json')).write_text(json.dumps(normalized_metadata, indent=2) + '\n')
        copy(cwd/'Cargo.lock', debug/label/'Cargo.lock')
        for pkg in metadata['packages']:
            base = Path(pkg['manifest_path']).parent
            for p in package_licenses(base):
                copy(base/p, licenses/label/(pkg['name']+'-'+pkg['version'])/p)
    docs = [Path(output('rustc', '+'+compiler, '--print', 'sysroot'))/'share/doc/rust'
            for compiler in [CONFIG['stable'], CONFIG['nightly']]]
    collect_licenses(licenses, a.sdk, [doc/'licenses' for doc in docs], shutil.which('arm-none-eabi-gcc'))
    for compiler, doc in zip([CONFIG['stable'], CONFIG['nightly']], docs):
        for name in ['COPYRIGHT.html', 'COPYRIGHT-library.html']:
            if (doc/name).is_file():
                copy(doc/name, licenses/'rust-toolchain'/compiler/name)
        if (doc/'licenses').is_dir():
            for p in (doc/'licenses').rglob('*'):
                if p.is_file():
                    copy(p, licenses/'rust-toolchain'/compiler/'licenses'/p.relative_to(doc/'licenses'))
    require(not list(first.rglob('ARM_INSTALL')), 'arm file in package')
    gate = json.loads((ROOT/'release/gate.json').read_text())
    fingerprint = round_trip_fingerprint(uapp.read_bytes(), (user/'loader.bin').read_bytes())
    blockers = gate_open(gate, crc, fingerprint)
    for reason in blockers:
        print('Not releasable:', reason)
    require(not release or not blockers, f'v{a.version} is not the gated build: ' + '; '.join(blockers))
    manifest = {'schema': 1, 'version': a.version, 'build_id': build_id, 'source_commit': commit, 'tags': tags, 'dirty': dirty,
                'release_ready': not blockers and not dirty and not a.skip_checks, 'release_gate': gate, 'configuration': 'default features; UNAcore + LS012 + original modules',
                'toolchain': CONFIG, 'features': {'standalone-boot': [], 'wrist-loader': [], 'wrist-flash': ['panic-handler'], 'wrist-installer-ui': ['device']}, 'container_id': a.container_id, 'checks_skipped': a.skip_checks,
                'tracked_diff_and_recipe_sha256': hashlib.sha256(subprocess.check_output(['git', 'diff', '--binary', 'HEAD']) + (ROOT/'tools/release-build.py').read_bytes() + (ROOT/'tools/firmware_build.py').read_bytes() + (ROOT/'release/toolchain.json').read_bytes()).hexdigest(),
                'tools': {t: output(*cmd) for t, cmd in {
                    'rustc': ['rustc', '-Vv'], 'nightly': ['rustc', '+'+CONFIG['nightly'], '-Vv'],
                    'cargo': ['cargo', '--version'], 'objcopy': ['arm-none-eabi-objcopy', '--version'],
                    'objdump': ['arm-none-eabi-objdump', '--version'],
                    'gcc': ['arm-none-eabi-gcc', '--version'], 'cmake': ['cmake', '--version'],
                    'python': [sys.executable, '--version']}.items()},
                'commands': commands, 'environment': normalize_paths(
                    {k: env[k] for k in ['RUSTFLAGS', 'SOURCE_DATE_EPOCH', 'TZ']}, replacements),
                'installer': abi, 'ram_commit_functions_checked': commit_functions, 'image_crc32': crc, 'round_trip_fingerprint': fingerprint, 'reproducibility': 'built in toolchain.json build_image' if a.container_id == CONFIG['build_image'] else 'not built in toolchain.json build_image; not expected to match CI',
                'files': {str(p.relative_to(a.out)): {'bytes': p.stat().st_size, 'sha256': sha(p)}
                          for tree in [user, debug] for p in sorted(tree.rglob('*')) if p.is_file()}}
    for tree in [user, debug]:
        (tree/'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        (tree/'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(tree).as_posix()}\n' for p in sorted(tree.rglob('*')) if p.is_file() and p.name != 'SHA256SUMS'))
        zip_path = a.out/f'wrist-rs-{a.version}{"-debug" if tree == debug else ""}.zip'
        archive(tree, zip_path)
        verify_archive(zip_path, debug=tree == debug)
    (a.out/'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.name}\n' for p in sorted(a.out.glob('*.zip'))))
    print('Candidate package:', a.out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as e:
        sys.exit(str(e))
