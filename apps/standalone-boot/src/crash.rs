//! The retained failure record (`HARDWARE.md`, "Retained failure record").
use crate::{mmio::{read32, write32}, uart::Uart};
use wrist_crash::{Build, Decoded, Kind, Record};
/// The gap is the loader's mailbox (`MAILBOX_*` in `apps/wrist-loader/src/main.rs`).
const ADDRS: [u32; 6] = [0x2026_FFE0, 0x2026_FFE4, 0x2026_FFE8, 0x2026_FFEC, 0x2026_FFF8, 0x2026_FFFC];
fn read() -> Decoded { wrist_crash::decode(ADDRS.map(|a| unsafe { read32(a) })) }
fn write(words: wrist_crash::Words) {
    // The tag last.
    for i in [0, 1, 2, 3, 5, 4] { unsafe { write32(ADDRS[i], words[i]) } }
    cortex_m::asm::dsb();
}
fn store(kind: Kind) {
    write(Record {
        kind,
        build: Build::from_id(crate::version::BUILD_ID),
        uptime_s: u32::try_from(embassy_time::Instant::now().as_secs()).unwrap_or(u32::MAX),
        count: wrist_crash::next_count(read()),
    }.encode());
}
pub fn panic(file: &str, line: u32) { store(Kind::Panic { line, file_fnv1a: wrist_crash::fnv1a(file.as_bytes()) }); }
pub fn fault(status: u32, pc: u32) { store(Kind::Fault { cfsr_low20: status & 0xF_FFFF, pc }); }
pub fn clear() { write([0; 6]); }
pub fn report(uart: Uart) {
    let record = read();
    let kind = match record {
        Decoded::None => return uart.write_line("[crash] no retained failure"),
        Decoded::Old(kind) | Decoded::Record(Record { kind, .. }) => kind,
    };
    match kind {
        Kind::Panic { line, file_fnv1a } => {
            uart.write_str("[crash] panic line="); uart.write_u32(line);
            uart.write_str(" file_fnv1a="); uart.write_hex32(file_fnv1a);
        }
        Kind::Fault { cfsr_low20, pc } => {
            uart.write_str("[crash] fault cfsr_low20="); uart.write_hex32(cfsr_low20);
            uart.write_str(" pc="); uart.write_hex32(pc);
        }
    }
    let Decoded::Record(r) = record else {
        return uart.write_line(" (an older image's record: no build, uptime or count)");
    };
    uart.write_str(" build="); uart.write_str(r.build.text(&mut [0; 18]));
    uart.write_str(" uptime_s="); uart.write_u32(r.uptime_s);
    uart.write_str(" count="); uart.write_u32(u32::from(r.count));
    uart.write_line(if r.build == Build::from_id(crate::version::BUILD_ID) { "" } else { " (not the running build)" });
}
