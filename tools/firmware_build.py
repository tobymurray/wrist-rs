#!/usr/bin/env python3
"""How the release recipe compiles the binaries, and their build ID; run alone, it prints the ID.

Everything here shapes app.bin, loader.bin, image.bin or the installer, so this file is one
of the build ID's inputs (release/build-inputs.txt); tools/release-build.py checks and
packages around it.
"""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
APPS = [('wrist-loader', 'loader'), ('standalone-boot', 'app')]


def git(*args, root=ROOT, stdin=None):
    # No index refresh, which would rewrite the index that build.rs watches.
    return subprocess.run(['git', '--no-optional-locks', *args], cwd=root, input=stdin, capture_output=True, check=True).stdout


def inputs(root=ROOT):
    """The pathspecs of release/build-inputs.txt."""
    lines = (root/'release/build-inputs.txt').read_text().splitlines()
    return [line.strip() for line in lines if line.strip() and not line.startswith('#')]


def build_id(root=ROOT):
    """The firmware hash; apps/standalone-boot/build.rs computes the same for builds outside the recipe."""
    specs = inputs(root)
    listing = git('ls-files', '-s', '-z', '--', *specs, root=root)
    digest = git('hash-object', '--stdin', root=root, stdin=listing).decode().strip()
    dirty = git('status', '--porcelain', '--untracked-files=normal', '--', *specs, root=root)
    return digest[:12] + ('-dirty' if dirty else '')


def environment(base, sdk, build_id, root=ROOT):
    """The build's environment: no caller's target directory, wrapper or flags, and no
    builder's paths in the binaries."""
    env = dict(base)
    for name in ['CARGO_TARGET_DIR', 'RUSTC_WRAPPER', 'CARGO_ENCODED_RUSTFLAGS']:
        env.pop(name, None)
    env['UNA_SDK'] = str(sdk)
    # The last commit to change an input, so that a later commit of anything else changes no byte.
    env['SOURCE_DATE_EPOCH'] = git('log', '-1', '--format=%ct', 'HEAD', '--', *inputs(root), root=root).decode().strip()
    env['WRIST_RELEASE_BUILD_ID'] = build_id
    env['RUSTFLAGS'] = f'--remap-path-prefix={root}=/src --remap-path-prefix={Path.home()}=/builder'
    env['TZ'] = 'UTC'
    return env


def with_map(env, map_path):
    """`env` with the linker writing its map to `map_path`. RUSTFLAGS enter Cargo's
    fingerprints, so the path is the same on every build."""
    return dict(env, RUSTFLAGS=env['RUSTFLAGS'] + f' -C link-arg=-Map={map_path}')


def build_app(execute, env, app, binary, root=ROOT, target='thumbv8m.main-none-eabihf'):
    """Compile one of APPS and copy out its raw binary; returns its ELF."""
    cwd = root/'apps'/app
    execute('cargo', 'build', '--locked', '--release', cwd=cwd, env=env)
    elf = cwd/'target'/target/'release'/app
    execute('arm-none-eabi-objcopy', '-O', 'binary', elf, binary, env=env)
    return elf


def combine(execute, env, loader, app, image, root=ROOT):
    execute(sys.executable, root/'tools/combine-image.py', loader, app, image, env=env)


def build_installer(execute, env, sdk, version, nightly, build, root=ROOT):
    """Configure and build the installer's .uapp in `build`; returns its path."""
    appdir = root/'apps/image-installer/App'
    (appdir.parent/'Output').mkdir(exist_ok=True)
    flags = f'-fmacro-prefix-map={root}=/src -fmacro-prefix-map={sdk}=/sdk'
    execute('cmake', '-S', appdir, '-B', build, '-G', 'Unix Makefiles',
            f'-DBUILD_VERSION={version}', f'-DWRIST_NIGHTLY={nightly}',
            f'-DCMAKE_C_FLAGS={flags}', f'-DCMAKE_CXX_FLAGS={flags}',
            f'-DWRIST_RUSTFLAGS=-C relocation-model=pic {env["RUSTFLAGS"]}', env=env)
    execute('cmake', '--build', build, '--parallel', '2', env=env)
    return build/f'Install_{version}.uapp'


if __name__ == '__main__':
    try:
        print(build_id(Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT))
    except (OSError, subprocess.CalledProcessError) as e:
        sys.exit(str(e))
