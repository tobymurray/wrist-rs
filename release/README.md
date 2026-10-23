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
([GATE.md](GATE.md#publishing)). [`tools/firmware_build.py`](../tools/firmware_build.py)
holds the part of the recipe that shapes the binaries, and computes the ID.

## Build locally

With Docker, on Linux or macOS:

```sh
read -r url commit < <(python3 -c 'import json; c = json.load(open("release/toolchain.json")); print(c["sdk_url"], c["sdk_commit"])')
git clone "$url" /tmp/wrist-sdk && git -C /tmp/wrist-sdk checkout "$commit"
python3 tools/release-docker.py --sdk /tmp/wrist-sdk --out /tmp/wrist-stage/candidate \
  --version 0.0.0-local --allow-dirty
```

[`tools/release-docker.py`](../tools/release-docker.py) pulls `build_image` and runs the
recipe in it, as the [firmware workflow](../.github/workflows/firmware.yml) does: the
checkout at `/src`, the SDK read-only at `/sdk` and the output's parent at `/stage`, as
your user, with `HOME=/tmp` and a new, empty `CARGO_HOME`. It passes `--version`,
`--allow-dirty` and `--skip-checks` on to the recipe. Use a new output directory each time.
The image is `linux/amd64`; on Apple silicon Docker runs it under emulation. To rebuild a
CI candidate, check out its commit cleanly and pass its version,
`0.0.0-candidate.<commit12>`.

`--local-image` builds [`Dockerfile`](Dockerfile) as `wrist-rs-build:local` and uses that
instead, for working on the image. Its builds are not expected to match CI's, and their
manifest says so. Natively, with the same rustup toolchains, the GNU Arm tools, CMake 3.21
or newer, make, Python 3.10 or newer and `pyelftools`, the binaries differ from CI's too:

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

In the same image, a commit rebuilds byte for byte: two clean builds of one commit, from
different checkout paths, with new target directories and `CARGO_HOME`, one of them on two
CPUs, gave identical `app.bin`, `loader.bin`, `image.bin`, `.uapp`, `INSTALL-CRC.txt` and
zips. What keeps them so:

- The container's paths are fixed, and the recipe remaps the checkout, `CARGO_HOME`,
  `RUSTUP_HOME` and the home directory in what rustc embeds, and the checkout and SDK in
  what GCC's macros embed.
- `SOURCE_DATE_EPOCH` is the last commit to change an input, `TZ=UTC`, the image's locale
  is `C.UTF-8` and the umask is `022`. The zips carry fixed timestamps and modes, in sorted
  order.
- Rust output does not depend on the number of jobs, and `app.bin` and `loader.bin` build
  as one codegen unit each; CMake sorts the installer's sources.

The build image is what makes this hold across hosts and months, and why it is pinned by
digest: [`Dockerfile`](Dockerfile) narrows a rebuild (a base digest, an Ubuntu snapshot,
rustup by checksum, exact Rust toolchains), but a rebuild is a new image, not this one.
Not demonstrated: a build on macOS, and a local build against a CI run's.

A build also fetches what the image does not hold: the crates in the lock files, the
Embassy commit in [`apps/standalone-boot/Cargo.lock`](../apps/standalone-boot/Cargo.lock),
and the UNA SDK at `sdk_commit`.

## Moving the image forward

The image changes only by a reviewed commit:

1. A branch changes [`Dockerfile`](Dockerfile): a toolchain, a package, or a
   [Dependabot](../.github/dependabot.yml) bump of the base.
2. The [build image workflow](../.github/workflows/toolchain.yml) publishes it to
   `ghcr.io/tobymurray/wrist-rs-build`, tagged with the commit, on a push that changes it.
   Dependabot's runs cannot write packages, so on its branch, run the workflow by hand.
   The run's summary prints the digest.
3. That branch sets `build_image` in [`toolchain.json`](toolchain.json) to the digest.
   Until then, its builds stop: the recipe runs only in an image labelled with the hash of
   the `Dockerfile` beside it.
4. Both files are build-ID inputs ([`build-inputs.txt`](build-inputs.txt)), so the build ID
   and the image change, and the [gate](GATE.md) runs again on the new candidate.

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
   pin the image that branch publishes, as above. Both are build-ID inputs, so one gate
   covers the change.

## GitHub

Releasing follows [GATE.md](GATE.md). A release is never replaced: a corrected build needs
a new version. Verify a download with `gh attestation verify wrist-rs-VERSION.zip --repo
tobymurray/wrist-rs`.
