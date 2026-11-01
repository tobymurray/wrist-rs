# Building

## What you need

- [rustup](https://rustup.rs); [`rust-toolchain.toml`](rust-toolchain.toml) pins the compiler and target.
- An `objcopy`: the `llvm-tools` rustup component, `arm-none-eabi-objcopy`, or
  `cargo objcopy` from `cargo-binutils`.
- Python 3 with [`tools/requirements.txt`](tools/requirements.txt), for the host tools.
- For the stock installer and the release zip: Docker and the UNA SDK, as
  [release/README.md](release/README.md#build-locally) describes.

## The image

```sh
cd apps/standalone-boot
cargo build --release
rustup component add llvm-tools
"$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy -O binary \
    target/thumbv8m.main-none-eabihf/release/standalone-boot app.bin
```

`cargo build --release --features usb-log` also keeps `embassy-usb`'s own log of the link,
which the console's `usblog` prints. `--features ble` adds the console and updates over
Bluetooth ([below](#bluetooth)).

The console's `version` prints the image's version, build ID and CRC:

```text
[usb] standalone-boot 0.3.0+ble.local build 4f1e9a07c3d2 crc 6D81FE53
```

The ID is 12 hex digits hashing the tracked files that go into the binaries, listed in
[release/build-inputs.txt](release/build-inputs.txt), with `-dirty` when one of them has
an uncommitted change, and then hashing the edits too. A commit that touches none of them,
such as a docs change, keeps the ID and the image's bytes. `python3 tools/firmware_build.py`
prints that hash, which is the ID of the release recipe's build.

A build that differs from the recipe's hashes how into its ID, and names it after the
version: each feature beyond the release's (`app_features` in
[release/toolchain.json](release/toolchain.json)), and `local` for any build
outside the recipe, since the binary holds the checkout's and Cargo's paths
([release/README.md](release/README.md#the-recipe) has the rest). So the default, `ble`
and `usb-log` builds have three IDs. The report's `[step] build` line also gives the
source hash, as `<ID> (<hash>+ble.local)`. `WRIST_RELEASE_BUILD_ID=<name> cargo build
--release` names a build with the release's features instead, and is refused with others.

The CRC is the CRC-32 of the image's own bytes in flash, computed once at boot (`crc
pending` for its first moments), and the same that `tools/wrist-update.py` prints when it
sends that app.bin, so it names exactly what the watch runs; `report` and the about page
show it too.
`python3 tools/test-build-id.py` checks all this, building the image several ways in a
clone of HEAD.

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
`target/tmp/wrist-glass/`. `cargo run -p wrist-installer-ui --features preview --example
screens -- release/images` redraws the installer's screens that the install guide shows.

After editing [`l10n/`](l10n), `cargo run -p wrist-l10n-gen` regenerates the messages
and locale tables; `cargo test --workspace` fails until it has been run.

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
[`tools/wrist-crash.py`](tools/wrist-crash.py) decodes the crash record `power` shows, given the failing
build's release zip, `manifest.json` or image. The UNA Dev Tool hears the image's UART as well, under the conditions in
[HARDWARE.md](HARDWARE.md#debug-and-boot).

Two commands test the watch without touching it. `press <up|down|back|select> [long]` hands
the button task a debounced press, as a released button does. It cannot stand in for a hold,
so holds still need a hand, and `power` counts these presses apart from real ones. `page dump`
writes the frame last drawn, one run-length row a line, and
[`tools/wrist-frame.py`](tools/wrist-frame.py) makes it a PNG. To compare it with the host's
render, pass the example what the watch was showing (it prints its usage for the other pages):

```sh
.venv/bin/python tools/wrist-cmd.py "press select"
.venv/bin/python tools/wrist-frame.py face.png --raw-out face.raw
cargo run -p wrist-glass --example face -- 09:11 2026-10-23 100 host.raw
.venv/bin/python tools/wrist-frame.py --raw face.raw --compare host.raw face.png
```

## Bluetooth

A `--features ble` image keeps the radio in reset until `ble on` on the console; `ble off`
puts it back, and the choice survives a reset. The console answers over the air only to a
bonded computer, over an encrypted link, and takes application updates but never the
loader. On battery the radio advertises about once a second and keeps an idle link slow;
`ble profile battery|cable|auto` forces one timing for testing.

From Linux, with the watch also on USB, pairing needs no button: the tool confirms the
watch's number over the USB console.

```sh
.venv/bin/pip install bleak
.venv/bin/python tools/wrist-cmd.py "ble on"
.venv/bin/python tools/wrist-ble.py --pair
.venv/bin/python tools/wrist-ble.py "ble;boot"
.venv/bin/python tools/wrist-ble.py --update app.bin   # waits, over the air, for state boot
```

An update that arrived over the air is kept once USB is configured or an encrypted
Bluetooth link is up. BlueZ keeps a bonded watch's services: after a build that changes
them, `bluetoothctl remove <address>` and pair again.

The watch can also scan for any Bluetooth LE device: `ble scan [s]` lists advertisers by
address, signal, name and services.

[`tools/ble-suite.py`](tools/ble-suite.py) runs the Bluetooth regression scenarios
unattended against a bonded watch on USB, judges each from the console, and writes its log
and `result.json` under `target/ble-suite/`; `--help` lists the scenarios and their options.
[`tools/hr-sim.py`](tools/hr-sim.py) plays a heart-rate strap from a Linux host.

```sh
.venv/bin/python tools/ble-suite.py --image app.bin --soak-minutes 30   # app.bin: the running image
```

## Working on InscribedDisc

The face is drawn with [InscribedDisc](https://crates.io/crates/inscribed-disc), developed
in [tobymurray/watch-apps](https://github.com/tobymurray/watch-apps). To build against a
checkout beside this repository, uncomment the `[patch]` in the root [`Cargo.toml`](Cargo.toml).
