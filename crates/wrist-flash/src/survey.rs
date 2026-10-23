//! `flash survey`: which pages hold anything, and what the flash controller's status and
//! option registers say. Read-only; the image's console does the reads, this decodes them.

use crate::plan::PAGE_SIZE;

/// Pages in each bank.
pub const PAGES_PER_BANK: usize = 256;

/// Pages a map line covers.
pub const PAGES_PER_LINE: usize = 64;

/// Words in a page, as the console reads them.
pub const PAGE_WORDS: usize = PAGE_SIZE / 4;

/// Erased flash reads as all ones.
#[must_use]
pub fn blank(words: &[u32]) -> bool {
    words.iter().all(|&w| w == u32::MAX)
}

/// A bank's pages, one bit each, set when the page holds anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Map([u64; PAGES_PER_BANK / 64]);

impl Map {
    pub fn set(&mut self, page: usize) {
        self.0[page / 64] |= 1 << (page % 64);
    }

    #[must_use]
    pub fn used(&self, page: usize) -> bool {
        self.0[page / 64] & (1 << (page % 64)) != 0
    }

    /// How many of `pages` hold anything.
    #[must_use]
    pub fn count(&self, pages: core::ops::Range<usize>) -> usize {
        pages.filter(|&p| self.used(p)).count()
    }

    /// `#` for a page that holds anything, `.` for a blank one, starting at `first`.
    pub fn line(&self, first: usize, out: &mut [u8; PAGES_PER_LINE]) {
        for (i, c) in out.iter_mut().enumerate() {
            *c = if self.used(first + i) { b'#' } else { b'.' };
        }
    }
}

/// The non-secure flash registers the survey reads (RM0456 7.9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub acr: u32,
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
    pub const ACR: u32 = 0x00;
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

/// An ECC error `ECCR` holds: whether it was corrected, the bank and the offset in it. The
/// console prints the raw register too, for `SYSF_ECC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ecc {
    pub corrected: bool,
    pub double: bool,
    pub bank2: bool,
    pub offset: u32,
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

    /// `ACR.LATENCY`, in wait states.
    #[must_use]
    pub const fn latency(&self) -> u32 {
        self.acr & 0xF
    }

    /// `ACR.PRFTEN`.
    #[must_use]
    pub const fn prefetch(&self) -> bool {
        self.acr & (1 << 8) != 0
    }

    /// Whether ES0553 2.2.13's flash conditions hold for a low-power entry run from flash:
    /// prefetch on and at least four wait states.
    #[must_use]
    pub const fn low_power_hang_possible(&self) -> bool {
        self.prefetch() && self.latency() >= 4
    }

    /// `NSSR.BSY | WDW`.
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.nssr & (0b11 << 16) != 0
    }

    #[must_use]
    pub const fn interrupted(&self) -> Option<Interrupted> {
        let code = (self.opsr >> 29) as u8;
        if code == 0 {
            return None;
        }
        Some(Interrupted {
            code,
            bank2: self.opsr & (1 << 21) != 0,
            system_flash: self.opsr & (1 << 22) != 0,
            offset: self.opsr & ADDR_MASK,
        })
    }

    #[must_use]
    pub const fn ecc(&self) -> Option<Ecc> {
        let corrected = self.eccr & (1 << 30) != 0;
        let double = self.eccr & (1 << 31) != 0;
        if !corrected && !double {
            return None;
        }
        Some(Ecc {
            corrected,
            double,
            bank2: self.eccr & (1 << 21) != 0,
            offset: self.eccr & ADDR_MASK,
        })
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

    /// No area protected, no bank swap, the controller locked and idle, and no ECC error or
    /// interrupted operation recorded: what the glyph store's survey (M2) passes on.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.protected().iter().all(Option::is_none)
            && !self.swapped()
            && self.locked()
            && !self.busy()
            && self.ecc().is_none()
            && self.interrupted().is_none()
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

    /// Reset values (RM0456 7.9) with the option bytes as an unprotected dual-bank part.
    fn reset() -> Registers {
        Registers {
            acr: 0x0000_0001,
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
    fn each_finding_spoils_a_clean_survey() {
        let spoil: [fn(&mut Registers); 6] = [
            |r| r.wrp[2] = 0x00FF_00C4,
            |r| r.optr |= 1 << 20,
            |r| r.nscr = 0,
            |r| r.nssr = 1 << 17,
            |r| r.eccr = 1 << 30,
            |r| r.opsr = 4 << 29,
        ];
        for f in spoil {
            let mut r = reset();
            f(&mut r);
            assert!(!r.clean(), "{r:?}");
        }
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
        assert!(e.double && !e.corrected && e.bank2);
        assert_eq!(e.offset, 0x1_0010);
    }

    #[test]
    fn the_low_power_hang_needs_prefetch_and_four_wait_states() {
        let mut r = reset();
        r.acr = 4;
        assert!(!r.low_power_hang_possible());
        r.acr = (1 << 8) | 3;
        assert!(!r.low_power_hang_possible());
        r.acr = (1 << 8) | 4;
        assert!(r.low_power_hang_possible());
    }
}
