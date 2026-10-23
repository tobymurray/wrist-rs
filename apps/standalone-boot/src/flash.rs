//! `flash survey`: every page from the loader up, blank or not, and the flash controller's
//! status and option registers (`wrist_flash::survey`). It only reads. The vendor
//! bootloader's pages, bank 1 0 to 47, are not read.
use crate::{
    mmio::{read32, FLASH_BASE},
    uart::Uart,
};
use wrist_flash::{
    plan::{bank1_page_address, bank2_page_address, FIRST_KERNEL_PAGE},
    survey::{blank, offset, Map, Registers, PAGES_PER_BANK, PAGES_PER_LINE, PAGE_WORDS},
};

/// The ranges the glyph-store plan names for bank 2, as `(first, last, what)`.
const BANK2_REGIONS: [(usize, usize, &str); 5] = [
    (0, 23, "stock spill"),
    (48, 115, "reserve"),
    (116, 127, "settings"),
    (128, 195, "staging"),
    (196, 255, "glyph store"),
];

pub fn command(args: &[u8], uart: Uart) {
    match args {
        b" survey" => crate::REPORT.signal(crate::Report::FlashSurvey),
        _ => uart.write_line("[flash] usage: flash survey"),
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
            uart.write_str(if e.bank2 { " bank2 offset " } else { " bank1 offset " });
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

/// One page a turn of the executor: a page is 2048 word reads.
pub async fn survey(uart: Uart) {
    let before = registers();
    write_registers(uart, &before);
    crate::console::make_room().await;
    if before.swapped() {
        uart.write_line("[flash] banks swapped: the addresses below are not the pages named; stopping");
        return;
    }

    let first = usize::from(FIRST_KERNEL_PAGE);
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
    uart.write_str("[flash] bank1 182-255 (past the state page): ");
    uart.write_u32(u32::try_from(bank1.count(182..256)).unwrap_or(0));
    uart.write_line(" of 74 not blank");
    for (lo, hi, what) in BANK2_REGIONS {
        uart.write_str("[flash] bank2 ");
        uart.write_u32(u32::try_from(lo).unwrap_or(0));
        uart.write_str("-");
        uart.write_u32(u32::try_from(hi).unwrap_or(0));
        uart.write_str(" ");
        uart.write_str(what);
        uart.write_str(": ");
        uart.write_u32(u32::try_from(bank2.count(lo..hi + 1)).unwrap_or(0));
        uart.write_str(" of ");
        uart.write_u32(u32::try_from(hi + 1 - lo).unwrap_or(0));
        uart.write_line(" not blank");
    }
    let after = registers();
    write_eccr(uart, "[flash] eccr after the reads ", after.eccr);
    uart.write_str("[flash] survey ");
    uart.write_line(if before.clean() && after.ecc().is_none() { "clean" } else { "NOT clean" });
}
