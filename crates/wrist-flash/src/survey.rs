//! What `flash survey` reports, decoded on the host: the page map, the regions, the flash
//! registers, and which ECC errors the NMI may return from.

use crate::layout::{bank1, bank2};
use crate::plan::PAGE_SIZE;

/// Pages in each bank.
pub const PAGES_PER_BANK: usize = crate::layout::PAGES_PER_BANK as usize;

/// Pages a map line covers.
pub const PAGES_PER_LINE: usize = 64;

/// Words in a page, as the console reads them.
pub const PAGE_WORDS: usize = PAGE_SIZE / 4;

/// Whether every word reads `0xFFFFFFFF`, which a quadword programmed with all ones does too,
/// so it is no check that the page can be programmed (RM0456 7.3.7).
#[must_use]
pub fn blank(words: &[u32]) -> bool {
    words.iter().all(|&w| w == u32::MAX)
}

/// A bank's pages, one bit each: those that hold anything, and those whose read raised an
/// ECC error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Map {
    used: [u64; PAGES_PER_BANK / 64],
    ecc: [u64; PAGES_PER_BANK / 64],
}

impl Map {
    pub fn set(&mut self, page: usize) {
        self.used[page / 64] |= 1 << (page % 64);
    }

    pub fn set_ecc(&mut self, page: usize) {
        self.ecc[page / 64] |= 1 << (page % 64);
    }

    #[must_use]
    pub fn used(&self, page: usize) -> bool {
        self.used[page / 64] & (1 << (page % 64)) != 0
    }

    #[must_use]
    pub fn ecc(&self, page: usize) -> bool {
        self.ecc[page / 64] & (1 << (page % 64)) != 0
    }

    /// How many of `pages` hold anything.
    #[must_use]
    pub fn count(&self, pages: core::ops::Range<usize>) -> usize {
        pages.filter(|&p| self.used(p)).count()
    }

    /// How many of `pages` raised an ECC error.
    #[must_use]
    pub fn count_ecc(&self, pages: core::ops::Range<usize>) -> usize {
        pages.filter(|&p| self.ecc(p)).count()
    }

    /// `!` for a page that raised an ECC error, `#` for one that holds anything, `.` for a
    /// blank one, starting at `first`.
    pub fn line(&self, first: usize, out: &mut [u8; PAGES_PER_LINE]) {
        for (i, c) in out.iter_mut().enumerate() {
            *c = if self.ecc(first + i) {
                b'!'
            } else if self.used(first + i) {
                b'#'
            } else {
                b'.'
            };
        }
    }
}

/// Where an ECC error was, as `FLASH_ECCR` says: `BK_ECC` and `SYSF_ECC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Bank1,
    Bank2,
    /// ST's system flash, where its ECC test words are.
    System,
}

/// A run of pages the survey names, and gives a CRC of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub area: Area,
    pub pages: core::ops::Range<u16>,
    pub name: &'static str,
}

const fn region(area: Area, pages: core::ops::Range<u16>, name: &'static str) -> Region {
    Region { area, pages, name }
}

/// Every region the survey reports, in the order it reads them, as [`crate::layout`] names them.
pub const REGIONS: [Region; 13] = [
    region(Area::Bank1, bank1::LOADER, "loader"),
    region(Area::Bank1, bank1::ACTIVE, "active"),
    region(Area::Bank1, bank1::DFU, "dfu"),
    region(Area::Bank1, bank1::STATE..bank1::STATE + 1, "state"),
    region(Area::Bank1, bank1::UNUSED, "unused"),
    region(Area::Bank2, bank2::STOCK_SPILL, "stock spill"),
    region(Area::Bank2, bank2::UNERASED_LOW, "unerased"),
    region(Area::Bank2, bank2::SELF_TEST..bank2::SELF_TEST + 1, "self-test"),
    region(Area::Bank2, bank2::UNERASED_HIGH, "unerased"),
    region(Area::Bank2, bank2::RESERVE, "reserve"),
    region(Area::Bank2, bank2::SETTINGS, "settings"),
    region(Area::Bank2, bank2::STAGING, "staging"),
    region(Area::Bank2, bank2::GLYPH_STORE, "glyph store"),
];

/// The index in [`REGIONS`] of the region `page` of `area` lies in.
#[must_use]
pub fn region_at(area: Area, page: u16) -> Option<usize> {
    REGIONS.iter().position(|r| r.area == area && r.pages.contains(&page))
}

/// The non-secure flash registers the survey reads (RM0456 7.9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub nssr: u32,
    pub nscr: u32,
    pub eccr: u32,
    pub opsr: u32,
    pub optr: u32,
    /// `WRP1AR`, `WRP1BR`, `WRP2AR`, `WRP2BR`.
    pub wrp: [u32; 4],
}

/// RM0456 7.9, Table 79.
pub mod offset {
    pub const NSSR: u32 = 0x20;
    pub const NSCR: u32 = 0x28;
    pub const ECCR: u32 = 0x30;
    pub const OPSR: u32 = 0x34;
    pub const OPTR: u32 = 0x40;
    pub const WRP: [u32; 4] = [0x58, 0x5C, 0x68, 0x6C];
}

/// `ADDR_OP` and `ADDR_ECC`: 21 bits on this part, an offset within the bank up to `0x1FFFF0`
/// (RM0456 7.9.11, 7.9.12; 20 bits only on the 2 Mbyte parts).
pub const ADDR_MASK: u32 = 0x1F_FFFF;

/// An operation `OPSR` says a reset interrupted: `CODE_OP`, the bank and the offset in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interrupted {
    pub code: u8,
    pub bank2: bool,
    pub system_flash: bool,
    pub offset: u32,
}

impl Interrupted {
    /// RM0456 7.9.12.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self.code {
            1 => "single write",
            2 => "OTP write",
            3 => "burst write",
            4 => "page erase",
            5 => "bank erase",
            6 => "mass erase",
            7 => "option change",
            _ => "reserved",
        }
    }

    /// The page the operation was on, within its bank.
    #[must_use]
    pub const fn page(&self) -> u32 {
        self.offset / crate::plan::PAGE_SIZE_U32
    }
}

/// `FLASH_ECCR.ECCD`: a double error, which raises the NMI (RM0456 7.9.11). Write 1 to clear.
pub const ECCD: u32 = 1 << 31;
/// `FLASH_ECCR.ECCC`: a corrected error. Write 1 to clear.
pub const ECCC: u32 = 1 << 30;
/// `FLASH_ECCR.ECCIE`: the corrected-error interrupt enable.
pub const ECCIE: u32 = 1 << 24;
/// `FLASH_ECCR.SYSF_ECC`: the error was in system flash.
pub const SYSF_ECC: u32 = 1 << 22;
/// `FLASH_ECCR.BK_ECC`: the error was in bank 2.
pub const BK_ECC: u32 = 1 << 21;

/// What to write to `FLASH_ECCR` to clear `ECCD` alone, keeping `ECCIE`.
#[must_use]
pub const fn eccr_clearing_double(eccr: u32) -> u32 {
    (eccr & ECCIE) | ECCD
}

/// What to write to `FLASH_ECCR` to clear both flags, keeping `ECCIE`; neither is set again
/// until both are clear (RM0456 7.9.11).
#[must_use]
pub const fn eccr_clearing_both(eccr: u32) -> u32 {
    (eccr & ECCIE) | ECCD | ECCC
}

/// An ECC error `ECCR` holds: whether it was corrected, where, and the offset there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ecc {
    pub corrected: bool,
    pub double: bool,
    pub area: Area,
    pub offset: u32,
}

impl Ecc {
    /// The page the error was on, within its area.
    #[must_use]
    pub const fn page(&self) -> u32 {
        self.offset / crate::plan::PAGE_SIZE_U32
    }
}

/// The error `eccr` records, if either flag is set.
#[must_use]
pub const fn ecc(eccr: u32) -> Option<Ecc> {
    let corrected = eccr & ECCC != 0;
    let double = eccr & ECCD != 0;
    if !corrected && !double {
        return None;
    }
    let area = if eccr & SYSF_ECC != 0 {
        Area::System
    } else if eccr & BK_ECC != 0 {
        Area::Bank2
    } else {
        Area::Bank1
    };
    Some(Ecc { corrected, double, area, offset: eccr & ADDR_MASK })
}

/// The operation `opsr` says a reset interrupted, if any.
#[must_use]
pub const fn interrupted(opsr: u32) -> Option<Interrupted> {
    let code = (opsr >> 29) as u8;
    if code == 0 {
        return None;
    }
    Some(Interrupted {
        code,
        bank2: opsr & (1 << 21) != 0,
        system_flash: opsr & (1 << 22) != 0,
        offset: opsr & ADDR_MASK,
    })
}

/// The NMI's catch armed for one page of `area`; never 0, which means disarmed.
#[must_use]
pub const fn arm(area: Area, page: u32) -> u32 {
    (1 << 31) | ((area as u32) << 16) | (page & 0xFFFF)
}

/// Whether the NMI may clear `eccr`'s error and return: a double error in the page `armed` names.
#[must_use]
pub const fn catches(armed: u32, eccr: u32) -> bool {
    match ecc(eccr) {
        Some(e) => armed != 0 && e.double && arm(e.area, e.page()) == armed,
        None => false,
    }
}

impl Registers {
    /// `NSCR.LOCK`: nothing can program or erase until the keys are written.
    #[must_use]
    pub const fn locked(&self) -> bool {
        self.nscr & (1 << 31) != 0
    }

    /// `OPTR.SWAP_BANK`: bank 2 is mapped at `0x08000000`.
    #[must_use]
    pub const fn swapped(&self) -> bool {
        self.optr & (1 << 20) != 0
    }

    /// `OPTR.DUALBANK`.
    #[must_use]
    pub const fn dual_bank(&self) -> bool {
        self.optr & (1 << 21) != 0
    }

    /// `NSSR.BSY | WDW`.
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.nssr & (0b11 << 16) != 0
    }

    #[must_use]
    pub const fn interrupted(&self) -> Option<Interrupted> {
        interrupted(self.opsr)
    }

    #[must_use]
    pub const fn ecc(&self) -> Option<Ecc> {
        ecc(self.eccr)
    }

    /// Each write-protection area as its first and last page, or `None` when it is empty
    /// (start past end, RM0456 7.6.1). In the order of [`Registers::wrp`].
    #[must_use]
    pub fn protected(&self) -> [Option<(u8, u8)>; 4] {
        self.wrp.map(|r| {
            let (start, end) = ((r & 0xFF) as u8, ((r >> 16) & 0xFF) as u8);
            (start <= end).then_some((start, end))
        })
    }

    /// What is wrong, by the names the survey prints.
    pub fn problems(&self) -> impl Iterator<Item = &'static str> {
        [
            (self.protected().iter().any(Option::is_some), "wrp"),
            (self.swapped(), "swap_bank"),
            (!self.locked(), "unlocked"),
            (self.busy(), "busy"),
            (self.ecc().is_some(), "ecc"),
            (self.interrupted().is_some(), "opsr"),
        ]
        .into_iter()
        .filter_map(|(found, name)| found.then_some(name))
    }

    #[must_use]
    pub fn clean(&self) -> bool {
        self.problems().next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_blank_only_when_every_word_is_erased() {
        let mut page = [u32::MAX; PAGE_WORDS];
        assert!(blank(&page));
        page[PAGE_WORDS - 1] = 0xFFFF_FFFE;
        assert!(!blank(&page));
    }

    #[test]
    fn the_map_counts_and_draws_pages() {
        let mut map = Map::default();
        for p in [0, 63, 64, 196, 255] {
            map.set(p);
        }
        assert_eq!(map.count(0..PAGES_PER_BANK), 5);
        assert_eq!(map.count(196..256), 2);
        assert_eq!(map.count(1..63), 0);
        let mut line = [0; PAGES_PER_LINE];
        map.line(192, &mut line);
        assert_eq!(&line[..6], b"....#.");
        assert_eq!(line[63], b'#');
        map.line(0, &mut line);
        assert_eq!((line[0], line[1], line[63]), (b'#', b'.', b'#'));
    }

    #[test]
    fn an_ecc_page_draws_over_its_contents() {
        let mut map = Map::default();
        map.set(3);
        map.set_ecc(3);
        map.set_ecc(4);
        assert_eq!(map.count_ecc(0..PAGES_PER_BANK), 2);
        assert_eq!(map.count(0..PAGES_PER_BANK), 1);
        let mut line = [0; PAGES_PER_LINE];
        map.line(0, &mut line);
        assert_eq!(&line[..6], b"...!!.");
    }

    #[test]
    fn the_regions_are_ordered_apart_and_inside_their_banks() {
        for r in &REGIONS {
            assert!(r.area != Area::System, "{r:?}");
            assert!(r.pages.start < r.pages.end && usize::from(r.pages.end) <= PAGES_PER_BANK, "{r:?}");
        }
        assert!(REGIONS[0].pages.start >= bank1::BOOTLOADER.end, "the vendor bootloader's pages are never read");
        for w in REGIONS.windows(2) {
            let ordered = w[0].area == w[1].area && w[0].pages.end <= w[1].pages.start;
            assert!(ordered || (w[0].area, w[1].area) == (Area::Bank1, Area::Bank2), "{w:?}");
        }
        assert_eq!(REGIONS.iter().filter(|r| r.area == Area::Bank1).map(|r| r.pages.len()).sum::<usize>(), 208);
        assert_eq!(REGIONS.iter().filter(|r| r.area == Area::Bank2).map(|r| r.pages.len()).sum::<usize>(), PAGES_PER_BANK);
    }

    #[test]
    fn region_at_finds_each_page_s_region() {
        assert_eq!(region_at(Area::Bank1, 47), None);
        assert_eq!(region_at(Area::Bank1, 48).map(|i| REGIONS[i].name), Some("loader"));
        assert_eq!(region_at(Area::Bank1, 181).map(|i| REGIONS[i].name), Some("state"));
        assert_eq!(region_at(Area::Bank2, 34).map(|i| REGIONS[i].name), Some("self-test"));
        assert_eq!(region_at(Area::Bank2, 255).map(|i| REGIONS[i].name), Some("glyph store"));
        assert_eq!(region_at(Area::System, 8), None);
    }

    #[test]
    fn the_catch_takes_only_a_double_error_in_its_page() {
        let bank2_page = |page: u32| ECCD | BK_ECC | (page * 8192 + 0x10);
        let armed = arm(Area::Bank2, 200);
        assert!(catches(armed, bank2_page(200)));
        assert!(!catches(0, bank2_page(200)), "disarmed");
        assert!(!catches(armed, bank2_page(201)), "the next page");
        assert!(!catches(armed, bank2_page(200) & !BK_ECC), "bank 1");
        assert!(!catches(armed, (bank2_page(200) & !ECCD) | ECCC), "corrected only");
        assert!(!catches(armed, 0));
        let test_word = ECCD | SYSF_ECC | 0x1_1F80;
        assert!(catches(arm(Area::System, 8), test_word), "ST's 2-bit test word");
        assert!(!catches(arm(Area::Bank1, 8), test_word));
        assert!(catches(arm(Area::Bank2, 255), ECCD | BK_ECC | 0x1F_FFF0), "the top of a bank");
    }

    #[test]
    fn clearing_keeps_the_interrupt_enable_and_writes_no_reserved_bit() {
        for eccr in [0, u32::MAX, ECCD | ECCC | 0x18_8000, ECCIE | ECCD | BK_ECC] {
            let one = eccr_clearing_double(eccr);
            let both = eccr_clearing_both(eccr);
            assert_eq!(one & ECCIE, eccr & ECCIE, "{eccr:08X}");
            assert_eq!(one & !ECCIE, ECCD, "only ECCD cleared, {eccr:08X}");
            assert_eq!(both & !ECCIE, ECCD | ECCC, "{eccr:08X}");
        }
    }

    /// Reset values (RM0456 7.9) with the option bytes as an unprotected dual-bank part.
    fn reset() -> Registers {
        Registers {
            nssr: 0,
            nscr: 0x8000_0000,
            eccr: 0,
            opsr: 0,
            optr: 0x1FEF_F8AA,
            wrp: [0x0000_00FF; 4],
        }
    }

    #[test]
    fn the_reset_state_is_clean() {
        let r = reset();
        assert_eq!(r.protected(), [None; 4]);
        assert!(r.dual_bank());
        assert!(!r.swapped());
        assert!(r.locked());
        assert!(r.clean());
    }

    #[test]
    fn each_finding_is_named_and_spoils_a_clean_survey() {
        /// A change to the reset state, and the problem it should be named as.
        type Spoil = (fn(&mut Registers), &'static str);
        let spoil: [Spoil; 6] = [
            (|r| r.wrp[2] = 0x00FF_00C4, "wrp"),
            (|r| r.optr |= 1 << 20, "swap_bank"),
            (|r| r.nscr = 0, "unlocked"),
            (|r| r.nssr = 1 << 17, "busy"),
            (|r| r.eccr = 1 << 30, "ecc"),
            (|r| r.opsr = 4 << 29, "opsr"),
        ];
        for (f, name) in spoil {
            let mut r = reset();
            f(&mut r);
            assert!(!r.clean(), "{r:?}");
            assert_eq!(r.problems().collect::<Vec<_>>(), [name], "{r:?}");
        }
        let r = Registers { eccr: ECCD, opsr: 4 << 29, ..reset() };
        assert_eq!(r.problems().collect::<Vec<_>>(), ["ecc", "opsr"]);
    }

    #[test]
    fn write_protection_areas_decode_as_pages() {
        let mut r = reset();
        r.wrp[2] = 0x00FF_00C4;
        assert_eq!(r.protected(), [None, None, Some((0xC4, 0xFF)), None]);
    }

    #[test]
    fn an_interrupted_erase_names_its_bank_and_page() {
        let r = Registers { opsr: (4 << 29) | (1 << 21) | 0x1_8800, ..reset() };
        let op = r.interrupted().unwrap();
        assert_eq!(op.name(), "page erase");
        assert!(op.bank2 && !op.system_flash);
        assert_eq!(op.page(), 0x1_8800 / 8192);
    }

    #[test]
    fn addresses_reach_the_top_of_a_bank() {
        let r = Registers { opsr: (3 << 29) | (1 << 21) | 0x1F_E010, eccr: (1 << 31) | (1 << 21) | 0x1F_FFF0, ..reset() };
        assert_eq!(r.interrupted().unwrap().page(), 255);
        assert_eq!(r.ecc().unwrap().offset, 0x1F_FFF0);
    }

    #[test]
    fn an_ecc_error_decodes() {
        let r = Registers { eccr: (1 << 31) | (1 << 21) | 0x1_0010, ..reset() };
        let e = r.ecc().unwrap();
        assert!(e.double && !e.corrected);
        assert_eq!(e.area, Area::Bank2);
        assert_eq!(e.offset, 0x1_0010);
        assert_eq!(ecc(ECCC | SYSF_ECC | BK_ECC | 0x1_1F00).unwrap().area, Area::System, "SYSF_ECC wins");
    }
}
