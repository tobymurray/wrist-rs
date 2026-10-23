//! The install's arithmetic and refusals, pure so the host tests cover them.

pub const PAGE_SIZE: usize = 8192;
pub const PAGE_SIZE_U32: u32 = 8192;
const _: () = assert!(PAGE_SIZE as u64 == PAGE_SIZE_U32 as u64);

/// The programming granularity, RM0456 7.3.7.
pub const QUADWORD: usize = 16;

pub const KERNEL_BASE: u32 = 0x0806_0000;

/// No plan starts below this: the pages under it are the vendor bootloader's.
pub const FIRST_KERNEL_PAGE: u8 = 48;

/// The loader and the active slot (`apps/wrist-loader/memory.x`).
pub const MAX_PAGES: u8 = 68;

pub const MAX_IMAGE: usize = PAGE_SIZE * MAX_PAGES as usize;

pub const DFU_FIRST_PAGE: u8 = 116;
const _: () = assert!(FIRST_KERNEL_PAGE as usize + MAX_PAGES as usize == DFU_FIRST_PAGE as usize);

pub const BANK1_BASE: u32 = 0x0800_0000;
pub const BANK2_BASE: u32 = 0x0820_0000;
pub const FLASH_END: u32 = 0x0840_0000;

/// A generous bound on how far into bank 2 a stock kernel reaches.
pub const STOCK_KERNEL_BANK2_PAGES: u8 = 24;
/// In bank 2.
pub const SELF_TEST_PAGE: u8 = 34;

pub const STAGING_FIRST_PAGE: u8 = 128;
pub const STAGING_ADDRESS: u32 = 0x0830_0000;
pub const STAGING_END: u32 = STAGING_ADDRESS + MAX_PAGES as u32 * PAGE_SIZE_U32;
const _: () = assert!(STAGING_ADDRESS == BANK2_BASE + STAGING_FIRST_PAGE as u32 * PAGE_SIZE_U32);
const _: () = assert!(STAGING_END <= FLASH_END);
const _: () = assert!(STAGING_FIRST_PAGE > STOCK_KERNEL_BANK2_PAGES);
const _: () = assert!(STAGING_FIRST_PAGE > SELF_TEST_PAGE);
const _: () = assert!(SELF_TEST_PAGE > STOCK_KERNEL_BANK2_PAGES);
const _: () = assert!(STAGING_ADDRESS >= BANK1_BASE + DFU_FIRST_PAGE as u32 * PAGE_SIZE_U32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// A bank 1 page.
    pub first_page: u8,
    pub pages: u8,
    /// Rounded up to a whole [`QUADWORD`].
    pub padded_len: usize,
    pub quadwords: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotKernelBase,
    Empty,
    TooLarge,
    /// Cannot happen for [`KERNEL_BASE`]; checked anyway, as the one mistake that
    /// would lose the way back to stock.
    WouldEraseBootloader,
}

impl Refusal {
    /// Negative codes for the C side, never renumbered.
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Refusal::NotKernelBase => -2,
            Refusal::Empty => -3,
            Refusal::TooLarge => -4,
            Refusal::WouldEraseBootloader => -5,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::NotKernelBase => "target is not 0x08060000",
            Refusal::Empty => "image is empty",
            Refusal::TooLarge => "image needs more pages than allowed",
            Refusal::WouldEraseBootloader => "refused: would erase a bootloader page",
        }
    }
}

/// # Errors
/// The first [`Refusal`] that applies.
pub fn plan(target: u32, len: usize) -> Result<Plan, Refusal> {
    if target != KERNEL_BASE {
        return Err(Refusal::NotKernelBase);
    }
    if len == 0 {
        return Err(Refusal::Empty);
    }

    let offset = (target - BANK1_BASE) as usize;
    let first_page = u8::try_from(offset / PAGE_SIZE).map_err(|_| Refusal::NotKernelBase)?;
    if first_page < FIRST_KERNEL_PAGE {
        return Err(Refusal::WouldEraseBootloader);
    }

    let pages_needed = len.div_ceil(PAGE_SIZE);
    let pages = u8::try_from(pages_needed).map_err(|_| Refusal::TooLarge)?;
    if pages > MAX_PAGES {
        return Err(Refusal::TooLarge);
    }

    let padded_len = len.next_multiple_of(QUADWORD);
    Ok(Plan {
        first_page,
        pages,
        padded_len,
        quadwords: padded_len / QUADWORD,
    })
}

#[must_use]
pub fn bank1_page_address(page: u8) -> u32 {
    BANK1_BASE + u32::from(page) * PAGE_SIZE_U32
}

#[must_use]
pub fn bank2_page_address(page: u8) -> u32 {
    BANK2_BASE + u32::from(page) * PAGE_SIZE_U32
}

/// The bank 2 page that holds staged page `index`, if the image has one.
#[must_use]
pub fn staging_page(index: u8) -> Option<u8> {
    if index < MAX_PAGES {
        Some(STAGING_FIRST_PAGE + index)
    } else {
        None
    }
}

/// `(offset, length)` of the bytes of a `len`-byte image that page `index` holds.
#[must_use]
pub fn page_chunk(len: usize, index: u8) -> Option<(usize, usize)> {
    let p = plan(KERNEL_BASE, len).ok()?;
    if index >= p.pages {
        return None;
    }
    let offset = usize::from(index) * PAGE_SIZE;
    Some((offset, (len - offset).min(PAGE_SIZE)))
}

/// Little-endian, padded with erased flash's `0xFF`.
#[must_use]
pub fn quadword(bytes: &[u8], index: usize) -> [u32; 4] {
    let mut quad = [0u32; 4];
    for (w, slot) in quad.iter_mut().enumerate() {
        let mut word = [0xFFu8; 4];
        for (b, dst) in word.iter_mut().enumerate() {
            if let Some(&byte) = bytes.get(index * QUADWORD + w * 4 + b) {
                *dst = byte;
            }
        }
        *slot = u32::from_le_bytes(word);
    }
    quad
}

/// The zlib CRC-32.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    !crc32_continue(0xFFFF_FFFF, bytes)
}

/// [`crc32`] a chunk at a time: start from `0xFFFF_FFFF` and invert the result.
#[must_use]
pub fn crc32_continue(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_anything_but_the_kernel_base() {
        for target in [0x0800_0000, 0x0805_F000, 0x0806_0004, 0x0820_0000, 0] {
            assert_eq!(plan(target, 1024), Err(Refusal::NotKernelBase), "{target:#x}");
        }
    }

    #[test]
    fn refuses_an_empty_image() {
        assert_eq!(plan(KERNEL_BASE, 0), Err(Refusal::Empty));
    }

    #[test]
    fn refuses_an_image_needing_too_many_pages() {
        let ok = PAGE_SIZE * MAX_PAGES as usize;
        assert!(plan(KERNEL_BASE, ok).is_ok());
        assert_eq!(plan(KERNEL_BASE, ok + 1), Err(Refusal::TooLarge));
    }

    #[test]
    fn never_plans_a_bootloader_page() {
        let p = plan(KERNEL_BASE, PAGE_SIZE * MAX_PAGES as usize).unwrap();
        assert_eq!(p.first_page, FIRST_KERNEL_PAGE);
        assert!(p.first_page >= FIRST_KERNEL_PAGE);
        assert!(p.first_page as usize + p.pages as usize > FIRST_KERNEL_PAGE as usize);
    }

    #[test]
    fn page_count_rounds_up() {
        assert_eq!(plan(KERNEL_BASE, 1).unwrap().pages, 1);
        assert_eq!(plan(KERNEL_BASE, PAGE_SIZE).unwrap().pages, 1);
        assert_eq!(plan(KERNEL_BASE, PAGE_SIZE + 1).unwrap().pages, 2);
    }

    #[test]
    fn padding_rounds_to_a_whole_quadword() {
        let p = plan(KERNEL_BASE, 7896).unwrap();
        assert_eq!(p.padded_len, 7904);
        assert_eq!(p.quadwords, 494);
        assert_eq!(p.pages, 1);

        let exact = plan(KERNEL_BASE, QUADWORD * 3).unwrap();
        assert_eq!(exact.padded_len, QUADWORD * 3);
        assert_eq!(exact.quadwords, 3);
    }

    #[test]
    fn padded_length_never_shrinks_or_overruns_its_pages() {
        for len in [1usize, 15, 16, 17, 8191, 8192, 8193, 20000] {
            let Ok(p) = plan(KERNEL_BASE, len) else { continue };
            assert!(p.padded_len >= len);
            assert!(p.padded_len <= p.pages as usize * PAGE_SIZE);
            assert_eq!(p.padded_len % QUADWORD, 0);
        }
    }

    #[test]
    fn kernel_base_is_page_48() {
        assert_eq!(bank1_page_address(FIRST_KERNEL_PAGE), KERNEL_BASE);
        assert_eq!(bank1_page_address(0), 0x0800_0000);
        assert_eq!(bank1_page_address(FIRST_KERNEL_PAGE - 1), 0x0805_E000);
    }

    #[test]
    fn accepts_a_512_kib_image() {
        let p = plan(KERNEL_BASE, 512 * 1024).unwrap();
        assert_eq!(p.pages, 64);
        assert_eq!(p.padded_len, 512 * 1024);
    }

    #[test]
    fn accepts_the_loader_and_a_full_active_slot() {
        let p = plan(KERNEL_BASE, MAX_IMAGE).unwrap();
        assert_eq!(MAX_IMAGE, (32 + 512) * 1024);
        assert_eq!(p.pages, MAX_PAGES);
        assert_eq!(p.first_page + p.pages - 1, DFU_FIRST_PAGE - 1);
    }

    #[test]
    fn refuses_one_byte_or_one_page_past_the_active_slot() {
        assert_eq!(plan(KERNEL_BASE, MAX_IMAGE + 1), Err(Refusal::TooLarge));
        assert_eq!(plan(KERNEL_BASE, MAX_IMAGE + PAGE_SIZE), Err(Refusal::TooLarge));
        assert_eq!(plan(KERNEL_BASE, PAGE_SIZE * 256), Err(Refusal::TooLarge));
        assert_eq!(plan(KERNEL_BASE, usize::MAX), Err(Refusal::TooLarge));
    }

    #[test]
    fn never_plans_the_dfu_slot_or_the_state_page() {
        let last_full = MAX_IMAGE - PAGE_SIZE;
        for len in [
            1,
            PAGE_SIZE,
            128 * 1024,
            256 * 1024,
            last_full,
            last_full + 1,
            MAX_IMAGE - 1,
            MAX_IMAGE,
        ] {
            let p = plan(KERNEL_BASE, len).unwrap();
            assert!(p.first_page >= FIRST_KERNEL_PAGE, "{len}");
            assert!(p.first_page as usize + p.pages as usize <= DFU_FIRST_PAGE as usize, "{len}");
            let end = bank1_page_address(p.first_page) as usize + p.padded_len;
            assert!(end <= bank1_page_address(DFU_FIRST_PAGE) as usize, "{len}");
        }
    }

    #[test]
    fn staging_is_in_bank_2_clear_of_everything_else() {
        assert_eq!(bank2_page_address(STAGING_FIRST_PAGE), STAGING_ADDRESS);
        assert_eq!(staging_page(0), Some(STAGING_FIRST_PAGE));
        assert_eq!(staging_page(MAX_PAGES - 1), Some(195));
        assert_eq!(staging_page(MAX_PAGES), None);
        assert_eq!(staging_page(u8::MAX), None);

        let end = STAGING_ADDRESS + u32::from(MAX_PAGES) * PAGE_SIZE_U32;
        assert_eq!(end, 0x0838_8000);
        assert_eq!(STAGING_END, end);
        assert!(STAGING_ADDRESS >= bank1_page_address(DFU_FIRST_PAGE));
    }

    #[test]
    fn chunks_tile_the_image_exactly() {
        for len in [
            1usize,
            15,
            16,
            PAGE_SIZE - 1,
            PAGE_SIZE,
            PAGE_SIZE + 1,
            248_000,
            512 * 1024,
            MAX_IMAGE,
        ] {
            let p = plan(KERNEL_BASE, len).unwrap();
            let mut next = 0;
            for i in 0..p.pages {
                let (offset, n) = page_chunk(len, i).unwrap();
                assert_eq!(offset, next, "len {len} page {i}");
                assert!(n > 0 && n <= PAGE_SIZE);
                if i + 1 < p.pages {
                    assert_eq!(n, PAGE_SIZE);
                }
                next = offset + n;
            }
            assert_eq!(next, len);
            assert_eq!(page_chunk(len, p.pages), None);
        }
        assert_eq!(page_chunk(0, 0), None);
        assert_eq!(page_chunk(MAX_IMAGE + 1, 0), None);
    }

    #[test]
    fn quadwords_pad_with_ff_and_keep_the_bytes() {
        let bytes = [0xAAu8, 0xBB, 0xCC];
        let q = quadword(&bytes, 0);
        assert_eq!(q[0], u32::from_le_bytes([0xAA, 0xBB, 0xCC, 0xFF]));
        assert_eq!(&q[1..], &[0xFFFF_FFFF; 3]);
        assert_eq!(quadword(&[0x11; 16], 0), [0x1111_1111; 4]);
        assert_eq!(quadword(&bytes, 1), [0xFFFF_FFFF; 4]);
    }

    #[test]
    fn staged_pages_reassemble_the_padded_image() {
        let len = 2 * PAGE_SIZE + 7901;
        let image: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let p = plan(KERNEL_BASE, len).unwrap();
        let mut staged = Vec::new();
        for i in 0..p.pages {
            let (offset, n) = page_chunk(len, i).unwrap();
            let chunk = &image[offset..offset + n];
            for q in 0..n.div_ceil(QUADWORD) {
                staged.extend_from_slice(&quadword(chunk, q));
            }
        }
        assert_eq!(staged.len() * 4, p.padded_len);
        let whole: Vec<u32> = (0..p.quadwords).flat_map(|q| quadword(&image, q)).collect();
        assert_eq!(staged, whole);
        for (i, &byte) in image.iter().enumerate() {
            assert_eq!(staged[i / 4].to_le_bytes()[i % 4], byte, "byte {i}");
        }
        assert_eq!(staged.last().unwrap().to_le_bytes()[3], 0xFF);
    }

    #[test]
    fn crc32_matches_known_values() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }
}
