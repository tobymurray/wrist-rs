#!/usr/bin/env python3
"""Run tools/release-build.py in release/toolchain.json's build_image, passing on the arguments it does not know."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LOCAL_IMAGE = 'wrist-rs-build:local'
# Set by .github/workflows/toolchain.yml to the sha256 of the release/Dockerfile it built.
DOCKERFILE_LABEL = 'io.github.tobymurray.wrist-rs.dockerfile-sha256'
# Inside the container, whose /tmp is new in every run.
ENVIRONMENT = {'HOME': '/tmp', 'CARGO_HOME': '/tmp/wrist-rs-cargo'}
SCRIPT = ('umask 022; git config --global --add safe.directory /src; '
          'git config --global --add safe.directory /sdk; exec python3 tools/release-build.py "$@"')


def pinned_image(config):
    """toolchain.json's build_image; ValueError unless it is pinned by digest."""
    image = config.get('build_image', '')
    if not re.fullmatch(r'[a-z0-9.-]+(?:/[a-z0-9._-]+)+@sha256:[0-9a-f]{64}', image):
        raise ValueError(f'release/toolchain.json build_image {image!r} is not a published image digest: '
                         'run .github/workflows/toolchain.yml and pin the digest it prints '
                         '(release/README.md, "Moving the image forward"), or use --local-image')
    return image


def sdk_repository(config):
    """The GitHub owner/name of toolchain.json's sdk_url."""
    m = re.fullmatch(r'https://github\.com/([\w.-]+/[\w.-]+?)(?:\.git)?/?', config['sdk_url'])
    if not m:
        raise ValueError(f'release/toolchain.json sdk_url {config["sdk_url"]!r} is not a GitHub repository')
    return m[1]


def command(image, sdk, out, recipe_args, container_id, uid, gid, root=ROOT):
    """The `docker run` that builds into `out`, a new directory: the checkout at /src, the SDK
    read-only at /sdk and `out`'s parent at /stage, as the caller's user so the outputs are theirs."""
    env = [arg for name, value in ENVIRONMENT.items() for arg in ('--env', f'{name}={value}')]
    return ['docker', 'run', '--rm', '--platform', 'linux/amd64', '--user', f'{uid}:{gid}', *env,
            '--workdir', '/src', '--entrypoint', 'bash',
            '-v', f'{root}:/src', '-v', f'{sdk}:/sdk:ro', '-v', f'{out.parent}:/stage',
            image, '-c', SCRIPT, '--',
            '--sdk', '/sdk', '--out', f'/stage/{out.name}', '--container-id', container_id, *recipe_args]


def docker(*args, capture=False):
    print('+ docker', *args, flush=True)
    result = subprocess.run(['docker', *args], check=True, stdout=subprocess.PIPE if capture else None, text=True)
    return result.stdout.strip() if capture else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--sdk', type=Path, help='an SDK checkout at toolchain.json sdk_commit')
    ap.add_argument('--out', type=Path, help='new directory for the candidate')
    ap.add_argument('--local-image', action='store_true',
                    help=f'build release/Dockerfile as {LOCAL_IMAGE} and use it; not reproducible against CI')
    ap.add_argument('--pins', action='store_true',
                    help='print build_image, sdk_repository and sdk_commit as GitHub step outputs')
    a, recipe_args = ap.parse_known_args()
    config = json.loads((ROOT/'release/toolchain.json').read_text())
    if a.pins:
        print(f'build_image={pinned_image(config)}\nsdk_repository={sdk_repository(config)}\n'
              f'sdk_commit={config["sdk_commit"]}')
        return
    if not a.sdk or not a.out:
        ap.error('--sdk and --out are required')
    sdk, out = a.sdk.resolve(), a.out.resolve()
    if not sdk.is_dir():
        raise ValueError(f'{sdk} is not a directory: clone the SDK at toolchain.json sdk_commit')
    if out.exists():
        raise ValueError(f'{out} exists: use a new directory')
    if ROOT in out.parents:
        raise ValueError('--out must be outside the checkout')
    if a.local_image:
        docker('build', '--platform', 'linux/amd64', '-t', LOCAL_IMAGE, '-f', str(ROOT/'release/Dockerfile'),
               str(ROOT/'release'))
        image = LOCAL_IMAGE
        container_id = 'unpinned ' + docker('image', 'inspect', '--format', '{{.Id}}', image, capture=True)
        print('Not the pinned build_image: this candidate is not expected to match CI.', file=sys.stderr)
    else:
        image = container_id = pinned_image(config)
        docker('pull', '--platform', 'linux/amd64', image)
        built = docker('image', 'inspect', '--format', f'{{{{index .Config.Labels "{DOCKERFILE_LABEL}"}}}}', image,
                       capture=True)
        if built != hashlib.sha256((ROOT/'release/Dockerfile').read_bytes()).hexdigest():
            raise ValueError('build_image was not built from this release/Dockerfile: publish it with '
                             'toolchain.yml and pin the new digest, or use --local-image')
    out.parent.mkdir(parents=True, exist_ok=True)
    argv = command(image, sdk, out, recipe_args, container_id, os.getuid(), os.getgid())
    print('+', *argv, flush=True)
    sys.exit(subprocess.run(argv).returncode)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as e:
        sys.exit(str(e))
