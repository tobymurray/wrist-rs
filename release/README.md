# Release engineering

How a release zip is built. [INSTALL.md](INSTALL.md) becomes the zip's `START-HERE.md`,
[NOTES.md](NOTES.md) its `RELEASE-NOTES.md` and the release's text, and
[GATE.md](GATE.md) is what has to pass before anything is published.

## The recipe

`python3 tools/release-build.py` is the one build, test and package entry point, used by
CI and locally. [`toolchain.json`](toolchain.json) pins the compilers, the UNA SDK and, as
`build_image`, the image every build runs in, by digest; the three
[`Cargo.lock`](../Cargo.lock) files pin every other input.

The version is a clean `vA.B.C[-suffix]` tag on HEAD; other builds are
`0.0.0-candidate.<commit12>`. A dirty tree needs `--allow-dirty` and is never releasable.
The tag's `A.B.C` is the apps' Cargo version, bumped in both [`Cargo.toml`](../Cargo.toml) files before
tagging.

The image carries a build ID, which the console's `version` prints and `manifest.json`
records as `build_id` beside `source_commit`: a hash of the files that go into the
binaries, listed in [`build-inputs.txt`](build-inputs.txt), so that a commit of only the
gate's record, docs, the site or host tools rebuilds the same bytes
([GATE.md](GATE.md#publishing)). With uncommitted changes to those files it is a hash of
the tree as it stands, then `-dirty`. [`tools/firmware_build.py`](../tools/firmware_build.py)
holds the part of the recipe that shapes the binaries, and computes the ID.

That ID is the recipe's build's: the features `app_features` in
[`toolchain.json`](toolchain.json) names, the pinned compiler and target, the
release profile as [`Cargo.toml`](../apps/standalone-boot/Cargo.toml) sets it, and the
recipe's flags. Any other build of the image hashes how it differs into its own ID, in
[`build.rs`](../apps/standalone-boot/build.rs), and names the differences after the
version, as SemVer build metadata: `+` and each feature beyond `app_features`, `no-<feature>`
for each one of them left out, and `local` for a build outside the recipe; `Variant` in
build.rs names the rest. So `cargo build --release --features ble` prints
`standalone-boot 0.3.0+ble.local build <its own ID>`, and the report's `[step] build`
line adds the source hash it came from, `(<firmware hash>+ble.local)`.
`WRIST_RELEASE_BUILD_ID` is for the recipe's features only: build.rs refuses it on any
other build, and the recipe and the gate pass `app_features` as `--features`.
build.rs's target and opt-level are copies, which
[`tools/test-release.py`](../tools/test-release.py) checks against `.cargo/config.toml` and
`[profile.release]`.

`local` stays in the hash because a build's paths change its bytes. Panic messages hold the
source paths, and Cargo hashes each path crate outside the image's workspace, everything
under `crates/` and `vendor/`, into its symbols by its absolute path, which moves code.
`--remap-path-prefix` rewrites the messages but cannot undo the hashing, so two checkouts
of one commit build two images. Only the recipe's image, which mounts every checkout at
`/src`, is the same from anywhere; a native run of the recipe from elsewhere records
`checkout_at_src: false` in `manifest.json` and is never release-ready.

build.rs sees only what Cargo tells a build script, so not everything that changes the
bytes reaches the ID. The image's CRC covers the rest: at boot it hashes its own
bytes in flash, from the vector table to the end of `.data`'s load image, which is exactly
app.bin, and `version`, `report` and the about page show it as `crc XXXXXXXX`, the CRC-32
that [`tools/wrist-update.py`](../tools/wrist-update.py) prints for that app.bin. It names
those bytes only: whatever the slot holds after them is not covered.
`manifest.json`'s `image_crc32` is over image.bin, the loader padded with `FF` to 32 KiB
and then app.bin, so it is app.bin's CRC continued from the padded loader's:
`zlib.crc32(app, zlib.crc32(loader_padded))`, not the image's own CRC.

## Build locally

With Docker, on Linux or macOS:

```sh
read -r url commit < <(python3 -c 'import json; c = json.load(open("release/toolchain.json")); print(c["sdk_url"], c["sdk_commit"])')
git clone "$url" /tmp/wrist-sdk && git -C /tmp/wrist-sdk checkout "$commit"
python3 tools/release-docker.py --sdk /tmp/wrist-sdk --out /tmp/wrist-stage/candidate \
  --version 0.0.0-local --allow-dirty
```

[`tools/release-docker.py`](../tools/release-docker.py) runs the recipe in `build_image`
as your user, the same way the [firmware workflow](../.github/workflows/firmware.yml) does,
and passes the arguments it does not know on to the recipe. Use a new output directory each time.
The image is `linux/amd64`; on Apple silicon Docker runs it under emulation. To rebuild a
CI candidate, check out its commit cleanly and pass its version,
`0.0.0-candidate.<commit12>`.

`--local-image` builds [`Dockerfile`](Dockerfile) as `wrist-rs-build:local` and uses that
instead, for working on the image. Its builds are not expected to match CI's, and their
manifest says so. Natively, with the same rustup toolchains, the GNU Arm tools, CMake 3.21
or newer, make, Python 3.10 or newer, `pyelftools` and Pillow, the binaries differ from CI's too:

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

In the same build image, a clean build of a commit is meant to rebuild byte for byte, from
any checkout path: the recipe fixes the paths, time and timezone the binaries embed
(`environment` in [`tools/firmware_build.py`](../tools/firmware_build.py)), and the zips
their timestamps, modes and order.

The image is pinned by digest because building [`Dockerfile`](Dockerfile) again gives a new
image, however closely its pins narrow it.

A build also fetches what the image does not hold: the crates in the lock files, the
Embassy commit in [`apps/standalone-boot/Cargo.lock`](../apps/standalone-boot/Cargo.lock),
and the UNA SDK at `sdk_commit`.

## Moving the image forward

The image changes only by a reviewed commit:

1. A branch changes [`Dockerfile`](Dockerfile): a toolchain, a package, or a
   [Dependabot](../.github/dependabot.yml) bump of the base.
2. The [build image workflow](../.github/workflows/toolchain.yml) publishes it to
   `ghcr.io/tobymurray/wrist-rs-build`, tagged with the commit, on a push that changes it.
   It skips Dependabot's pushes, which cannot write packages, so on its branch, run it by hand.
   The run's summary prints the digest.
3. That branch sets `build_image` in [`toolchain.json`](toolchain.json) to the digest.
   Until then, `tools/release-docker.py` refuses to build.
4. `toolchain.json` is a build-ID input ([`build-inputs.txt`](build-inputs.txt)), so the
   build ID changes with the pin, and the [gate](GATE.md) runs again on the new candidate.

No image version that a tag's `toolchain.json` names is ever deleted from GHCR, and the
package stays public, so every release rebuilds.

## Third-party inputs

Two inputs live in other accounts, and each has a way to keep it:

- **The base image**, `xanderhendriks/stm32cubeide` on Docker Hub. The build image
  workflow, run by hand with `mirror_base`, copies the base named in `FROM` to
  `ghcr.io/tobymurray/stm32cubeide`, digest unchanged, so a `FROM` that names the mirror
  with the same digest builds the same image. Dependabot then watches the mirror, which
  changes only when copied to: a base bump puts upstream's new tag and digest in `FROM`
  and runs the copy on that branch before the image is published.
- **The UNA SDK** at `sdk_commit`, from `sdk_url`. A fork under `tobymurray` keeps that
  commit fetchable; CI checks out `sdk_url`, so pointing it at the fork moves every build.

To switch to both:

1. Run the build image workflow with `mirror_base`.
2. In the package settings of `stm32cubeide` and `wrist-rs-build`, make both public and
   link them to this repository.
3. Fork `UNAWatch/una-sdk` to `tobymurray/una-sdk`, with all branches, and check that
   `sdk_commit` and the `sdk_tag` tag are in it.
4. On one branch, change `FROM` in [`Dockerfile`](Dockerfile) to
   `ghcr.io/tobymurray/stm32cubeide:<tag>@<the same digest>`, set `sdk_url` in
   [`toolchain.json`](toolchain.json) to `https://github.com/tobymurray/una-sdk`, and
   pin the image that branch publishes, as above. The new pin and `sdk_url` are both in
   `toolchain.json`, a build-ID input, so one gate covers the change.

## GitHub

Releasing follows [GATE.md](GATE.md). A release is never replaced: a corrected build needs
a new version. Verify a download with `gh attestation verify wrist-rs-VERSION.zip --repo
tobymurray/wrist-rs`.
