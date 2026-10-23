# Building

## What you need

- [rustup](https://rustup.rs); [`rust-toolchain.toml`](rust-toolchain.toml) pins the compiler and target.
- An `objcopy`: the `llvm-tools` rustup component, `arm-none-eabi-objcopy`, or
  `cargo objcopy` from `cargo-binutils`.
- Python 3 with [`tools/requirements.txt`](tools/requirements.txt), for the host tools.
- For the stock installer and the release zip: Docker and the UNA SDK, as
  [release/README.md](release/README.md) describes.

## The image

```sh
cd apps/standalone-boot
cargo build --release
rustup component add llvm-tools
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy -O binary \
    target/thumbv8m.main-none-eabihf/release/standalone-boot app.bin
```

`cargo build --release --features usb-log` also keeps `embassy-usb`'s own log of the link,
which the console's `usblog` prints.

The console's `version` prints the image's build ID: 12 hex digits hashing the tracked
files that go into the binaries, listed in [release/build-inputs.txt](release/build-inputs.txt),
with `-dirty` when one of them has an uncommitted change. A commit that touches none of
them, such as a docs change, keeps the ID and the image's bytes. `python3
tools/firmware_build.py` prints the ID of the checkout, and `WRIST_RELEASE_BUILD_ID=<name>
cargo build --release` names a build instead.

## The loader

```sh
cd apps/wrist-loader
cargo build --release
<objcopy> -O binary target/thumbv8m.main-none-eabihf/release/wrist-loader loader.bin
```

[apps/wrist-loader/LAYOUT.md](apps/wrist-loader/LAYOUT.md) has the flash map and the
update flows. The loader has no second slot, so replace it only when release notes ask,
with the stock firmware file at hand.

## Checks

Each tree is its own workspace; Clippy stays at zero warnings.

```sh
# Repository root
cargo test --workspace
cargo clippy --workspace --all-targets

# apps/standalone-boot, then apps/wrist-loader
cargo build --release
cargo clippy --release
```

`python3 tools/test-release.py`, which needs `pyelftools`, tests the release recipe, and
`cargo test -p wrist-glass --features preview` renders the face to PNGs under
`target/tmp/wrist-glass/`.

[`tools/extract_pinmap.py`](tools/extract_pinmap.py), which needs `pdftotext`, regenerates
[`crates/wrist-board/src/pins.rs`](crates/wrist-board/src/pins.rs) from the UNAcore schematic.

## On the watch

The image is a serial console on USB: `/dev/ttyACM*` on Linux, where
[`tools/60-wrist-rs.rules`](tools/60-wrist-rs.rules) lets your user open it, and `/dev/cu.usbmodem*` on macOS.

```sh
python3 -m venv .venv && .venv/bin/pip install -r tools/requirements.txt
.venv/bin/python tools/wrist-update.py app.bin        # --loader loader.bin for the loader
.venv/bin/python tools/wrist-time.py
.venv/bin/python tools/wrist-cmd.py "boot;version"    # "help" lists the commands
```

Stop anything else holding the port first. After an update, wait until `boot` says
`state boot` before resetting, or the previous version comes back.
[`tools/wrist-crash.py`](tools/wrist-crash.py) decodes the crash record `power` shows, given the exact failing
image. The UNA Dev Tool hears the image's UART as well, under the conditions in
[HARDWARE.md](HARDWARE.md#debug-and-boot).

## Working on InscribedDisc

The face is drawn with [InscribedDisc](https://crates.io/crates/inscribed-disc), developed
in [tobymurray/watch-apps](https://github.com/tobymurray/watch-apps). To build against a
checkout beside this repository, uncomment the `[patch]` in the root [`Cargo.toml`](Cargo.toml).
