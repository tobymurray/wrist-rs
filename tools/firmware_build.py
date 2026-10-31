#!/usr/bin/env python3
"""How the release recipe compiles the binaries, and their build ID; run alone, it prints the ID.

Everything here shapes app.bin, loader.bin, image.bin or the installer, so this file is one
of the build ID's inputs (release/build-inputs.txt); tools/release-build.py checks and
packages around it.
"""
import json
import os
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


def app_features(root=ROOT):
    """The image's features in the recipe's build, `app_features` in release/toolchain.json,
    which apps/standalone-boot/build.rs also reads."""
    return sorted(json.loads((root/'release/toolchain.json').read_text())['app_features'])


def feature_args(app, root=ROOT):
    """Cargo's arguments for the recipe's features of `app`, one of APPS."""
    features = app_features(root) if app == 'standalone-boot' else []
    return ['--features', ','.join(features)] if features else []


def build_id(root=ROOT):
    """The firmware hash, the ID of the recipe's build. apps/standalone-boot/build.rs computes the
    same, and for any other build hashes how it differs from the recipe's into its own ID."""
    specs = inputs(root)
    listing = git('ls-files', '-s', '-z', '--', *specs, root=root)
    dirty = git('status', '--porcelain', '--untracked-files=normal', '--', *specs, root=root)
    if dirty:
        listing += worktree(root, specs)
    digest = git('hash-object', '--stdin', root=root, stdin=listing).decode().strip()
    return digest[:12] + ('-dirty' if dirty else '')


def worktree(root, specs):
    """After `worktree\\0`, each input that differs from the index as `<blob> <path>\\0`, with `-`
    for a deleted one's blob, so that two uncommitted trees that differ have different IDs."""
    listed = git('ls-files', '-z', '--modified', '--deleted', '--others', '--exclude-standard', '--', *specs, root=root)
    paths = sorted({p for p in listed.split(b'\0') if p})
    present = [p for p in paths if (root/os.fsdecode(p)).is_file()]
    blobs = iter(git('hash-object', '--stdin-paths', root=root, stdin=b'\n'.join(present)).split(b'\n'))
    return b'worktree\0' + b''.join((next(blobs) if p in present else b'-') + b' ' + p + b'\0' for p in paths)


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
    # rustc applies the last prefix that matches: the checkout is /src, inside a home or not.
    prefixes = [(Path.home(), '/builder')] + [(env[name], '/' + name.lower()) for name in
                                              ['CARGO_HOME', 'RUSTUP_HOME'] if env.get(name)]
    env['RUSTFLAGS'] = ' '.join(f'--remap-path-prefix={path}={token}' for path, token in [*prefixes, (root, '/src')])
    env['TZ'] = 'UTC'
    return env


def with_map(env, map_path):
    """`env` with the linker writing its map to `map_path`. RUSTFLAGS enter Cargo's
    fingerprints, so the path is the same on every build."""
    return dict(env, RUSTFLAGS=env['RUSTFLAGS'] + f' -C link-arg=-Map={map_path}')


def build_app(execute, env, app, binary, root=ROOT, target='thumbv8m.main-none-eabihf'):
    """Compile one of APPS and copy out its raw binary; returns its ELF."""
    cwd = root/'apps'/app
    execute('cargo', 'build', '--locked', '--release', *feature_args(app, root), cwd=cwd, env=env)
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
    return build/f'wrist-rs-installer_{version}.uapp'


if __name__ == '__main__':
    try:
        print(build_id(Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT))
    except (OSError, subprocess.CalledProcessError) as e:
        sys.exit(str(e))
