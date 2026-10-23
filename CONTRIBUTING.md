# Contributing

`main` is the watch-face product and what it takes to build, install, update and recover
it.

## Rules

- Never modify the vendor bootloader's code. The image writes only its own partitions, and
  the installer only the pages [apps/wrist-loader/LAYOUT.md](apps/wrist-loader/LAYOUT.md)
  gives it.
- Keep the way back: the button-hold reset into the bootloader, Select's hard reset, and
  recovery through `UNA BOOT`. A change to how flash is written repeats the recovery steps
  of [release/GATE.md](release/GATE.md).
- No vendor firmware, decompiled excerpts, OTA signing key or register maps under NDA.
  Public, licensed vendor material goes in with its licence.
- No task holds the executor for more than a few milliseconds; the one exception is in
  [`apps/standalone-boot/src/update.rs`](apps/standalone-boot/src/update.rs).
- Pure logic goes in a host-tested crate under `crates/`.
- Clippy stays at zero warnings.
- Hosts are Linux and macOS, phones Android: no iOS-specific features.

## What counts as working

A build or a host test does not show that something works on the watch. Run a change that
affects the watch through the relevant part of [release/GATE.md](release/GATE.md), say in
the pull request what you ran and what the watch showed, and add any pin, address or
behaviour learned to [HARDWARE.md](HARDWARE.md). Release notes claim only what such runs
show. Instrument new work through the console.

## Checks

[BUILDING.md](BUILDING.md#checks), plus [`tools/test-release.py`](tools/test-release.py) when the release recipe
changes and `actionlint` when a workflow does.

## Layout

| Path | What |
|---|---|
| [`apps/standalone-boot`](apps/standalone-boot) | The firmware |
| [`apps/wrist-loader`](apps/wrist-loader) | The A/B loader in front of it |
| [`apps/image-installer`](apps/image-installer) | The stock-firmware app that installs both the first time |
| `crates/` | Host-tested logic |
| `vendor/` | Embassy's STM32 HAL and USB driver, with our patches |
| `tools/` | Host tools, and the release recipe with its tests |
| `release/` | The release zip's documents, and the gate before a release |
| `site/` | The download page |
