//! Everything that can fail recoverably fails before the first bank 1 erase.

// Target only, where `usize` is `u32`.
#![allow(clippy::cast_possible_truncation)]

use crate::plan::{
    self, bank1_page_address, bank2_page_address, crc32, quadword, Plan, MAX_PAGES, PAGE_SIZE,
    QUADWORD, SELF_TEST_PAGE, STAGING_ADDRESS,
};

// RM0456 7.9, Table 79.
const FLASH_BASE: u32 = 0x4002_2000;
const FLASH_NSKEYR: u32 = FLASH_BASE + 0x08;
const FLASH_NSSR: u32 = FLASH_BASE + 0x20;
const FLASH_NSCR: u32 = FLASH_BASE + 0x28;

const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xCDEF_89AB;

const NSCR_LOCK: u32 = 1 << 31;
const NSCR_STRT: u32 = 1 << 16;
const NSCR_BKER: u32 = 1 << 11;
const NSCR_PNB_SHIFT: u32 = 3;
const NSCR_PNB: u32 = 0xFF << NSCR_PNB_SHIFT;
const NSCR_PER: u32 = 1 << 1;
const NSCR_PG: u32 = 1 << 0;

const NSSR_BSY: u32 = 1 << 16;
const NSSR_WDW: u32 = 1 << 17;
const NSSR_NOT_IDLE: u32 = NSSR_BSY | NSSR_WDW;
const NSSR_EOP: u32 = 1 << 0;
const NSSR_ERRORS: u32 = 0x20FA;

/// RM0456 8.7.1.
const ICACHE_CR: u32 = 0x4003_0400;
const ICACHE_CR_CACHEINV: u32 = 1 << 1;
const ICACHE_CR_EN: u32 = 1 << 0;

/// RM0456 61.7.1.
const IWDG_KR: u32 = 0x4000_3000;
const IWDG_KEY_RELOAD: u32 = 0xAAAA;

const SCB_AIRCR: u32 = 0xE000_ED0C;
const AIRCR_SYSRESETREQ: u32 = (0x5FA << 16) | (1 << 2);

/// A blank page at the tail of the bootloader's partition, which the recovery's
/// erase does not reach (`HARDWARE.md`, "Boot handoff").
const STATUS_PAGE: u8 = 45;
const STATUS_ADDRESS: u32 = 0x0805_A000;
const STATUS_MAGIC: u32 = u32::from_le_bytes(*b"UNAF");

/// Slot 0 of the status record, written before bank 1 is touched; the outcome goes in slot 1.
const OUTCOME_BEGAN: u32 = 9;
const OUTCOME_OK: u32 = 1;
/// `detail`: the page's index in the image `<< 24`, then `NSSR`'s errors.
const OUTCOME_ERASE_FAILED: u32 = 2;
/// `detail`: `NSSR`'s errors.
const OUTCOME_PROGRAM_FAILED: u32 = 3;
/// `detail`: the byte offset of the first mismatch.
const OUTCOME_VERIFY_FAILED: u32 = 4;
/// `detail`: the word read instead of `0xFFFF_FFFF`.
const OUTCOME_ERASE_NOT_BLANK: u32 = 5;
/// `detail`: [`POLL_TIMED_OUT`], after the page's index `<< 24` when erasing.
const OUTCOME_TIMEOUT: u32 = 6;
/// `detail`: the page's index `<< 24`, then [`NOT_ARMED`] with `NSCR`'s top half.
const OUTCOME_ERASE_NOT_ARMED: u32 = 7;

pub const ERR_UNLOCK: i32 = -6;
pub const ERR_STATUS_PAGE: i32 = -7;
pub const ERR_WITNESS_FOREIGN: i32 = -8;
pub const ERR_STAGE_ARGS: i32 = -9;
pub const ERR_STAGE_ERASE: i32 = -10;
pub const ERR_STAGE_PROGRAM: i32 = -11;
pub const ERR_STAGE_VERIFY: i32 = -12;
pub const ERR_STAGED_MISMATCH: i32 = -13;

unsafe fn witness_page_ours(address: u32, magic: u32) -> bool {
    let first = read_flash(address);
    first == 0xFFFF_FFFF || first == magic
}

/// Far beyond a page erase at any clock, and still finite.
const POLL_BUDGET: u32 = 20_000_000;
const POLL_TIMED_OUT: u32 = 0xFFFF;
const NOT_ARMED: u32 = 0xE000;

const WRITE_ATTEMPTS: u32 = 3;

#[inline]
unsafe fn read32(addr: u32) -> u32 {
    (addr as *const u32).read_volatile()
}

#[inline]
unsafe fn write32(addr: u32, value: u32) {
    (addr as *mut u32).write_volatile(value);
}

/// Whether `NSCR` is writable after; the keys go in only while `LOCK` is set
/// (`HARDWARE.md`, "Boot handoff").
unsafe fn unlock() -> bool {
    if read32(FLASH_NSCR) & NSCR_LOCK == 0 {
        return true;
    }
    write32(FLASH_NSKEYR, KEY1);
    write32(FLASH_NSKEYR, KEY2);
    read32(FLASH_NSCR) & NSCR_LOCK == 0
}

unsafe fn lock() {
    write32(FLASH_NSCR, NSCR_LOCK);
}

/// `PRIMASK` as it was, for [`restore_irqs`].
unsafe fn mask_irqs() -> u32 {
    let primask: u32;
    core::arch::asm!("mrs {}, PRIMASK", out(reg) primask, options(nomem, nostack, preserves_flags));
    core::arch::asm!("cpsid i", options(nomem, nostack));
    primask
}

unsafe fn restore_irqs(primask: u32) {
    if primask & 1 == 0 {
        core::arch::asm!("cpsie i", options(nomem, nostack));
    }
}

/// In case the vendor kernel started the IWDG; reloading a stopped one does nothing.
unsafe fn kick_watchdog() {
    write32(IWDG_KR, IWDG_KEY_RELOAD);
}

pub fn reset() -> ! {
    loop {
        unsafe {
            write32(SCB_AIRCR, AIRCR_SYSRESETREQ);
            core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));
        }
    }
}

/// Data reads of flash go through the ICACHE too (RM0456 8.7.1), so a read after a
/// write needs this.
unsafe fn invalidate_icache() {
    write32(ICACHE_CR, read32(ICACHE_CR) | ICACHE_CR_CACHEINV);
    for _ in 0..POLL_BUDGET {
        if read32(ICACHE_CR) & ICACHE_CR_CACHEINV == 0 {
            break;
        }
    }
    core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));
}

unsafe fn read_flash(addr: u32) -> u32 {
    invalidate_icache();
    read32(addr)
}

/// `Err`: `NSSR`'s errors, cleared, or [`POLL_TIMED_OUT`].
unsafe fn wait_idle() -> Result<(), u32> {
    let mut idle = false;
    for _ in 0..POLL_BUDGET {
        if read32(FLASH_NSSR) & NSSR_NOT_IDLE == 0 {
            idle = true;
            break;
        }
    }
    if !idle {
        return Err(POLL_TIMED_OUT);
    }
    let errors = read32(FLASH_NSSR) & NSSR_ERRORS;
    if errors != 0 {
        write32(FLASH_NSSR, errors);
        return Err(errors);
    }
    if read32(FLASH_NSSR) & NSSR_EOP != 0 {
        write32(FLASH_NSSR, NSSR_EOP);
    }
    Ok(())
}

unsafe fn clear_op_bits(mask: u32) {
    let cr = read32(FLASH_NSCR);
    write32(FLASH_NSCR, cr & !mask);
}

/// Leaves `PER` set for [`clear_op_bits`] after the last page; `Err` as
/// [`wait_idle`]'s, or [`NOT_ARMED`] with `NSCR`'s top half.
unsafe fn erase_page(page: u8, bank2: bool) -> Result<(), u32> {
    wait_idle()?;

    let mut cr = read32(FLASH_NSCR);
    if bank2 {
        cr |= NSCR_BKER;
    } else {
        cr &= !NSCR_BKER;
    }
    write32(FLASH_NSCR, cr);

    // Page numbers repeat across banks, so a dropped bank select would erase a live kernel page.
    let mut cr = read32(FLASH_NSCR);
    if (cr & NSCR_BKER != 0) != bank2 || cr & NSCR_LOCK != 0 {
        return Err(NOT_ARMED | (cr >> 16));
    }
    cr &= !(NSCR_STRT | NSCR_PNB | NSCR_PER);
    cr |= (u32::from(page) << NSCR_PNB_SHIFT) | NSCR_STRT | NSCR_PER;
    write32(FLASH_NSCR, cr);

    // A locked `NSCR` drops a store without a flag, so read it back.
    let readback = read32(FLASH_NSCR);
    if readback & NSCR_LOCK != 0 || readback & NSCR_PER == 0 {
        return Err(NOT_ARMED | (readback >> 16));
    }

    wait_idle()
}

unsafe fn program_quadword(address: u32, quad: &[u32]) -> Result<(), u32> {
    wait_idle()?;
    let cr = read32(FLASH_NSCR);
    write32(FLASH_NSCR, cr | NSCR_PG);
    for (i, &word) in quad.iter().enumerate() {
        write32(address + (i as u32) * 4, word);
    }
    core::arch::asm!("dsb sy", options(nomem, nostack));
    let result = wait_idle();
    clear_op_bits(NSCR_PG);
    result
}

/// `words` must be whole quad-words, at a 128-bit-aligned `address`.
unsafe fn program(address: u32, words: &[u32]) -> Result<(), u32> {
    let mut addr = address;
    let (quads, _) = words.as_chunks::<{ QUADWORD / 4 }>();
    for quad in quads {
        program_quadword(addr, quad)?;
        addr += QUADWORD as u32;
    }
    Ok(())
}

unsafe fn program_paged(address: u32, words: &[u32]) -> Result<(), u32> {
    let mut addr = address;
    for page in words.chunks(PAGE_SIZE / 4) {
        kick_watchdog();
        program(addr, page)?;
        addr += PAGE_SIZE as u32;
    }
    Ok(())
}

/// The byte offset of the first word that differs.
unsafe fn verify(address: u32, words: &[u32]) -> Option<usize> {
    invalidate_icache();
    for (i, &want) in words.iter().enumerate() {
        if read32(address + (i as u32) * 4) != want {
            return Some(i * 4);
        }
    }
    None
}

unsafe fn record_at(slot: u32, outcome: u32, detail: u32, image_crc: u32, plan: Plan) {
    if !unlock() {
        return;
    }
    let packed = (u32::from(plan.first_page) << 8) | u32::from(plan.pages);
    let _: Result<(), u32> = program(
        STATUS_ADDRESS + slot * (QUADWORD as u32),
        &[STATUS_MAGIC, (outcome << 16) | packed, detail, image_crc],
    );
    // No lock here: the caller's next erase needs `NSCR` unlocked.
}

unsafe fn staged_crc(len: usize) -> u32 {
    invalidate_icache();
    crc32(core::slice::from_raw_parts(STAGING_ADDRESS as *const u8, len))
}

unsafe fn staged_padding_erased(len: usize, plan: Plan) -> bool {
    invalidate_icache();
    (len..plan.padded_len).all(|i| ((STAGING_ADDRESS as usize + i) as *const u8).read_volatile() == 0xFF)
}

/// Returns only if it aborts before bank 1 is touched; otherwise it resets.
unsafe fn commit(plan: Plan, image_crc: u32) -> i32 {
    let words = core::slice::from_raw_parts(STAGING_ADDRESS as *const u32, plan.padded_len / 4);

    core::arch::asm!("cpsid i", options(nomem, nostack));
    // Before anything is erased, so a fault here costs nothing.
    kick_watchdog();

    // Off until the reset; restored on the abort paths.
    let icache = read32(ICACHE_CR);
    write32(ICACHE_CR, icache & !ICACHE_CR_EN);
    core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));

    if !unlock() {
        write32(ICACHE_CR, icache);
        core::arch::asm!("cpsie i", options(nomem, nostack));
        return ERR_UNLOCK;
    }

    // The witness page is in the bootloader's partition: refuse unless it is blank or ours.
    if !witness_page_ours(STATUS_ADDRESS, STATUS_MAGIC) {
        lock();
        write32(ICACHE_CR, icache);
        core::arch::asm!("cpsie i", options(nomem, nostack));
        return ERR_WITNESS_FOREIGN;
    }
    let witness_ok = erase_page(STATUS_PAGE, false).is_ok();
    clear_op_bits(NSCR_PER | NSCR_STRT);
    if !witness_ok {
        lock();
        write32(ICACHE_CR, icache);
        core::arch::asm!("cpsie i", options(nomem, nostack));
        return ERR_STATUS_PAGE;
    }

    record_at(0, OUTCOME_BEGAN, 0, image_crc, plan);

    // Point of no return: the kernel's vector table is in these pages.
    let mut outcome = OUTCOME_ERASE_FAILED;
    let mut detail = 0u32;
    let base = bank1_page_address(plan.first_page);
    for _ in 0..WRITE_ATTEMPTS {
        let mut failed = false;
        for i in 0..plan.pages {
            kick_watchdog();
            if let Err(errors) = erase_page(plan.first_page + i, false) {
                outcome = if errors == POLL_TIMED_OUT {
                    OUTCOME_TIMEOUT
                } else if errors & 0xF000 == NOT_ARMED {
                    OUTCOME_ERASE_NOT_ARMED
                } else {
                    OUTCOME_ERASE_FAILED
                };
                detail = (u32::from(i) << 24) | errors;
                failed = true;
                break;
            }
        }
        clear_op_bits(NSCR_PER | NSCR_STRT);
        if failed {
            continue;
        }

        // A silently dropped erase would otherwise surface as a program failure.
        let mut not_blank = None;
        for i in 0..plan.pages {
            let word = read_flash(bank1_page_address(plan.first_page + i));
            if word != 0xFFFF_FFFF {
                not_blank = Some(word);
                break;
            }
        }
        if let Some(word) = not_blank {
            outcome = OUTCOME_ERASE_NOT_BLANK;
            detail = word;
            continue;
        }

        match program_paged(base, words) {
            Err(errors) => {
                outcome =
                    if errors == POLL_TIMED_OUT { OUTCOME_TIMEOUT } else { OUTCOME_PROGRAM_FAILED };
                detail = errors;
            }
            Ok(()) => {
                if let Some(at) = verify(base, words) {
                    outcome = OUTCOME_VERIFY_FAILED;
                    detail = at as u32;
                } else {
                    outcome = OUTCOME_OK;
                    detail = 0;
                    break;
                }
            }
        }
    }

    record_at(1, outcome, detail, image_crc, plan);
    write32(FLASH_NSCR, NSCR_LOCK);
    reset()
}

const SELF_TEST_ADDRESS: u32 = 0x0824_4000;
const SELF_TEST_PATTERN: u32 = u32::from_le_bytes(*b"TEST");

/// Bits: 0 erased, 1 blank, 2 programmed, 3 read back, 4 erased again, 5 blank
/// again, 8 to 23 `NSSR` after the first erase; -1 if flash would not unlock.
///
/// # Safety
/// Erases and programs bank 2's [`SELF_TEST_PAGE`] only.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_erase_selftest() -> i32 {
    core::arch::asm!("cpsid i", options(nomem, nostack));
    let mut r = 0i32;

    if !unlock() {
        core::arch::asm!("cpsie i", options(nomem, nostack));
        return -1;
    }

    if erase_page(SELF_TEST_PAGE, true).is_ok() {
        r |= 1;
    }
    clear_op_bits(NSCR_PER | NSCR_STRT);
    r |= i32::try_from(read32(FLASH_NSSR) & 0xFFFF).unwrap_or(0) << 8;
    if read_flash(SELF_TEST_ADDRESS) == 0xFFFF_FFFF {
        r |= 2;
    }

    if program(
        SELF_TEST_ADDRESS,
        &[SELF_TEST_PATTERN, SELF_TEST_PATTERN, SELF_TEST_PATTERN, SELF_TEST_PATTERN],
    )
    .is_ok()
    {
        r |= 4;
    }
    if read_flash(SELF_TEST_ADDRESS) == SELF_TEST_PATTERN {
        r |= 8;
    }

    if erase_page(SELF_TEST_PAGE, true).is_ok() {
        r |= 16;
    }
    if read_flash(SELF_TEST_ADDRESS) == 0xFFFF_FFFF {
        r |= 32;
    }

    write32(FLASH_NSCR, NSCR_LOCK);
    core::arch::asm!("cpsie i", options(nomem, nostack));
    r
}

/// Bit 0: the first unlock worked; bit 1: a second one returned instead of faulting.
///
/// # Safety
/// Writes FLASH registers. Nothing is erased or programmed.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_unlock_selftest() -> i32 {
    core::arch::asm!("cpsid i", options(nomem, nostack));
    let mut result = 0i32;
    if unlock() {
        result |= 1;
    }
    if unlock() {
        result |= 2;
    }
    write32(FLASH_NSCR, NSCR_LOCK);
    core::arch::asm!("cpsie i", options(nomem, nostack));
    result
}

/// Stages page `index` of a `len`-byte image from exactly the bytes
/// [`plan::page_chunk`] gives it: 0, or a negative `ERR_` code.
///
/// # Safety
/// `chunk` must point to `chunk_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_stage_page(
    len: usize,
    index: u32,
    chunk: *const u8,
    chunk_len: usize,
) -> i32 {
    let Ok(index) = u8::try_from(index) else {
        return ERR_STAGE_ARGS;
    };
    let (Some((_, expected)), Some(page)) = (plan::page_chunk(len, index), plan::staging_page(index))
    else {
        return ERR_STAGE_ARGS;
    };
    if chunk.is_null() || chunk_len != expected {
        return ERR_STAGE_ARGS;
    }
    let bytes = core::slice::from_raw_parts(chunk, chunk_len);
    let address = bank2_page_address(page);
    let quads = chunk_len.div_ceil(QUADWORD);

    let primask = mask_irqs();
    if !unlock() {
        restore_irqs(primask);
        return ERR_UNLOCK;
    }
    let erased = erase_page(page, true);
    clear_op_bits(NSCR_PER | NSCR_STRT);
    restore_irqs(primask);
    if erased.is_err() {
        lock();
        return ERR_STAGE_ERASE;
    }

    let mut addr = address;
    for q in 0..quads {
        let primask = mask_irqs();
        let programmed = program_quadword(addr, &quadword(bytes, q));
        restore_irqs(primask);
        if programmed.is_err() {
            lock();
            return ERR_STAGE_PROGRAM;
        }
        addr += QUADWORD as u32;
    }
    lock();

    invalidate_icache();
    let mut addr = address;
    for q in 0..quads {
        let want = quadword(bytes, q);
        for (w, &word) in want.iter().enumerate() {
            if read32(addr + (w as u32) * 4) != word {
                return ERR_STAGE_VERIFY;
            }
        }
        addr += QUADWORD as u32;
    }
    0
}

/// 0 if [`plan::plan`] refuses `len`.
///
/// # Safety
/// Reads flash only.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_staged_crc(len: usize) -> u32 {
    if plan::plan(plan::KERNEL_BASE, len).is_err() {
        return 0;
    }
    staged_crc(len)
}

/// Staging pages that are not blank.
///
/// # Safety
/// Reads flash only.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_staging_used() -> u32 {
    invalidate_icache();
    let mut used = 0;
    for index in 0..MAX_PAGES {
        let Some(page) = plan::staging_page(index) else {
            break;
        };
        let base = bank2_page_address(page);
        let words = plan::PAGE_SIZE_U32 / 4;
        if (0..words).any(|w| read32(base + w * 4) != 0xFFFF_FFFF) {
            used += 1;
        }
    }
    used
}

/// How many of the glyph store's pages (`layout::bank2::GLYPH_STORE`) hold anything, for the
/// dry run's log: every return to stock should leave none.
///
/// # Safety
/// Reads flash only.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_store_used() -> u32 {
    invalidate_icache();
    let mut used = 0;
    for page in crate::layout::bank2::GLYPH_STORE {
        let base = bank2_page_address(page as u8);
        let words = plan::PAGE_SIZE_U32 / 4;
        if (0..words).any(|w| read32(base + w * 4) != 0xFFFF_FFFF) {
            used += 1;
        }
    }
    used
}

/// Returns a negative code only if nothing in bank 1 was touched; otherwise it resets.
///
/// # Safety
/// The caller's code must be in RAM: the kernel's is erased.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_install(target: u32, len: usize, crc: u32) -> i32 {
    let p = match plan::plan(target, len) {
        Err(refusal) => return refusal.code(),
        Ok(p) => p,
    };
    if staged_crc(len) != crc || !staged_padding_erased(len, p) {
        return ERR_STAGED_MISMATCH;
    }
    commit(p, crc)
}
