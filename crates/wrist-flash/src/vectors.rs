//! Whether a file sent over the console is an image for the slot it is going to, and the top of
//! SRAM that the vendor bootloader, the loader and the image share (HARDWARE.md, "Boot handoff").

use core::ops::Range;

use crate::layout::bank1;
use crate::plan::{BANK1_BASE, KERNEL_BASE, PAGE_SIZE_U32};

/// SRAM1 to SRAM5, contiguous.
pub const SRAM_START: u32 = 0x2000_0000;
pub const SRAM_END: u32 = 0x2027_0000;

/// Popped at and above the SP word at handoff (HARDWARE.md, "Boot handoff").
pub const HANDOFF_POP: u32 = 16;
pub const SP_MIN: u32 = SRAM_START;
pub const SP_MAX: u32 = SRAM_END - HANDOFF_POP;

/// The loader's mailbox: [`MAILBOX_TAG`], then `RCC_BDCR` as the loader found it.
pub const MAILBOX_MAGIC: u32 = SRAM_END - 16;
pub const MAILBOX_BDCR: u32 = SRAM_END - 12;
pub const MAILBOX_TAG: u32 = u32::from_be_bytes(*b"LDCR");

/// The retained failure record's words, either side of the mailbox (HARDWARE.md, "Retained
/// failure record").
pub const FAILURE_RECORD: [u32; 6] =
    [SRAM_END - 32, SRAM_END - 28, SRAM_END - 24, SRAM_END - 20, SRAM_END - 8, SRAM_END - 4];
/// Below the record and the mailbox.
pub const STACK_START: u32 = FAILURE_RECORD[0];
const _: () = assert!(STACK_START <= SP_MAX);

/// The two slots of `apps/wrist-loader/LAYOUT.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Loader,
    Application,
}

impl Target {
    const fn pages(self) -> Range<u16> {
        match self {
            Target::Loader => bank1::LOADER,
            Target::Application => bank1::ACTIVE,
        }
    }

    #[must_use]
    pub const fn base(self) -> u32 {
        BANK1_BASE + self.pages().start as u32 * PAGE_SIZE_U32
    }

    #[must_use]
    pub const fn size(self) -> u32 {
        let pages = self.pages();
        (pages.end - pages.start) as u32 * PAGE_SIZE_U32
    }

    #[must_use]
    pub fn accepts(self, image: &[u8]) -> bool {
        vectors_ok(image, self.base(), self.size())
    }
}

const _: () = assert!(Target::Loader.base() == KERNEL_BASE);

/// The initial SP is one the bootloader can hand over, and the reset vector is Thumb and inside
/// `base..base + size`.
#[must_use]
pub fn vectors_ok(image: &[u8], base: u32, size: u32) -> bool {
    let word = |i: usize| {
        image
            .get(i * 4..i * 4 + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let (Some(sp), Some(reset)) = (word(0), word(1)) else {
        return false;
    };
    let entry = reset & !1;
    (SP_MIN..=SP_MAX).contains(&sp) && reset & 1 == 1 && entry >= base && entry - base < size
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOADER: [u8; 8] = [0xE0, 0xFF, 0x26, 0x20, 0x51, 0x02, 0x06, 0x08];
    const APP: [u8; 8] = [0xE0, 0xFF, 0x26, 0x20, 0x51, 0x82, 0x06, 0x08];

    fn table(sp: u32, reset: u32) -> Vec<u8> {
        let mut image = Vec::new();
        image.extend_from_slice(&sp.to_le_bytes());
        image.extend_from_slice(&reset.to_le_bytes());
        image.extend_from_slice(&[0xAA; 64]);
        image
    }

    #[test]
    fn the_slots_are_the_linker_layout() {
        assert_eq!(Target::Loader.base(), 0x0806_0000);
        assert_eq!(Target::Application.base(), 0x0806_8000);
        assert_eq!(
            Target::Loader.base() + Target::Loader.size(),
            Target::Application.base()
        );
        assert_eq!(Target::Application.size(), 512 * 1024);
    }

    #[test]
    fn accepts_the_shipped_vector_tables() {
        assert!(Target::Loader.accepts(&LOADER));
        assert!(Target::Application.accepts(&APP));
    }

    #[test]
    fn refuses_each_image_in_the_other_slot() {
        assert!(!Target::Application.accepts(&LOADER));
        assert!(!Target::Loader.accepts(&APP));
    }

    #[test]
    fn refuses_text_and_short_files() {
        for target in [Target::Loader, Target::Application] {
            assert!(!target.accepts(b"1A2B3C4D\n"));
            assert!(!target.accepts(&APP[..7]));
            assert!(!target.accepts(&[]));
        }
    }

    #[test]
    fn refuses_an_even_reset_vector() {
        assert!(!Target::Application.accepts(&table(STACK_START, 0x0806_8250)));
    }

    #[test]
    fn the_sp_stops_16_bytes_below_the_bootloader_limit() {
        let reset = 0x0806_8251;
        assert!(Target::Application.accepts(&table(SP_MAX, reset)));
        assert!(!Target::Application.accepts(&table(SRAM_END, reset)));
        assert!(Target::Application.accepts(&table(SP_MIN, reset)));
        assert!(!Target::Application.accepts(&table(SP_MIN - 4, reset)));
    }

    fn hex_after(line: &str, prefix: &str) -> u32 {
        let hex: String = line.split(prefix).nth(1).unwrap().chars().take_while(|c| c.is_ascii_hexdigit() || *c == '_').collect();
        u32::from_str_radix(&hex.replace('_', ""), 16).unwrap()
    }

    fn line<'a>(text: &'a str, start: &str) -> &'a str {
        text.lines().find(|l| l.trim_start().starts_with(start)).unwrap_or_else(|| panic!("no {start}"))
    }

    #[test]
    fn both_linker_scripts_share_this_map() {
        let app = include_str!("../../../apps/standalone-boot/memory.x");
        assert_eq!(hex_after(line(app, "_stack_start"), "= 0x"), STACK_START);

        let loader_x = include_str!("../../../apps/wrist-loader/memory.x");
        let ram = line(loader_x, "RAM ");
        let (origin, length) = (hex_after(ram, "ORIGIN = 0x"), hex_after(ram, "LENGTH = 0x"));
        assert_eq!((origin, origin + length), (SRAM_START, SRAM_END));
        let below: u32 = line(loader_x, "_stack_start")
            .strip_prefix("_stack_start = ORIGIN(RAM) + LENGTH(RAM) - ")
            .and_then(|rest| rest.strip_suffix(';'))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(SRAM_END - below, STACK_START);
    }

    #[test]
    fn the_reset_vector_stays_inside_the_slot() {
        let sp = STACK_START;
        let end = Target::Application.base() + Target::Application.size();
        assert!(Target::Application.accepts(&table(sp, (end - 2) | 1)));
        assert!(!Target::Application.accepts(&table(sp, end | 1)));
        assert!(!Target::Application.accepts(&table(sp, (Target::Application.base() - 2) | 1)));
        assert!(Target::Loader.accepts(&table(sp, Target::Loader.base() | 1)));
        assert!(!Target::Loader.accepts(&table(sp, Target::Application.base() | 1)));
    }
}
