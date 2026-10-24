//! `flash survey`: every page from the loader up, blank or not, and the flash controller's
//! status and option registers (`wrist_flash::survey`). It only reads. The vendor
//! bootloader's pages, bank 1 0 to 47, are not read.
//!
//! Also the NMI's catch: a double ECC error in the glyph store is cleared and returned from,
//! so a damaged pack is rejected instead of resetting the watch
//! (`plan/GLYPH-STORE-2026-10-23.md` 6.2). `flash ecc-test` proves the path on ST's
//! test words in system flash.
use crate::{
    mmio::{read32, write32, FLASH_BASE},
    uart::Uart,
};
use core::{
    ops::Range,
    ptr::read_volatile,
    sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed},
};
use wrist_store::{eccr_clearing_both, eccr_clearing_double, nmi_in_store, nmi_in_system_flash, ECCC, ECCD, ECCIE};
use wrist_flash::{
    layout::{bank1, bank2},
    plan::{bank1_page_address, bank2_page_address},
    survey::{blank, offset, Map, Registers, PAGES_PER_BANK, PAGES_PER_LINE, PAGE_WORDS},
};

/// The bank 2 regions `wrist_flash::layout` names.
const BANK2_REGIONS: [(Range<u16>, &str); 5] = [
    (bank2::STOCK_SPILL, "stock spill"),
    (bank2::RESERVE, "reserve"),
    (bank2::SETTINGS, "settings"),
    (bank2::STAGING, "staging"),
    (bank2::GLYPH_STORE, "glyph store"),
];

/// Words in system flash with a 1-bit and a 2-bit ECC error, for testing (RM0456 7.3.2).
const TEST_WORD_CORRECTED: u32 = 0x0BFA_1F00;
const TEST_WORD_DOUBLE: u32 = 0x0BFA_1F80;
const SYSF_ECC: u32 = 1 << 22;

/// Double errors the NMI has returned from since boot, and the `FLASH_ECCR` of the last.
static CAUGHT: AtomicU32 = AtomicU32::new(0);
static LAST_CAUGHT: AtomicU32 = AtomicU32::new(0);
/// Set by `flash ecc-test` for its one read of the 2-bit word; the NMI takes it.
static SELF_TEST: AtomicBool = AtomicBool::new(false);

/// From the NMI. True when it was a double ECC error in the glyph store, or the armed
/// self-test's, now cleared, so the handler returns. Every other NMI still resets.
pub fn nmi_caught() -> bool {
    let eccr = unsafe { read32(FLASH_BASE + offset::ECCR) };
    let test = SELF_TEST.swap(false, Relaxed);
    if !(nmi_in_store(eccr) || (test && nmi_in_system_flash(eccr))) {
        return false;
    }
    unsafe { write32(FLASH_BASE + offset::ECCR, eccr_clearing_double(eccr)) };
    // RM0456 7.3.2: the NMI "must clean the cache".
    crate::update::invalidate_icache();
    LAST_CAUGHT.store(eccr, Relaxed);
    CAUGHT.fetch_add(1, Relaxed);
    true
}

fn fence() {
    unsafe { core::arch::asm!("dsb sy", "isb sy", options(nostack)) };
}

fn clear_ecc() {
    unsafe { write32(FLASH_BASE + offset::ECCR, eccr_clearing_both(read32(FLASH_BASE + offset::ECCR))) };
    crate::update::invalidate_icache();
}

/// Reads ST's corrected word, then its double-error word with the catch armed for it.
fn ecc_test(uart: Uart) {
    let eccr = || unsafe { read32(FLASH_BASE + offset::ECCR) };
    if eccr() & ECCIE != 0 {
        // A corrected error would raise the flash interrupt, which nothing here handles.
        uart.write_line("[flash] ecc-test: ECCIE is set; not reading");
        return;
    }
    clear_ecc();
    let _ = unsafe { read_volatile(TEST_WORD_CORRECTED as *const u32) };
    fence();
    let corrected = eccr();
    write_eccr(uart, "[flash] ecc-test 1-bit word: eccr ", corrected);
    clear_ecc();

    let before = CAUGHT.load(Relaxed);
    SELF_TEST.store(true, Relaxed);
    fence();
    let _ = unsafe { read_volatile(TEST_WORD_DOUBLE as *const u32) };
    fence();
    let armed_after = SELF_TEST.swap(false, Relaxed);
    let caught = CAUGHT.load(Relaxed).wrapping_sub(before);
    let after = eccr();
    uart.write_str("[flash] ecc-test 2-bit word: NMIs caught ");
    uart.write_u32(caught);
    uart.write_line(if armed_after { ", catch still armed" } else { "" });
    if caught > 0 {
        write_eccr(uart, "[flash] ecc-test eccr in the NMI ", LAST_CAUGHT.load(Relaxed));
    }
    write_eccr(uart, "[flash] ecc-test eccr now ", after);
    clear_ecc();

    let pass = corrected & (ECCC | SYSF_ECC) == ECCC | SYSF_ECC
        && caught == 1
        && LAST_CAUGHT.load(Relaxed) & (ECCD | SYSF_ECC) == ECCD | SYSF_ECC
        && after & ECCD == 0;
    uart.write_line(if pass { "[flash] ecc-test passed" } else { "[flash] ecc-test FAILED" });
}

pub fn command(args: &[u8], uart: Uart) {
    match args {
        b" survey" => crate::REPORT.signal(crate::Report::FlashSurvey),
        b" ecc-test" => ecc_test(uart),
        _ => uart.write_line("[flash] usage: flash survey|ecc-test"),
    }
}

fn registers() -> Registers {
    let at = |off| unsafe { read32(FLASH_BASE + off) };
    Registers {
        acr: at(offset::ACR),
        nssr: at(offset::NSSR),
        nscr: at(offset::NSCR),
        eccr: at(offset::ECCR),
        opsr: at(offset::OPSR),
        optr: at(offset::OPTR),
        wrp: offset::WRP.map(at),
    }
}

fn page_blank(address: u32) -> bool {
    // SAFETY: a whole page of memory-mapped flash, read only.
    let words = unsafe { core::slice::from_raw_parts(address as *const u32, PAGE_WORDS) };
    blank(words)
}

fn write_on_off(uart: Uart, label: &str, on: bool) {
    uart.write_str(label);
    uart.write_str(if on { "on" } else { "off" });
}

fn write_registers(uart: Uart, r: &Registers) {
    uart.write_str("[flash] acr ");
    uart.write_hex32(r.acr);
    uart.write_str(" latency ");
    uart.write_u32(r.latency());
    write_on_off(uart, " prefetch ", r.prefetch());
    uart.write_line(if r.low_power_hang_possible() { " (ES0553 2.2.13 applies)" } else { "" });

    uart.write_str("[flash] nssr ");
    uart.write_hex32(r.nssr);
    uart.write_str(" nscr ");
    uart.write_hex32(r.nscr);
    uart.write_str(if r.locked() { " locked" } else { " UNLOCKED" });
    uart.write_line(if r.busy() { " busy" } else { " idle" });

    uart.write_str("[flash] optr ");
    uart.write_hex32(r.optr);
    write_on_off(uart, " dualbank ", r.dual_bank());
    write_on_off(uart, " swap_bank ", r.swapped());
    uart.write_line("");

    uart.write_str("[flash] wrp");
    for (raw, area) in r.wrp.iter().zip(r.protected()) {
        uart.write_str(" ");
        uart.write_hex32(*raw);
        if let Some((first, last)) = area {
            uart.write_str("=");
            uart.write_u32(u32::from(first));
            uart.write_str("-");
            uart.write_u32(u32::from(last));
        }
    }
    uart.write_line(if r.protected().iter().all(Option::is_none) { " (none)" } else { " (PROTECTED)" });

    uart.write_str("[flash] opsr ");
    uart.write_hex32(r.opsr);
    match r.interrupted() {
        None => uart.write_line(" (no interrupted operation)"),
        Some(op) => {
            uart.write_str(" interrupted ");
            uart.write_str(op.name());
            uart.write_str(if op.system_flash { " system flash" } else if op.bank2 { " bank2 page " } else { " bank1 page " });
            uart.write_u32(op.page());
            uart.write_line("");
        }
    }
    write_eccr(uart, "[flash] eccr ", r.eccr);
}

fn write_eccr(uart: Uart, label: &str, eccr: u32) {
    uart.write_str(label);
    uart.write_hex32(eccr);
    match (Registers { eccr, ..Registers::default() }).ecc() {
        None => uart.write_line(" (none)"),
        Some(e) => {
            uart.write_str(if e.double { " DOUBLE" } else { "" });
            uart.write_str(if e.corrected { " corrected" } else { "" });
            uart.write_str(if eccr & SYSF_ECC != 0 {
                " system flash offset "
            } else if e.bank2 {
                " bank2 offset "
            } else {
                " bank1 offset "
            });
            uart.write_hex32(e.offset);
            uart.write_line("");
        }
    }
}

async fn write_map(uart: Uart, bank: &str, map: &Map, unread: usize) {
    let mut line = [0; PAGES_PER_LINE];
    for first in (0..PAGES_PER_BANK).step_by(PAGES_PER_LINE) {
        map.line(first, &mut line);
        for (i, c) in line.iter_mut().enumerate() {
            if first + i < unread {
                *c = b' ';
            }
        }
        uart.write_str("[flash] ");
        uart.write_str(bank);
        uart.write_str(" ");
        if first < 100 {
            uart.write_str(if first < 10 { "00" } else { "0" });
        }
        uart.write_u32(u32::try_from(first).unwrap_or(0));
        uart.write_str(" ");
        uart.write_line(core::str::from_utf8(&line).unwrap_or("?"));
        crate::console::make_room().await;
    }
}

/// `[flash] bank2 196-255 glyph store: 0 of 60 not blank`.
fn write_count(uart: Uart, bank: &str, pages: &Range<u16>, what: &str, map: &Map) {
    uart.write_str("[flash] ");
    uart.write_str(bank);
    uart.write_str(" ");
    uart.write_u32(u32::from(pages.start));
    uart.write_str("-");
    uart.write_u32(u32::from(pages.end - 1));
    uart.write_str(" ");
    uart.write_str(what);
    uart.write_str(": ");
    let count = map.count(usize::from(pages.start)..usize::from(pages.end));
    uart.write_u32(u32::try_from(count).unwrap_or(0));
    uart.write_str(" of ");
    uart.write_u32(u32::from(pages.end - pages.start));
    uart.write_line(" not blank");
}

/// One page a turn of the executor: a page is 2048 word reads.
pub async fn survey(uart: Uart) {
    let before = registers();
    write_registers(uart, &before);
    crate::console::make_room().await;
    if before.swapped() {
        uart.write_line("[flash] banks swapped: the addresses below are not the pages named; stopping");
        return;
    }

    let first = usize::from(bank1::BOOTLOADER.end);
    let (mut bank1, mut bank2) = (Map::default(), Map::default());
    for page in first..PAGES_PER_BANK {
        if !page_blank(bank1_page_address(u8::try_from(page).unwrap_or(u8::MAX))) {
            bank1.set(page);
        }
        embassy_futures::yield_now().await;
    }
    for page in 0..PAGES_PER_BANK {
        if !page_blank(bank2_page_address(u8::try_from(page).unwrap_or(u8::MAX))) {
            bank2.set(page);
        }
        embassy_futures::yield_now().await;
    }

    uart.write_line("[flash] map: # holds anything, . blank, space not read (the vendor bootloader)");
    write_map(uart, "bank1", &bank1, first).await;
    write_map(uart, "bank2", &bank2, 0).await;
    write_count(uart, "bank1", &bank1::UNUSED, "unused, past the state page", &bank1);
    for (pages, what) in BANK2_REGIONS {
        write_count(uart, "bank2", &pages, what, &bank2);
    }
    let after = registers();
    write_eccr(uart, "[flash] eccr after the reads ", after.eccr);
    uart.write_str("[flash] nmi: double errors caught since boot ");
    uart.write_u32(CAUGHT.load(Relaxed));
    uart.write_line("");
    uart.write_str("[flash] survey ");
    uart.write_line(if before.clean() && after.ecc().is_none() { "clean" } else { "NOT clean" });
}
