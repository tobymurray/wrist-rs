//! The retained failure record (`HARDWARE.md`, "Retained failure record").
use crate::{mmio::{read32, write32}, uart::Uart};
const BASE: u32 = 0x2026_FFF8;
const PANIC: u32 = 0xD2C0_0000;
const FAULT: u32 = 0xD2F0_0000;
pub fn panic(file: &str, line: u32) {
    let hash = file.bytes().fold(0x811C_9DC5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    store(PANIC | (line & 0xF_FFFF), hash);
}
pub fn fault(status: u32, pc: u32) { store(FAULT | (status & 0xF_FFFF), pc); }
fn store(a: u32, b: u32) {
    unsafe { write32(BASE + 4, b); write32(BASE, a); }
    cortex_m::asm::dsb();
}
pub fn clear() { store(0, 0); }
pub fn report(uart: Uart) {
    let (a, b) = unsafe { (read32(BASE), read32(BASE + 4)) };
    if a & 0xFFF0_0000 == PANIC {
        uart.write_str("[crash] panic line="); uart.write_u32(a & 0xF_FFFF);
        uart.write_str(" file_fnv1a="); uart.write_hex32(b); uart.write_line("");
    } else if a & 0xFFF0_0000 == FAULT {
        uart.write_str("[crash] fault cfsr_low20="); uart.write_hex32(a & 0xF_FFFF);
        uart.write_str(" pc="); uart.write_hex32(b); uart.write_line("");
    } else { uart.write_line("[crash] no retained failure"); }
}
