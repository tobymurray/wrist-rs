# Flash layout behind the vendor bootloader

Bank 1. The vendor bootloader's recovery puts the stock kernel back over everything from the
loader up, and nothing below it.

| Region | Address | Size | Holds |
|---|---|---|---|
| bootloader | `0x08000000` | 160 KiB | UNA's bootloader; never written |
| library | `0x08028000` | 224 KiB | UNA's shared C library, which the stock kernel calls into; never written but for the installer's record. An `.ota` does not restore it |
| loader | `0x08060000` | 32 KiB | `wrist-loader`, which the vendor bootloader jumps to |
| active | `0x08068000` | 512 KiB | `standalone-boot`, the running application |
| dfu | `0x080E8000` | 520 KiB | the next application; one page more than active, for the swap |
| state | `0x0816A000` | 8 KiB | swap and revert state |

Both [`memory.x`](memory.x) files spell out these numbers; embassy-boot takes them as offsets from bank
1's base. They are fixed for the life of the project: moving dfu or state means replacing
the loader, which has no second slot. The installer's pages are bank 1 page 45, blank in the
library's partition between its code and its info block, and bank 2 pages 46 and 128 to 195
([`crates/wrist-flash/src/plan.rs`](../../crates/wrist-flash/src/plan.rs), `commit.rs`, `hw.rs`).

## Bank 2

`0x08200000`–`0x083FFFFF`, 8 KiB pages. Nothing here is part of an update. Page numbers repeat across the banks, and an erase that loses its bank select
erases the same page of bank 1 instead, so each region is placed for what bank 1 holds there
while the region is written.

| Region | Pages | Address | Holds | Opposite in bank 1 |
|---|---|---|---|---|
| stock spill | 0–23 | `0x08200000` | the stock kernel's last pages, under stock | the vendor bootloader, then the library |
| unerased | 24–45, 47 | `0x08230000` | nothing; never erased by wrist-rs | the library |
| self-test | 46 | `0x0825C000` | the installer's erase self-test, under stock only | a page the library leaves blank, which wrist-rs does not use |
| reserve | 48–115 | `0x08260000` | unused | the loader and active |
| settings | 116–127 | `0x082E8000` | settings, written only once the running image has kept itself and no update is received or waiting to swap | the start of dfu, then holding nothing needed |
| staging | 128–195 | `0x08300000` | the installer's staged image, under stock; a stale copy stays after an install | the running stock kernel, which the way back to stock restores |
| glyph store | 196–255 | `0x08388000` | language packs: data, CRC-checked, never executed | unused |

The bank 2 map is fixed for the life of the project. Nothing records where the glyph store
is and nothing migrates it: the store is a cache, and each pack written outranks every pack
already there. Bank 1 pages 196–255, opposite the store, stay unused.

Nothing of ours erases bank 2 below page 48 except the self-test, which `hw.rs` also arms only
after reading the bank select back. The numbers are in
[`crates/wrist-flash/src/layout.rs`](../../crates/wrist-flash/src/layout.rs), whose
compile-time checks tile the bank with the regions and hold the installer's plan to them, and whose
tests hold both `memory.x` files to bank 1's. `flash survey` on the console reports which
pages hold anything.

## Flows

**First install**: [`tools/combine-image.py`](../../tools/combine-image.py) pads the loader to its slot and appends the
application, and the stock firmware's ImageInstaller app writes the result over the
kernel. It never reaches dfu or state.

**Update**: `tools/wrist-update.py app.bin` over the USB console. The running application
checks the image in RAM, writes it to dfu and marks the swap; at the next reset the loader
swaps it into active. The new application keeps itself once it runs well, and a reset
before then reverts it.

**Back to stock**: hold Select into the bootloader's USB mode and copy an official `.ota`.

## Replacing the loader

The loader has no second slot. `tools/wrist-update.py --loader loader.bin` sends it like an
application, checked in RAM, then written over the loader's own pages and read back; it
takes effect at the next reset. A reset while those pages are written leaves a half-written
loader, which only the route back to stock recovers.
