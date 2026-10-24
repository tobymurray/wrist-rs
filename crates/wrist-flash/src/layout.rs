//! Every flash page's owner, once, with the rules between them checked at compile time.
//!
//! Pages are 8 KiB and numbered within their bank, 0 to 255. Page numbers repeat across the
//! banks, and an erase whose bank select is lost erases the same page of the other bank and
//! still reports success (RM0456 7.3.6). So the rule that matters is the **mirror rule**:
//! what lies opposite a bank 2 page that is erased. `apps/wrist-loader/LAYOUT.md` has the
//! map in prose, and a host test holds both `memory.x` files to the bank 1 numbers here.

use core::ops::Range;

pub const PAGES_PER_BANK: u16 = 256;

/// Bank 1, `0x08000000`. The vendor bootloader below page 48 is never written by wrist-rs;
/// its recovery erases from page 48 to the end of bank 2.
pub mod bank1 {
    use core::ops::Range;

    /// The vendor bootloader's partition: its code, and blank pages it leaves.
    pub const BOOTLOADER: Range<u16> = 0..48;
    /// The installer's outcome record, blank at the partition's end and outside the
    /// recovery's erase.
    pub const RECORD_PAGE: u16 = 45;
    pub const LOADER: Range<u16> = 48..52;
    pub const ACTIVE: Range<u16> = 52..116;
    /// One page more than active, for the swap.
    pub const DFU: Range<u16> = 116..181;
    /// embassy-boot's swap and revert state.
    pub const STATE: u16 = 181;
    /// Past the state page: never written by wrist-rs, so it holds the stock kernel's tail.
    /// Pages 196 to 255 stay unused for good: they are opposite the glyph store.
    pub const UNUSED: Range<u16> = 182..256;
}

/// Bank 2, `0x08200000`. Nothing here is part of an update, and the recovery erases all of it.
pub mod bank2 {
    use core::ops::Range;

    /// The stock kernel's last pages under stock; a generous bound.
    pub const STOCK_SPILL: Range<u16> = 0..24;
    /// The installer's erase self-test, under stock only. The one bank 2 page below 48 that
    /// anything erases: opposite it is bootloader code, so `hw.rs` reads the bank select back
    /// before it starts the erase.
    pub const SELF_TEST: u16 = 34;
    /// Reserved, not enabled: a large coverage pack or installed descriptions. Opposite it
    /// are the loader and the active slot, so a lost bank select there breaks the image but
    /// never the bootloader.
    pub const RESERVE: Range<u16> = 48..116;
    /// Settings (D3), written only after an image has kept itself and never during an
    /// update. Opposite it is the DFU slot's start, which holds nothing needed then.
    pub const SETTINGS: Range<u16> = 116..128;
    /// The installer's staged image, under stock; a stale copy stays after an install.
    pub const STAGING: Range<u16> = 128..196;
    /// Language packs: data, CRC-checked, never executed. Opposite it is unused bank 1.
    /// Fixed for the life of the project; nothing records where it is.
    pub const GLYPH_STORE: Range<u16> = 196..256;
}

const fn len(r: &Range<u16>) -> u16 {
    r.end - r.start
}

const fn disjoint(a: &Range<u16>, b: &Range<u16>) -> bool {
    a.end <= b.start || b.end <= a.start
}

const fn within(inner: &Range<u16>, outer: &Range<u16>) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

const fn page(p: u16) -> Range<u16> {
    p..p + 1
}

// Bank 1: the partitions are contiguous from the loader to the state page.
const _: () = assert!(bank1::BOOTLOADER.end == bank1::LOADER.start);
const _: () = assert!(bank1::LOADER.end == bank1::ACTIVE.start);
const _: () = assert!(bank1::ACTIVE.end == bank1::DFU.start);
const _: () = assert!(bank1::DFU.end == bank1::STATE);
const _: () = assert!(bank1::STATE + 1 == bank1::UNUSED.start);
const _: () = assert!(bank1::UNUSED.end == PAGES_PER_BANK);
const _: () = assert!(len(&bank1::DFU) == len(&bank1::ACTIVE) + 1);
const _: () = assert!(bank1::RECORD_PAGE < bank1::BOOTLOADER.end);

/// Every bank 2 region, for the disjointness check.
const BANK2: [Range<u16>; 6] = [
    bank2::STOCK_SPILL,
    page(bank2::SELF_TEST),
    bank2::RESERVE,
    bank2::SETTINGS,
    bank2::STAGING,
    bank2::GLYPH_STORE,
];

const fn all_disjoint() -> bool {
    let mut i = 0;
    while i < BANK2.len() {
        let mut j = i + 1;
        while j < BANK2.len() {
            if !disjoint(&BANK2[i], &BANK2[j]) {
                return false;
            }
            j += 1;
        }
        if BANK2[i].end > PAGES_PER_BANK {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = assert!(all_disjoint());
const _: () = assert!(bank2::GLYPH_STORE.start >= bank2::STAGING.end);
const _: () = assert!(bank2::GLYPH_STORE.end == PAGES_PER_BANK);
const _: () = assert!(len(&bank2::STAGING) == len(&bank1::LOADER) + len(&bank1::ACTIVE));

// The installer's plan keeps its own numbers (the release recipe reads `MAX_PAGES` from its
// source), held to these.
const _: () = assert!(crate::plan::FIRST_KERNEL_PAGE as u16 == bank1::LOADER.start);
const _: () = assert!(crate::plan::DFU_FIRST_PAGE as u16 == bank1::DFU.start);
const _: () = assert!(crate::plan::MAX_PAGES as u16 == len(&bank1::LOADER) + len(&bank1::ACTIVE));
const _: () = assert!(crate::plan::STAGING_FIRST_PAGE as u16 == bank2::STAGING.start);
const _: () = assert!(crate::plan::SELF_TEST_PAGE as u16 == bank2::SELF_TEST);
const _: () = assert!(crate::plan::STOCK_KERNEL_BANK2_PAGES as u16 == bank2::STOCK_SPILL.end);

// The mirror rule.
/// Opposite the store: only unused bank 1, past the state page.
const _: () = assert!(within(&bank2::GLYPH_STORE, &bank1::UNUSED));
/// Opposite settings: only the DFU slot.
const _: () = assert!(within(&bank2::SETTINGS, &bank1::DFU));
/// Opposite staging: the DFU slot and the state page's neighbours, never the bootloader.
const _: () = assert!(bank2::STAGING.start >= bank1::BOOTLOADER.end);
/// Opposite the reserve: never the bootloader.
const _: () = assert!(bank2::RESERVE.start >= bank1::BOOTLOADER.end);
/// The self-test is the one exception, and it is a single page.
const _: () = assert!(bank2::SELF_TEST < bank1::BOOTLOADER.end);

/// Whether erasing bank 2 `pages` could, with a lost bank select, erase the vendor
/// bootloader, the loader, the active slot or the state page. True only for the
/// self-test page and the reserve, which the doc comments above account for.
#[must_use]
pub const fn mirror_endangers_boot(pages: &Range<u16>) -> bool {
    !(disjoint(pages, &bank1::BOOTLOADER)
        && disjoint(pages, &bank1::LOADER)
        && disjoint(pages, &bank1::ACTIVE)
        && disjoint(pages, &page(bank1::STATE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_each_bank_2_erase_has_opposite_it() {
        assert!(!mirror_endangers_boot(&bank2::GLYPH_STORE));
        assert!(!mirror_endangers_boot(&bank2::SETTINGS));
        assert!(mirror_endangers_boot(&page(bank2::SELF_TEST)));
        assert!(mirror_endangers_boot(&bank2::RESERVE));
        // Staging's first pages lie opposite the DFU slot, its last opposite the state page
        // and unused bank 1.
        assert!(mirror_endangers_boot(&bank2::STAGING));
        assert!(!mirror_endangers_boot(&(bank2::STAGING.start..bank1::STATE)));
    }

    /// `name : ORIGIN = 0x..., LENGTH = nK` as a page range.
    fn region(memory_x: &str, name: &str) -> Range<u16> {
        let line = memory_x
            .lines()
            .find(|l| l.split(':').next().is_some_and(|n| n.trim() == name))
            .unwrap_or_else(|| panic!("no {name}"));
        let origin = line.split("ORIGIN = 0x").nth(1).unwrap().split(',').next().unwrap();
        let length = line.split("LENGTH = ").nth(1).unwrap().trim().trim_end_matches('K');
        let origin = u32::from_str_radix(origin.trim(), 16).unwrap();
        let length: u32 = length.parse().unwrap();
        assert_eq!(origin & 0x1FFF, 0, "{name} is not page-aligned");
        let first = u16::try_from((origin - plan_base()) / 8192).unwrap();
        first..first + u16::try_from(length * 1024 / 8192).unwrap()
    }

    fn plan_base() -> u32 {
        crate::plan::BANK1_BASE
    }

    #[test]
    fn the_linker_scripts_agree() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/");
        let loader = std::fs::read_to_string(format!("{root}wrist-loader/memory.x")).unwrap();
        let app = std::fs::read_to_string(format!("{root}standalone-boot/memory.x")).unwrap();
        for (text, own, active) in [(&loader, "FLASH", "ACTIVE"), (&app, "LOADER", "FLASH")] {
            assert_eq!(region(text, own), bank1::LOADER);
            assert_eq!(region(text, active), bank1::ACTIVE);
            assert_eq!(region(text, "DFU"), bank1::DFU);
            assert_eq!(region(text, "BOOTLOADER_STATE"), page(bank1::STATE));
        }
    }
}
