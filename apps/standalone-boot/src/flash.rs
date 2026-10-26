//! `flash survey` and `flash ecc-test`, which never write flash, and the NMI's catch for a
//! double ECC error in the page a read has armed.
use crate::{
    mmio::{read32, write32, FLASH_BASE},
    uart::Uart,
};
use core::{
    ptr::read_volatile,
    sync::atomic::{AtomicU32, Ordering::Relaxed},
};
use wrist_flash::{
    plan::{bank1_page_address, bank2_page_address, crc32_continue, FIRST_KERNEL_PAGE, PAGE_SIZE, PAGE_SIZE_U32},
    survey::{
        arm, blank, catches, ecc, eccr_clearing_both, eccr_clearing_double, offset, region_at, Area, Map, Region,
        Registers, ECCC, ECCD, ECCIE, PAGES_PER_BANK, PAGES_PER_LINE, REGIONS, SYSF_ECC,
    },
};

/// Where system flash begins, as `ECCR`'s offsets count (HARDWARE.md, `flash ecc-test`).
const SYSTEM_FLASH_BASE: u32 = 0x0BF9_0000;
/// Words in system flash with a 1-bit and a 2-bit ECC error, for testing (RM0456 7.3.2).
const TEST_WORD_CORRECTED: u32 = 0x0BFA_1F00;
const TEST_WORD_DOUBLE: u32 = 0x0BFA_1F80;

/// Bytes read a turn of the executor, since the CRC is bitwise.
const CHUNK: usize = 256;

/// The page the NMI's catch is armed for (`wrist_flash::survey::arm`), or 0.
static ARMED: AtomicU32 = AtomicU32::new(0);
/// Double errors the NMI has returned from since boot, and the `FLASH_ECCR` of the last.
static CAUGHT: AtomicU32 = AtomicU32::new(0);
static LAST_CAUGHT: AtomicU32 = AtomicU32::new(0);

/// From the NMI: whether it was a double ECC error in the armed page, now cleared, so the
/// handler may return.
pub fn nmi_caught() -> bool {
    let eccr = eccr();
    if !catches(ARMED.load(Relaxed), eccr) {
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

fn eccr() -> u32 {
    unsafe { read32(FLASH_BASE + offset::ECCR) }
}

fn clear_ecc() {
    unsafe { write32(FLASH_BASE + offset::ECCR, eccr_clearing_both(eccr())) };
}

/// Reads ST's corrected word, then its double-error word with the catch armed for it.
fn ecc_test(uart: Uart) {
    if eccr() & ECCIE != 0 {
        uart.write_line("[flash] ecc-test: ECCIE is set; not reading");
        return;
    }
    clear_ecc();
    crate::update::invalidate_icache();
    let _ = unsafe { read_volatile(TEST_WORD_CORRECTED as *const u32) };
    fence();
    let corrected = eccr();
    write_eccr(uart, "[flash] ecc-test 1-bit word: eccr ", corrected);
    clear_ecc();
    crate::update::invalidate_icache();

    let before = CAUGHT.load(Relaxed);
    ARMED.store(arm(Area::System, (TEST_WORD_DOUBLE - SYSTEM_FLASH_BASE) / PAGE_SIZE_U32), Relaxed);
    fence();
    let _ = unsafe { read_volatile(TEST_WORD_DOUBLE as *const u32) };
    fence();
    ARMED.store(0, Relaxed);
    let caught = CAUGHT.load(Relaxed).wrapping_sub(before);
    let after = eccr();
    uart.write_str("[flash] ecc-test 2-bit word: NMIs caught ");
    uart.write_u32(caught);
    uart.write_line("");
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
        nssr: at(offset::NSSR),
        nscr: at(offset::NSCR),
        eccr: at(offset::ECCR),
        opsr: at(offset::OPSR),
        optr: at(offset::OPTR),
        wrp: offset::WRP.map(at),
    }
}

/// The raw registers on one line, the verdict, then a decoded line for each problem.
fn write_registers(uart: Uart, r: &Registers) {
    uart.write_str("[flash] registers nssr ");
    uart.write_hex32(r.nssr);
    uart.write_str(" nscr ");
    uart.write_hex32(r.nscr);
    uart.write_str(" optr ");
    uart.write_hex32(r.optr);
    uart.write_str(" wrp");
    for raw in r.wrp {
        uart.write_str(" ");
        uart.write_hex32(raw);
    }
    uart.write_str(" opsr ");
    uart.write_hex32(r.opsr);
    uart.write_str(" eccr ");
    uart.write_hex32(r.eccr);
    uart.write_line("");
    write_verdict(uart, r);

    for (i, area) in r.protected().iter().enumerate() {
        if let Some((first, last)) = area {
            uart.write_str(["[flash] wrp WRP1A", "[flash] wrp WRP1B", "[flash] wrp WRP2A", "[flash] wrp WRP2B"][i]);
            uart.write_str(" protects pages ");
            uart.write_u32(u32::from(*first));
            uart.write_str("-");
            uart.write_u32(u32::from(*last));
            uart.write_line("");
        }
    }
    if let Some(op) = r.interrupted() {
        uart.write_str("[flash] opsr: a reset interrupted a ");
        uart.write_str(op.name());
        uart.write_str(if op.system_flash { " in system flash" } else if op.bank2 { " on bank2 page " } else { " on bank1 page " });
        if !op.system_flash {
            uart.write_u32(op.page());
        }
        uart.write_line("");
    }
    if r.ecc().is_some() {
        write_eccr(uart, "[flash] eccr ", r.eccr);
    }
}

/// `[flash] registers clean`, or `[flash] registers NOT clean: ecc, opsr`.
fn write_verdict(uart: Uart, r: &Registers) {
    if r.clean() {
        uart.write_line("[flash] registers clean");
        return;
    }
    uart.write_str("[flash] registers NOT clean:");
    for (i, name) in r.problems().enumerate() {
        uart.write_str(if i == 0 { " " } else { ", " });
        uart.write_str(name);
    }
    uart.write_line("");
}

fn write_eccr(uart: Uart, label: &str, eccr: u32) {
    uart.write_str(label);
    uart.write_hex32(eccr);
    match ecc(eccr) {
        None => uart.write_line(" (none)"),
        Some(e) => {
            uart.write_str(if e.double { " DOUBLE" } else { "" });
            uart.write_str(if e.corrected { " corrected" } else { "" });
            uart.write_str(match e.area {
                Area::Bank1 => " bank1 offset ",
                Area::Bank2 => " bank2 offset ",
                Area::System => " system flash offset ",
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

/// `[flash] bank2 196-255 glyph store: 0 of 60 not blank, 0 ECC, crc 0x1A2B3C4D`.
fn write_region(uart: Uart, r: &Region, map: &Map, crc: u32) {
    let pages = usize::from(r.pages.start)..usize::from(r.pages.end);
    uart.write_str(if r.area == Area::Bank1 { "[flash] bank1 " } else { "[flash] bank2 " });
    uart.write_u32(u32::from(r.pages.start));
    if r.pages.len() > 1 {
        uart.write_str("-");
        uart.write_u32(u32::from(r.pages.end - 1));
    }
    uart.write_str(" ");
    uart.write_str(r.name);
    uart.write_str(": ");
    uart.write_u32(u32::try_from(map.count(pages.clone())).unwrap_or(0));
    uart.write_str(" of ");
    uart.write_u32(u32::try_from(r.pages.len()).unwrap_or(0));
    uart.write_str(" not blank, ");
    uart.write_u32(u32::try_from(map.count_ecc(pages)).unwrap_or(0));
    uart.write_str(" ECC, crc ");
    uart.write_hex32(crc);
    uart.write_line("");
}

/// Reads a page [`CHUNK`] bytes a turn with the catch armed for it, carrying `crc` through it:
/// whether it is blank, and whether its read raised an ECC error.
async fn read_page(area: Area, page: u16, address: u32, crc: &mut u32) -> (bool, bool) {
    clear_ecc();
    let caught = CAUGHT.load(Relaxed);
    ARMED.store(arm(area, u32::from(page)), Relaxed);
    fence();
    let mut is_blank = true;
    for chunk in 0..PAGE_SIZE / CHUNK {
        let at = address as usize + chunk * CHUNK;
        // SAFETY: a word-aligned chunk of memory-mapped flash inside the page, read only.
        let bytes = unsafe { core::slice::from_raw_parts(at as *const u8, CHUNK) };
        let words = unsafe { core::slice::from_raw_parts(at as *const u32, CHUNK / 4) };
        is_blank &= blank(words);
        *crc = crc32_continue(*crc, bytes);
        embassy_futures::yield_now().await;
    }
    fence();
    ARMED.store(0, Relaxed);
    let here = ecc(eccr()).is_some_and(|e| e.area == area && e.page() == u32::from(page));
    (is_blank, here || CAUGHT.load(Relaxed) != caught)
}

pub async fn survey(uart: Uart) {
    let before = registers();
    write_registers(uart, &before);
    crate::console::make_room().await;
    if before.swapped() {
        uart.write_line("[flash] banks swapped: the addresses below are not the pages named; stopping");
        return;
    }

    // A cached line would not be read from flash again, nor raise its error again.
    crate::update::invalidate_icache();
    let caught_before = CAUGHT.load(Relaxed);
    let first = u16::from(FIRST_KERNEL_PAGE);
    let (mut bank1, mut bank2) = (Map::default(), Map::default());
    let mut crcs = [0; REGIONS.len()];
    for (area, map, from) in [(Area::Bank1, &mut bank1, first), (Area::Bank2, &mut bank2, 0)] {
        let mut crc = 0xFFFF_FFFF;
        for page in from..u16::try_from(PAGES_PER_BANK).unwrap_or(u16::MAX) {
            let region = region_at(area, page);
            if region.is_some_and(|i| REGIONS[i].pages.start == page) {
                crc = 0xFFFF_FFFF;
            }
            let p = u8::try_from(page).unwrap_or(u8::MAX);
            let address = if area == Area::Bank1 { bank1_page_address(p) } else { bank2_page_address(p) };
            let (is_blank, raised) = read_page(area, page, address, &mut crc).await;
            if !is_blank {
                map.set(usize::from(page));
            }
            if raised {
                map.set_ecc(usize::from(page));
            }
            if let Some(i) = region.filter(|&i| REGIONS[i].pages.end == page + 1) {
                crcs[i] = !crc;
            }
        }
    }
    clear_ecc();

    uart.write_line("[flash] map: # holds anything, . blank, ! ECC error, space not read (the vendor bootloader)");
    write_map(uart, "bank1", &bank1, usize::from(first)).await;
    write_map(uart, "bank2", &bank2, 0).await;
    for (r, crc) in REGIONS.iter().zip(crcs) {
        write_region(uart, r, if r.area == Area::Bank1 { &bank1 } else { &bank2 }, crc);
    }
    crate::console::make_room().await;
    let ecc_pages = bank1.count_ecc(0..PAGES_PER_BANK) + bank2.count_ecc(0..PAGES_PER_BANK);
    uart.write_str("[flash] ecc: ");
    uart.write_u32(u32::try_from(ecc_pages).unwrap_or(0));
    uart.write_str(" pages raised an error, ");
    uart.write_u32(CAUGHT.load(Relaxed).wrapping_sub(caught_before));
    uart.write_str(" double errors caught (");
    uart.write_u32(CAUGHT.load(Relaxed));
    uart.write_line(" since boot)");
    // Pages in use are not a fault, so the verdict leaves them out.
    uart.write_str("[flash] survey: registers ");
    uart.write_str(if before.clean() { "clean" } else { "NOT clean" });
    uart.write_line(if ecc_pages == 0 { ", no ECC errors in the reads" } else { ", ECC ERRORS in the reads" });
}
