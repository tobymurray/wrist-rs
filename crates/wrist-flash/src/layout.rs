//! Every flash page's owner, with the mirror rule checked at compile time: an erase that loses
//! its bank select erases the same page of the other bank (RM0456 7.3.6).

use core::ops::Range;

pub const PAGES_PER_BANK: u16 = 256;

pub mod bank1 {
    use core::ops::Range;

    /// The vendor bootloader; never written.
    pub const BOOTLOADER: Range<u16> = 0..20;
    /// UNA's shared C library (`HARDWARE.md`); wrist-rs writes only [`RECORD_PAGE`] in it.
    pub const LIBRARY: Range<u16> = 20..48;
    /// Blank in the library image UNA publishes (`HARDWARE.md`).
    pub const LIBRARY_BLANK: Range<u16> = 38..47;
    /// The installer's outcome record.
    pub const RECORD_PAGE: u16 = 45;
    pub const LOADER: Range<u16> = 48..52;
    pub const ACTIVE: Range<u16> = 52..116;
    /// One page more than active, for the swap.
    pub const DFU: Range<u16> = 116..181;
    /// embassy-boot's swap and revert state.
    pub const STATE: u16 = 181;
    /// Never written by wrist-rs; the pages opposite the glyph store stay unused for good.
    pub const UNUSED: Range<u16> = 182..256;
}

/// Bank 2: nothing here is part of an update.
pub mod bank2 {
    use core::ops::Range;

    /// The stock kernel's pages in bank 2, as `plan::STOCK_KERNEL_BANK2_PAGES` bounds them.
    pub const STOCK_SPILL: Range<u16> = 0..24;
    /// Never erased by wrist-rs, being opposite the library, either side of the self-test.
    pub const UNERASED_LOW: Range<u16> = 24..46;
    pub const UNERASED_HIGH: Range<u16> = 47..48;
    /// The installer's erase self-test, opposite a blank library page that is not the record,
    /// so a lost bank select erases only blank flash.
    pub const SELF_TEST: u16 = 46;
    /// Unused; a lost bank select here would break the image, never UNA's code.
    pub const RESERVE: Range<u16> = 48..116;
    /// Settings, written only while the DFU slot opposite holds nothing needed: once the
    /// running image has kept itself, and never while an update is received or waits to swap.
    pub const SETTINGS: Range<u16> = 116..128;
    /// The installer's staged image, written under stock, opposite the running stock kernel;
    /// a stale copy stays after an install.
    pub const STAGING: Range<u16> = 128..196;
    /// Language packs, never executed, on pages fixed for good since nothing records where
    /// they are.
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

// Bank 1: the partitions are contiguous from the bootloader to the state page.
const _: () = assert!(bank1::BOOTLOADER.end == bank1::LIBRARY.start);
const _: () = assert!(bank1::LIBRARY.end == bank1::LOADER.start);
const _: () = assert!(bank1::LOADER.end == bank1::ACTIVE.start);
const _: () = assert!(bank1::ACTIVE.end == bank1::DFU.start);
const _: () = assert!(bank1::DFU.end == bank1::STATE);
const _: () = assert!(bank1::STATE + 1 == bank1::UNUSED.start);
const _: () = assert!(bank1::UNUSED.end == PAGES_PER_BANK);
const _: () = assert!(len(&bank1::DFU) == len(&bank1::ACTIVE) + 1);
const _: () = assert!(within(&bank1::LIBRARY_BLANK, &bank1::LIBRARY));
const _: () = assert!(within(&page(bank1::RECORD_PAGE), &bank1::LIBRARY_BLANK));

/// Every bank 2 region, in page order.
const BANK2: [Range<u16>; 8] = [
    bank2::STOCK_SPILL,
    bank2::UNERASED_LOW,
    page(bank2::SELF_TEST),
    bank2::UNERASED_HIGH,
    bank2::RESERVE,
    bank2::SETTINGS,
    bank2::STAGING,
    bank2::GLYPH_STORE,
];

/// Whether the regions cover the bank, each page once.
const fn tiles(regions: &[Range<u16>]) -> bool {
    let mut next = 0;
    let mut i = 0;
    while i < regions.len() {
        if regions[i].start != next || regions[i].end <= next {
            return false;
        }
        next = regions[i].end;
        i += 1;
    }
    next == PAGES_PER_BANK
}

const _: () = assert!(tiles(&BANK2));
const _: () = assert!(bank2::GLYPH_STORE.end == PAGES_PER_BANK);
const _: () = assert!(len(&bank2::STAGING) == len(&bank1::LOADER) + len(&bank1::ACTIVE));

// The installer's plan, held to these numbers.
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
/// Opposite staging: never UNA's code.
const _: () = assert!(bank2::STAGING.start >= bank1::LIBRARY.end);
/// Opposite the reserve: never UNA's code.
const _: () = assert!(bank2::RESERVE.start >= bank1::LIBRARY.end);
/// Opposite the self-test: a page the library leaves blank and wrist-rs does not use.
const _: () = assert!(within(&page(bank2::SELF_TEST), &bank1::LIBRARY_BLANK));
const _: () = assert!(bank2::SELF_TEST != bank1::RECORD_PAGE);
const _: () = assert!(within(&bank2::UNERASED_LOW, &bank1::LIBRARY));
const _: () = assert!(within(&bank2::UNERASED_HIGH, &bank1::LIBRARY));

/// Whether erasing bank 2 `pages` under wrist-rs could, with a lost bank select, erase a bank 1
/// page that UNA or wrist-rs keeps.
#[must_use]
pub const fn mirror_endangers_boot(pages: &Range<u16>) -> bool {
    !(disjoint(pages, &bank1::BOOTLOADER)
        && disjoint(pages, &(bank1::LIBRARY.start..bank1::LIBRARY_BLANK.start))
        && disjoint(pages, &(bank1::LIBRARY_BLANK.end..bank1::LIBRARY.end))
        && disjoint(pages, &page(bank1::RECORD_PAGE))
        && disjoint(pages, &bank1::LOADER)
        && disjoint(pages, &bank1::ACTIVE)
        && disjoint(pages, &page(bank1::STATE)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_self_test_is_bank_2_page_46() {
        assert_eq!(bank2::SELF_TEST, 46);
        assert_eq!(crate::plan::bank2_page_address(crate::plan::SELF_TEST_PAGE), 0x0825_C000);
    }

    #[test]
    fn what_each_bank_2_erase_has_opposite_it() {
        assert!(!mirror_endangers_boot(&bank2::GLYPH_STORE));
        assert!(!mirror_endangers_boot(&bank2::SETTINGS));
        assert!(!mirror_endangers_boot(&page(bank2::SELF_TEST)));
        assert!(mirror_endangers_boot(&page(bank1::RECORD_PAGE)));
        assert!(mirror_endangers_boot(&page(bank1::LIBRARY.end - 1)));
        assert!(mirror_endangers_boot(&bank2::STOCK_SPILL));
        assert!(mirror_endangers_boot(&bank2::UNERASED_LOW));
        assert!(mirror_endangers_boot(&bank2::RESERVE));
        // Written only under stock, but under wrist-rs it would reach the state page.
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
        let first = u16::try_from((origin - crate::plan::BANK1_BASE) / 8192).unwrap();
        first..first + u16::try_from(length * 1024 / 8192).unwrap()
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
