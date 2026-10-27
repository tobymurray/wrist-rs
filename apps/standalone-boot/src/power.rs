//! Whether a cable is in (`HARDWARE.md`, "Power-source detection"), and what a stretch on battery
//! cost.

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::watch::{Receiver, Watch};
use wrist_power::{BatteryRun, StopInput};
use embassy_time::{Duration, Instant};

use crate::mmio::{read32, write32, GPIOB_BASE, GPIOG_BASE, GPIO_BSRR, GPIO_IDR};
use crate::{pmic, uart::Uart};

const PD_PORT_STATUS_1: u8 = 0x0E;
const PD_ATTACH: u8 = 1 << 0;
const PMIC_CHG_STATUS_1: u8 = 0x19;
/// PCA9420 datasheet Rev 4.2, Tables 11 to 15: write 1 to clear.
const PMIC_SUB_INT: [u8; 3] = [0x02, 0x04, 0x06];
const PMIC_SUB_INT0_MASK: u8 = 0x03;
const PMIC_IN_PWR: u8 = 1 << 0;
const DAM_DETECT_PIN: u32 = 15;
const PWR_INT_L_PIN: u32 = 7;
/// While something holds `PWR_INT_L` low.
const RETRY: Duration = Duration::from_secs(1);

static INTERRUPTS: AtomicU32 = AtomicU32::new(0);
static SAMPLES: AtomicU32 = AtomicU32::new(0);
static MASK: AtomicU32 = AtomicU32::new(u32::MAX);

const UNKNOWN: u8 = 0;
const BATTERY: u8 = 1;
const EXTERNAL: u8 = 2;

static INPUT: AtomicU8 = AtomicU8::new(UNKNOWN);

/// The heartbeat, the USB console and the radio each take one.
static CHANGES: Watch<CriticalSectionRawMutex, u8, 3> = Watch::new();

/// The stretch on battery in progress, and the last one finished.
type Runs = (Option<BatteryRun>, Option<BatteryRun>);
static RUNS: Mutex<CriticalSectionRawMutex, RefCell<Runs>> = Mutex::new(RefCell::new((None, None)));

static STOP_INPUT: Mutex<CriticalSectionRawMutex, RefCell<StopInput>> = Mutex::new(RefCell::new(StopInput::new()));

/// `None` once every receiver is handed out.
pub type Changes = Option<Receiver<'static, CriticalSectionRawMutex, u8, 3>>;

/// `None` while neither chip has answered.
pub fn external_input() -> Option<bool> {
    match INPUT.load(Ordering::Relaxed) {
        BATTERY => Some(false),
        EXTERNAL => Some(true),
        _ => None,
    }
}

pub fn on_battery() -> bool {
    external_input() == Some(false)
}

/// One per task, taken at its start.
pub fn changes() -> Changes {
    CHANGES.receiver()
}

pub async fn wait_while_on_battery(changes: &mut Changes) {
    while on_battery() {
        match changes {
            Some(rx) => {
                rx.changed().await;
            }
            None => embassy_time::Timer::after_secs(5).await,
        }
    }
}

/// At boot, before any awaited transfer: the PMIC pulls `PWR_INT_L` low when input power
/// comes or goes. `SUB_INT0_MASK` as read back, or `None` if the PMIC did not answer.
pub fn arm_input_interrupt(bus: &mut pmic::AsyncBus) -> Option<u8> {
    let pmic = wrist_board::i2c::PCA9420_PMIC;
    let mut mask = [0];
    bus.blocking_write_read(pmic, &[PMIC_SUB_INT0_MASK], &mut mask).ok()?;
    bus.blocking_write(pmic, &[PMIC_SUB_INT0_MASK, mask[0] & !PMIC_IN_PWR]).ok()?;
    bus.blocking_write_read(pmic, &[PMIC_SUB_INT0_MASK], &mut mask).ok()?;
    MASK.store(u32::from(mask[0]), Ordering::Relaxed);
    Some(mask[0])
}

/// The input, sampled at boot, on the PMIC's interrupt, and with each new minute.
pub struct Sampler {
    last: Option<Instant>,
    line_was_low: bool,
}

impl Sampler {
    pub const fn new() -> Self {
        Self { last: None, line_was_low: false }
    }

    pub fn due(&self, line_low: bool, new_minute: bool) -> bool {
        self.last.is_none() || new_minute || (line_low && Instant::now() >= self.retry_at())
    }

    pub fn retry_at(&self) -> Instant {
        self.last.map_or(Instant::from_ticks(0), |at| at + RETRY)
    }

    pub async fn service(&mut self, bus: &mut pmic::AsyncBus) {
        self.last = Some(Instant::now());
        SAMPLES.fetch_add(1, Ordering::Relaxed);
        // Fallen since the last sample left it.
        let changed = line_low() && !self.line_was_low;
        // Cleared before the input is read, so a change after the read raises the line again.
        for reg in PMIC_SUB_INT {
            let mut flags = [0];
            let read = bus.write_read(wrist_board::i2c::PCA9420_PMIC, &[reg], &mut flags).await;
            if read.is_ok() && flags[0] != 0 {
                if reg == PMIC_SUB_INT[0] && flags[0] & PMIC_IN_PWR != 0 {
                    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
                }
                let _ = bus.write(wrist_board::i2c::PCA9420_PMIC, &[reg, flags[0]]).await;
            }
        }
        let mut port = [0];
        let attached = bus
            .write_read(wrist_board::i2c::STUSB4500_USB_PD, &[PD_PORT_STATUS_1], &mut port)
            .await
            .ok()
            .map(|()| port[0] & PD_ATTACH != 0);
        let mut status = [0];
        let charger = bus
            .write_read(wrist_board::i2c::PCA9420_PMIC, &[PMIC_CHG_STATUS_1], &mut status)
            .await
            .ok()
            .map(|()| status[0]);
        let usb = wrist_power::external_power(attached, charger);
        self.line_was_low = line_low();
        let now = Instant::now().as_millis();
        let host = crate::console::host_active();
        crate::sleep::input(STOP_INPUT.lock(|s| s.borrow_mut().sample(now, usb, changed, host)));
        let code = match usb {
            None => UNKNOWN,
            Some(false) => BATTERY,
            Some(true) => EXTERNAL,
        };
        if INPUT.swap(code, Ordering::Relaxed) != code {
            // `HARDWARE.md`, "What the image turns off".
            let bsrr = if code == BATTERY { 1 << (DAM_DETECT_PIN + 16) } else { 1 << DAM_DETECT_PIN };
            unsafe { write32(GPIOB_BASE + GPIO_BSRR, bsrr) };
            let now = Instant::now().as_millis();
            RUNS.lock(|r| {
                let mut r = r.borrow_mut();
                if code == BATTERY {
                    r.0 = Some(BatteryRun::new(now));
                } else if let Some(run) = r.0.take() {
                    r.1 = Some(run);
                }
            });
            CHANGES.sender().send(code);
        }
    }
}

pub fn note_reading(s: &pmic::Snapshot) {
    let now = Instant::now().as_millis();
    RUNS.lock(|r| {
        if let Some(run) = r.borrow_mut().0.as_mut() {
            run.reading(
                now,
                s.avg_current_raw.map(pmic::current_ua),
                s.soc_raw.map(|raw| u16::try_from(pmic::soc_tenths(raw)).unwrap_or(1000)),
                s.tte_raw.and_then(wrist_power::tte_seconds),
                s.status2.map(wrist_power::hibernating),
            );
        }
    });
}

pub fn command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    if let Some(rest) = text.strip_prefix("sleep") {
        return crate::sleep::command(rest, uart);
    }
    if !text.is_empty() {
        return uart.write_line("[power] usage: power | power sleep [on|off|status|clear-crash]");
    }
    uart.write_str("[power] input ");
    uart.write_line(match external_input() {
        Some(true) => "external (USB)",
        Some(false) => "battery",
        None => "unknown (neither the PMIC nor the USB-PD controller answered)",
    });
    let stop = STOP_INPUT.lock(|s| *s.borrow());
    uart.write_str("[power] unanswered input reads in a row ");
    uart.write_u32(stop.misses());
    uart.write_line(if stop.assumed_battery() {
        "; Stop treats the input as battery (no USB host on the link)"
    } else {
        "; Stop follows the input"
    });
    uart.write_str("[power] PWR_INT_L ");
    uart.write_str(if line_low() { "low" } else { "high" });
    uart.write_str(", input interrupts ");
    uart.write_u32(INTERRUPTS.load(Ordering::Relaxed));
    uart.write_str(", samples ");
    uart.write_u32(SAMPLES.load(Ordering::Relaxed));
    uart.write_str(", SUB_INT0_MASK ");
    match u8::try_from(MASK.load(Ordering::Relaxed)) {
        Ok(mask) => uart.write_hex8(mask),
        Err(_) => uart.write_str("not set"),
    }
    uart.write_line("");
    match RUNS.lock(|r| r.borrow().1) {
        None => uart.write_line("[power] no stretch on battery finished since boot"),
        Some(run) => write_run(uart, &run),
    }
    crate::write_buttons(uart);
    crate::sleep::command("", uart);
}

fn write_run(uart: Uart, run: &BatteryRun) {
    let s = run.seconds();
    uart.write_str("[power] last on battery: ");
    write_duration(uart, s);
    uart.write_str(", mean ");
    match run.mean_ua() {
        Some(ua) => {
            if ua < 0 {
                uart.write_str("-");
            }
            uart.write_u32(ua.unsigned_abs());
            uart.write_str(" uA over ");
            uart.write_u32(run.samples());
            uart.write_str(" readings");
        }
        None => uart.write_str("-- (no reading)"),
    }
    if let (Some(a), Some(b)) = (run.first_soc_x10, run.last_soc_x10) {
        uart.write_str(", charge ");
        write_tenths(uart, a);
        uart.write_str(" % to ");
        write_tenths(uart, b);
        uart.write_str(" %");
    }
    uart.write_str(", the gauge's time to empty ");
    match run.last_tte_s {
        Some(t) => write_duration(uart, u64::from(t)),
        None => uart.write_str("unknown"),
    }
    uart.write_str(", the gauge hibernating at ");
    uart.write_u32(run.hibernating);
    uart.write_str(" of ");
    uart.write_u32(run.hib_known);
    uart.write_str(" readings");
    uart.write_line("");
}

fn write_duration(uart: Uart, s: u64) {
    let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
    let part = |v: u64, unit: &str| {
        uart.write_u32(u32::try_from(v).unwrap_or(u32::MAX));
        uart.write_str(unit);
    };
    if d > 0 {
        part(d, " d ");
        part(h, " h");
    } else if h > 0 {
        part(h, " h ");
        part(m, " min");
    } else {
        part(m, " min");
    }
}

fn write_tenths(uart: Uart, v: u16) {
    uart.write_u32(u32::from(v / 10));
    uart.write_str(".");
    uart.write_u32(u32::from(v % 10));
}

fn line_low() -> bool {
    let idr = unsafe { read32(GPIOG_BASE + GPIO_IDR) };
    idr & (1 << PWR_INT_L_PIN) == 0
}
