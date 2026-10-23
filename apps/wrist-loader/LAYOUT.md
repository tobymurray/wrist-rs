# Flash layout behind the vendor bootloader

Bank 1. The vendor bootloader's code below the loader is never written, and its
recovery puts the stock kernel back over everything here.

| Region | Address | Size | Holds |
|---|---|---|---|
| loader | `0x08060000` | 32 KiB | `wrist-loader`, which the vendor bootloader jumps to |
| active | `0x08068000` | 512 KiB | `standalone-boot`, the running application |
| dfu | `0x080E8000` | 520 KiB | the next application; one page more than active, for the swap |
| state | `0x0816A000` | 8 KiB | swap and revert state |

Both [`memory.x`](memory.x) files spell out these numbers; embassy-boot takes them as offsets from bank
1's base. They are fixed for the life of the project: moving dfu or state means replacing
the loader, which has no second slot. The installer's pages are bank 1 page 45, blank at the
end of the bootloader's partition, and bank 2 pages 34 and 128 to 195
([`crates/wrist-flash/src/plan.rs`](../../crates/wrist-flash/src/plan.rs), `hw.rs`).

## Bank 2

`0x08200000`–`0x083FFFFF`, 8 KiB pages. Nothing here is part of an update. Page numbers repeat across the banks, and an erase that loses its bank select
erases the same page of bank 1 instead, so each region is placed for what lies opposite it.

| Region | Pages | Address | Holds | Opposite in bank 1 |
|---|---|---|---|---|
| stock spill | 0–23 | `0x08200000` | the stock kernel's last pages, under stock | the vendor bootloader |
| self-test | 34 | `0x08244000` | the installer's erase self-test, under stock only | bootloader code: `hw.rs` reads the bank select back first |
| reserve | 48–115 | `0x08260000` | unused | the loader and active |
| settings | 116–127 | `0x082E8000` | settings, written only after an image has kept itself | the start of dfu |
| staging | 128–195 | `0x08300000` | the installer's staged image; a stale copy stays after an install | dfu and state, under stock |
| glyph store | 196–255 | `0x08388000` | language packs: data, CRC-checked, never executed | unused |

Nothing of ours erases bank 2 below page 48 except the self-test. The numbers are in
[`crates/wrist-flash/src/layout.rs`](../../crates/wrist-flash/src/layout.rs), whose
compile-time checks keep the regions apart and hold the installer's plan to them, and whose
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
