# ImageInstaller

A Glance app for the stock firmware that replaces the vendor kernel with wrist-rs's
loader and application, the image [`tools/combine-image.py`](../../tools/combine-image.py) makes. It is the only
destructive thing in this repository. [release/README.md](../../release/README.md)
builds it, and [release/INSTALL.md](../../release/INSTALL.md) is how to use it.

## The two files

Both go in this app's folder on the watch, beside the `.uapp`.

| File | Effect |
| --- | --- |
| `image.bin` | The image to install. Alone, it gets a dry run: the image is checked and planned, `install.log` records it, and nothing is replaced, though a flash self-test erases and programs a scratch page in bank 2. |
| `ARM_INSTALL` | The CRC-32 of `image.bin` as eight hex digits, optionally prefixed `0x`. The app installs only when it names the image present, and refuses anything else. |

Arming is a file, not a tap on the tile, because every glance service starts when the
carousel does ([`HARDWARE.md`](../../HARDWARE.md), "Boot handoff"). The file names the image by its CRC so a
leftover arm file never installs whatever `image.bin` has since become, and the app
removes it before committing.

## What the tile says

| Tile | Meaning |
| --- | --- |
| `refused` | The firmware isolates apps; nothing was attempted. |
| `no image` | `image.bin` is missing, empty or unreadable. |
| `ready XXXXXXXX` | A dry run of the image with that CRC-32. |
| `arm mismatch` | `ARM_INSTALL` names a different image. |
| `arm unreadable` | `ARM_INSTALL` is not exactly one CRC. |
| `unlock N` | The flash unlock self-test failed. |
| `arm stuck` | `ARM_INSTALL` could not be removed. |
| `refused -N` | A check refused the image: `plan::Refusal` in [`crates/wrist-flash`](../../crates/wrist-flash), or the `kErr` codes in [`Libs/Header/Service.hpp`](Libs/Header/Service.hpp). Staging may have written bank 2. |
| (the watch restarts) | It installed. |

None of the others has touched the stock firmware.

## How it stays safe

- The image is staged in bank 2 and checked against the arm file's CRC while the
  kernel's filesystem, which the commit erases, still exists.
- The commit copies flash to flash from code already in RAM, with interrupts masked; the
  release recipe refuses a build whose commit path branches outside RAM.
- The kernel write starts at page 48 or not at all (`plan::Refusal::WouldEraseBootloader`).
- Every wait is bounded, and every erase and program is read back.

`install.log` and `stage.log` stay on the watch's storage. The outcome record a commit
leaves in the bootloader partition's tail is defined in [`crates/wrist-flash/src/hw.rs`](../../crates/wrist-flash/src/hw.rs),
and the next dry run reports it. UNA's recovery leaves it in place, so a dry run after a
return to stock reports the last install.
