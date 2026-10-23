# Release engineering

How a release zip is built. [INSTALL.md](INSTALL.md) becomes the zip's `START-HERE.md`,
[NOTES.md](NOTES.md) its `RELEASE-NOTES.md` and the release's text, and
[GATE.md](GATE.md) is what has to pass before anything is published.

## The recipe

`python3 tools/release-build.py` is the one build, test and package entry point, used by
CI and locally. The compilers and the UNA SDK are pinned in [`toolchain.json`](toolchain.json) and
`Dockerfile`, and every other input by the three [`Cargo.lock`](../Cargo.lock) files.

The version is a clean `vA.B.C[-suffix]` tag on HEAD; other builds are
`0.0.0-candidate.<commit12>`. A dirty tree needs `--allow-dirty` and is never releasable.
The tag's `A.B.C` is the apps' Cargo version, bumped in both [`Cargo.toml`](../Cargo.toml) files before
tagging.

The image carries a build ID, which the console's `version` prints and `manifest.json`
records as `build_id` beside `source_commit`: a hash of the files that go into the
binaries, listed in [`build-inputs.txt`](build-inputs.txt), so that a commit of only the
gate's record, docs, the site or host tools rebuilds the same bytes
([GATE.md](GATE.md#publishing)). [`tools/firmware_build.py`](../tools/firmware_build.py)
holds the part of the recipe that shapes the binaries, and computes the ID.

## Build locally

With Docker, on Linux. On Apple silicon, `export DOCKER_DEFAULT_PLATFORM=linux/amd64`
first.

```sh
git clone https://github.com/UNAWatch/una-sdk /tmp/wrist-sdk
git -C /tmp/wrist-sdk checkout "$(python3 -c 'import json; print(json.load(open("release/toolchain.json"))["sdk_commit"])')"
docker build -t wrist-rs-build -f release/Dockerfile .
mkdir -p /tmp/wrist-stage
image=$(docker image inspect wrist-rs-build --format '{{.Id}}')
docker run --rm --entrypoint bash \
  -v "$PWD:/src" -v /tmp/wrist-sdk:/sdk:ro -v /tmp/wrist-stage:/stage \
  wrist-rs-build -c 'git config --global --add safe.directory /src; git config --global --add safe.directory /sdk; exec python3 tools/release-build.py "$@"' \
  -- --sdk /sdk --version 0.0.0-local --allow-dirty \
  --out /stage/candidate --container-id "$image"
```

Use a fresh output directory each time. On Linux, Docker writes its outputs as root, so a
disposable checkout keeps a working tree's ownership intact. Natively, with the same
rustup toolchains, the GNU Arm tools, CMake 3.21 or newer, make, Python 3.10 or newer and
`pyelftools`:

```sh
python3 tools/release-build.py --sdk /tmp/wrist-sdk --version 0.0.0-local \
  --allow-dirty --out /tmp/wrist-candidate
python3 tools/test-release.py
```

`--skip-checks` is for local iteration only.

## What it produces

`wrist-rs-VERSION.zip` is what a user downloads, and `wrist-rs-VERSION-debug.zip` keeps the
ELFs, maps and dependency metadata for diagnosing that build; `verify_archive` in
[`tools/release-build.py`](../tools/release-build.py) checks what each holds. The recipe's checks on sizes, addresses,
the installer's header and its RAM-only commit path are static guards, not proof of what
the hardware does. Checksums catch corruption; an attestation shows which workflow and
commit built a zip, not that it is safe.

## Reproducibility

Builds are traceable, not reproducible: the compiler image is built on each runner from a
pinned base, but its apt packages and rustup bootstrap are not frozen.

## GitHub

Releasing follows [GATE.md](GATE.md). A release is never replaced: a corrected build needs
a new version. Verify a download with `gh attestation verify wrist-rs-VERSION.zip --repo
tobymurray/wrist-rs`.
