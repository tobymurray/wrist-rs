//! Whether a file sent over the console is an image for the slot it is going to.

use crate::plan::KERNEL_BASE;

pub const SP_MIN: u32 = 0x2000_0000;
/// The vendor bootloader accepts up to `0x2027_0000` but pops 16 bytes above the SP word
/// (HARDWARE.md, "Boot handoff").
pub const SP_MAX: u32 = 0x2026_FFF0;

/// The two slots of `apps/wrist-loader/LAYOUT.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Loader,
    Application,
}

impl Target {
    #[must_use]
    pub const fn base(self) -> u32 {
        match self {
            Target::Loader => KERNEL_BASE,
            Target::Application => KERNEL_BASE + 32 * 1024,
        }
    }

    #[must_use]
    pub const fn size(self) -> u32 {
        match self {
            Target::Loader => 32 * 1024,
            Target::Application => 512 * 1024,
        }
    }

    #[must_use]
    pub fn accepts(self, image: &[u8]) -> bool {
        vectors_ok(image, self.base(), self.size())
    }
}

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
        assert!(!Target::Application.accepts(&table(0x2026_FFE0, 0x0806_8250)));
    }

    #[test]
    fn the_sp_stops_16_bytes_below_the_bootloader_limit() {
        let reset = 0x0806_8251;
        assert!(Target::Application.accepts(&table(SP_MAX, reset)));
        assert!(!Target::Application.accepts(&table(0x2027_0000, reset)));
        assert!(Target::Application.accepts(&table(SP_MIN, reset)));
        assert!(!Target::Application.accepts(&table(SP_MIN - 4, reset)));
    }

    #[test]
    fn the_reset_vector_stays_inside_the_slot() {
        let sp = 0x2026_FFE0;
        let end = Target::Application.base() + Target::Application.size();
        assert!(Target::Application.accepts(&table(sp, (end - 2) | 1)));
        assert!(!Target::Application.accepts(&table(sp, end | 1)));
        assert!(!Target::Application.accepts(&table(sp, (Target::Application.base() - 2) | 1)));
        assert!(Target::Loader.accepts(&table(sp, Target::Loader.base() | 1)));
        assert!(!Target::Loader.accepts(&table(sp, Target::Application.base() | 1)));
    }
}
