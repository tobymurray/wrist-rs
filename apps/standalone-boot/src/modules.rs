//! The display's and the GNSS module's EEPROM records, judged once at boot (`HARDWARE.md`, "The
//! module EEPROM").

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_stm32::i2c::{Error, I2c, Master};
use embassy_stm32::mode::Mode;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Instant;
use wrist_eeprom::info::{judge, Record, Verdict, RECORD_BYTES};
use wrist_eeprom::Eeprom;

use crate::uart::Uart;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    View = 0,
    Link1 = 1,
    Link2 = 2,
}

const ALL: [Slot; 3] = [Slot::View, Slot::Link1, Slot::Link2];

struct Spec {
    name: &'static str,
    bus: &'static str,
    expects: &'static str,
    drives: &'static str,
    without: &'static str,
}

const SPECS: [Spec; 3] = [
    Spec {
        name: "view",
        bus: "I2C1",
        expects: "LS012",
        drives: "the panel (lcd.rs)",
        without: "nothing drawn, VCOM not started",
    },
    Spec {
        name: "link1",
        bus: "I2C3",
        expects: "BLUENRG2",
        #[cfg(feature = "ble")]
        drives: "Bluetooth, once `ble on` (ble.rs)",
        #[cfg(not(feature = "ble"))]
        drives: "nothing: this build has no Bluetooth; the chip is held in reset (park.rs)",
        without: "`ble on` refused",
    },
    Spec {
        name: "link2",
        bus: "I2C4",
        expects: "AG3335M",
        drives: "the step counter (imu.rs); the compass reset to suspend at boot (park.rs)",
        without: "no steps; the IMU and compass keep whatever mode the last image left them in",
    },
];

#[derive(Clone, Copy)]
struct Found {
    verdict: Verdict,
    read: Result<[u8; RECORD_BYTES], &'static str>,
    took_us: u32,
}

static FOUND: Mutex<CriticalSectionRawMutex, RefCell<[Found; 3]>> = Mutex::new(RefCell::new(
    [Found { verdict: Verdict::Unread, read: Err("not read yet"), took_us: 0 }; 3],
));
static VIEW_FORCED: AtomicBool = AtomicBool::new(false);
static LINK1_FORCED: AtomicBool = AtomicBool::new(false);

/// The slots `modules force` may override for this boot.
fn forced(slot: Slot) -> bool {
    match slot {
        Slot::View => VIEW_FORCED.load(Ordering::Relaxed),
        Slot::Link1 => LINK1_FORCED.load(Ordering::Relaxed),
        Slot::Link2 => false,
    }
}

fn found(slot: Slot) -> Found {
    FOUND.lock(|f| f.borrow()[slot as usize])
}

pub fn read_at_boot<M: Mode>(slot: Slot, bus: &mut I2c<'static, M, Master>, uart: Uart) {
    let t = Instant::now();
    let mut eeprom = Eeprom::new(bus);
    let (verdict, read) = match (eeprom.record(), eeprom.record()) {
        (Ok(first), Ok(second)) => (judge(&first, &second, SPECS[slot as usize].expects), Ok(first)),
        (Err(e), _) | (_, Err(e)) => (Verdict::Unread, Err(error_word(e))),
    };
    let took_us = u32::try_from(t.elapsed().as_micros()).unwrap_or(u32::MAX);
    FOUND.lock(|f| f.borrow_mut()[slot as usize] = Found { verdict, read, took_us });
    write_slot(uart, slot);
}

fn error_word(e: Error) -> &'static str {
    match e {
        Error::Nack => "no answer at 0x50 (NACK)",
        Error::Timeout => "timed out (20 ms)",
        Error::Arbitration => "arbitration lost",
        _ => "bus error",
    }
}

pub fn allowed(slot: Slot) -> bool {
    !found(slot).verdict.refuses() || forced(slot)
}

pub fn command(args: &[u8], uart: Uart) {
    match core::str::from_utf8(args).unwrap_or("").trim() {
        "" => {
            for slot in ALL {
                write_slot(uart, slot);
            }
        }
        "force view" => {
            if !found(Slot::View).verdict.refuses() {
                return uart.write_line("[mod] view is not refused; nothing to force");
            }
            VIEW_FORCED.store(true, Ordering::Relaxed);
            crate::wake_display();
            uart.write_line("[mod] view forced for this boot: the panel starts now");
        }
        "force link1" => {
            if !found(Slot::Link1).verdict.refuses() {
                return uart.write_line("[mod] link1 is not refused; nothing to force");
            }
            LINK1_FORCED.store(true, Ordering::Relaxed);
            uart.write_line("[mod] link1 forced for this boot: `ble on` may start it");
        }
        _ => uart.write_line("[mod] usage: modules | modules force view|link1"),
    }
}

fn write_slot(uart: Uart, slot: Slot) {
    let spec = &SPECS[slot as usize];
    let f = found(slot);
    uart.write_str("[mod] ");
    uart.write_str(spec.name);
    uart.write_str(" ");
    uart.write_str(spec.bus);
    uart.write_str(": ");
    match &f.read {
        Err(why) => uart.write_str(why),
        Ok(bytes) => write_record(uart, bytes),
    }
    uart.write_str("; expects ");
    uart.write_str(spec.expects);
    uart.write_str(": ");
    uart.write_str(f.verdict.word());
    uart.write_str(" (");
    uart.write_u32(f.took_us);
    uart.write_str(" us); ");
    let forced = forced(slot);
    match f.verdict {
        Verdict::Expected => uart.write_str("drives "),
        Verdict::Other if forced => uart.write_str("REFUSED but forced from the console: drives "),
        Verdict::Other => uart.write_str("REFUSED: "),
        _ => uart.write_str("NOT CHECKED, carries on: "),
    }
    uart.write_str(spec.drives);
    if f.verdict.refuses() && !forced {
        uart.write_str("; ");
        uart.write_str(spec.without);
        match slot {
            Slot::View => uart.write_str("; `modules force view` drives it"),
            Slot::Link1 => uart.write_str("; `modules force link1` allows it"),
            Slot::Link2 => {}
        }
    }
    uart.write_line("");
}

fn write_record(uart: Uart, bytes: &[u8; RECORD_BYTES]) {
    match Record::parse(bytes) {
        Record::Module(info) => {
            uart.write_str("MI v1 ");
            uart.write_str(info.name().unwrap_or("?"));
            uart.write_str(" rev ");
            uart.write_str(info.revision().unwrap_or("?"));
            match info.serial() {
                Some("") => uart.write_str(", no serial"),
                Some(s) => {
                    uart.write_str(", serial ");
                    uart.write_str(s);
                }
                None => uart.write_str(", serial not text"),
            }
        }
        Record::Label(label) => {
            uart.write_str("older label \"");
            uart.write_str(label.text());
            uart.write_str("\": ");
            uart.write_str(label.name());
            uart.write_str(" rev ");
            uart.write_str(label.revision().unwrap_or("none"));
        }
        Record::Blank => uart.write_str("blank (all FF)"),
        Record::Unrecognised => {
            uart.write_str("unrecognised, starts");
            for b in &bytes[..16] {
                uart.write_str(" ");
                uart.write_hex8(*b);
            }
        }
    }
}
