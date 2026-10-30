//! Stop2 on battery, otherwise no Stop (`HARDWARE.md`, "Stop modes"), and what Stop did.
use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use embassy_stm32::low_power::{set_stop_observer, StopEvent};
use embassy_stm32::rcc::{StopMode, WakeGuard};
use embassy_sync::blocking_mutex::{raw::CriticalSectionRawMutex, Mutex};
use embassy_time::Instant;
use crate::uart::Uart;

const UNKNOWN: u8 = 0;
const BATTERY: u8 = 1;
const EXTERNAL: u8 = 2;
const LPMS_STOP2: u8 = 2;

static ENABLED: AtomicBool = AtomicBool::new(true);
static INPUT: AtomicU8 = AtomicU8::new(UNKNOWN);
static BLOCK: Mutex<CriticalSectionRawMutex, RefCell<Option<WakeGuard>>> = Mutex::new(RefCell::new(None));
static STOP1: AtomicU32 = AtomicU32::new(0);
static STOP2: AtomicU32 = AtomicU32::new(0);
static STOP_TICKS: AtomicU32 = AtomicU32::new(0);
static ENTER_TICKS: AtomicU32 = AtomicU32::new(0);
static WAKE_IRQ: AtomicU32 = AtomicU32::new(0);
static EXIT_TICKS: AtomicU32 = AtomicU32::new(0);
static RESTORE_TICKS: AtomicU32 = AtomicU32::new(0);
static SLEEPS: AtomicU32 = AtomicU32::new(0);
static SLEEP_FROM: AtomicU32 = AtomicU32::new(0);
static SLEEP_TICKS: AtomicU32 = AtomicU32::new(0);
/// The last Sleep's Stop refcounts, `stop1_holds << 16 | stop2_holds`.
static HOLDS: AtomicU32 = AtomicU32::new(0);

fn enabled() -> bool { ENABLED.load(Ordering::Relaxed) }
fn set_enabled(on: bool) { ENABLED.store(on, Ordering::Relaxed); policy(); }
pub fn input(usb: Option<bool>) {
    INPUT.store(match usb { None => UNKNOWN, Some(false) => BATTERY, Some(true) => EXTERNAL }, Ordering::Relaxed);
    policy();
}
fn policy() {
    BLOCK.lock(|b| {
        let mut b = b.borrow_mut();
        if !enabled() || INPUT.load(Ordering::Relaxed) != BATTERY {
            if b.is_none() { *b = Some(WakeGuard::new(StopMode::Stop1)); }
        } else { *b = None; }
    });
}
pub fn init() { set_stop_observer(on_stop); policy(); }

/// Runs inside the HAL's critical section around Stop (`vendor/embassy-stm32/WRIST-PATCHES.md`):
/// counters only.
#[allow(clippy::cast_possible_truncation)]
fn on_stop(event: StopEvent) {
    let now = Instant::now().as_ticks() as u32;
    let since = |from: &AtomicU32| now.wrapping_sub(from.load(Ordering::Relaxed));
    match event {
        StopEvent::Enter => ENTER_TICKS.store(now, Ordering::Relaxed),
        StopEvent::Exit { lpms } => {
            if lpms == LPMS_STOP2 { STOP2.fetch_add(1, Ordering::Relaxed); }
            else { STOP1.fetch_add(1, Ordering::Relaxed); }
            STOP_TICKS.fetch_add(since(&ENTER_TICKS), Ordering::Relaxed);
            EXIT_TICKS.store(now, Ordering::Relaxed);
            // ICSR.VECTPENDING
            WAKE_IRQ.store(unsafe { crate::mmio::read32(crate::mmio::SCB_ICSR) } >> 12 & 0x1FF, Ordering::Relaxed);
        }
        StopEvent::Restored => { RESTORE_TICKS.fetch_add(since(&EXIT_TICKS), Ordering::Relaxed); }
        StopEvent::Sleep { stop1_holds, stop2_holds } => {
            SLEEPS.fetch_add(1, Ordering::Relaxed);
            SLEEP_FROM.store(now, Ordering::Relaxed);
            HOLDS.store(stop1_holds.min(0xFFFF) << 16 | stop2_holds.min(0xFFFF), Ordering::Relaxed);
        }
        StopEvent::Woke => { SLEEP_TICKS.fetch_add(since(&SLEEP_FROM), Ordering::Relaxed); }
    }
}

fn ticks_ms(ticks: &AtomicU32) -> u32 {
    u32::try_from(u64::from(ticks.load(Ordering::Relaxed)) * 1000 / embassy_time::TICK_HZ).unwrap_or(u32::MAX)
}
pub fn command(text: &str, uart: Uart) {
    match text.trim() {
        "clear-crash" => crate::crash::clear(),
        "on" | "auto" => set_enabled(true),
        "off" => set_enabled(false),
        "" | "status" => {},
        _ => return uart.write_line("[sleep] usage: power sleep [on|off|status|clear-crash]"),
    }
    crate::crash::report(uart);
    uart.write_str("[sleep] policy="); uart.write_str(if enabled() { "auto" } else { "off" });
    uart.write_str(" input="); uart.write_str(match INPUT.load(Ordering::Relaxed) { BATTERY => "battery", EXTERNAL => "external", _ => "unknown" });
    uart.write_str(" stop1_wakes="); uart.write_u32(STOP1.load(Ordering::Relaxed));
    uart.write_str(" stop2_wakes="); uart.write_u32(STOP2.load(Ordering::Relaxed));
    uart.write_str(" stop_ms="); uart.write_u32(ticks_ms(&STOP_TICKS));
    uart.write_str(" restore_ms="); uart.write_u32(ticks_ms(&RESTORE_TICKS));
    uart.write_str(" sleeps="); uart.write_u32(SLEEPS.load(Ordering::Relaxed));
    uart.write_str(" sleep_ms="); uart.write_u32(ticks_ms(&SLEEP_TICKS));
    let holds = HOLDS.load(Ordering::Relaxed);
    uart.write_str(" last_sleep_holds="); uart.write_u32(holds >> 16); uart.write_str("/"); uart.write_u32(holds & 0xFFFF);
    uart.write_str(" last_wake_vector="); uart.write_u32(WAKE_IRQ.load(Ordering::Relaxed));
    uart.write_line("; USB, unknown input, active UART/DMA/timers inhibit Stop2");
    write_time_driver(uart);
}

/// The core's clock whenever the time driver runs (`main.rs`, "[9]").
const CORE_MHZ: u32 = 48;

fn write_cycles(uart: Uart, label: &str, waits: u32, cycles: u32, max: u32) {
    uart.write_str(label); uart.write_u32(waits);
    uart.write_str(", total "); uart.write_u32(cycles / CORE_MHZ);
    uart.write_str(" us, mean "); uart.write_u32(cycles.checked_div(waits).unwrap_or(0) * 10 / CORE_MHZ / 10);
    uart.write_str("."); uart.write_u32(cycles.checked_div(waits).unwrap_or(0) * 10 / CORE_MHZ % 10);
    uart.write_str(" us, max "); uart.write_u32(max / CORE_MHZ); uart.write_str(" us");
}

/// The LPTIM4 time driver's counters since boot (`vendor/embassy-stm32/WRIST-PATCHES.md`).
#[allow(clippy::cast_possible_truncation)]
fn write_time_driver(uart: Uart) {
    let s = embassy_stm32::low_power::time_driver_stats::snapshot();
    let ms = |ticks: u32| u32::try_from(u64::from(ticks) * 1000 / embassy_time::TICK_HZ).unwrap_or(u32::MAX);
    uart.write_str("[lptim] DIER writes "); uart.write_u32(s.dier_writes);
    uart.write_str(", skipped "); uart.write_u32(s.dier_skipped);
    uart.write_str(", before the last DIEROK "); uart.write_u32(s.dier_early);
    uart.write_str(", read back wrong "); uart.write_u32(s.dier_mismatch);
    uart.write_str("; Stop entered with one unacknowledged "); uart.write_u32(s.stop_dier_pending);
    uart.write_str("; overflows finding the alarm overdue "); uart.write_u32(s.overdue_at_overflow);
    uart.write_str(", with CCIE clear "); uart.write_u32(s.overdue_ccie_clear);
    uart.write_line("");
    write_cycles(uart, "[lptim] CMPOK waits ", s.cmpok_waits, s.cmpok_cycles, s.cmpok_max_cycles);
    uart.write_str(", set at the first poll "); uart.write_u32(s.cmpok_first_poll);
    uart.write_str(", unchanged and skipped "); uart.write_u32(s.compare_skipped);
    write_cycles(uart, "; DIEROK waits ", s.dierok_waits, s.dierok_cycles, s.dierok_max_cycles);
    uart.write_line("");
    uart.write_str("[lptim] waits over 2 ms: DIEROK "); uart.write_u32(s.dierok_timeouts);
    uart.write_str(", CMPOK "); uart.write_u32(s.cmpok_timeouts);
    uart.write_str("; given up after writing again: DIEROK "); uart.write_u32(s.dierok_given_up);
    uart.write_str(", CMPOK "); uart.write_u32(s.cmpok_given_up);
    uart.write_line("");
    if s.dierok_timeouts + s.cmpok_timeouts != 0 {
        let t = s.last_timeout;
        uart.write_str(if t.dier { "[lptim] the last: DIER" } else { "[lptim] the last: CCR" });
        uart.write_str(" written "); uart.write_hex32(t.written);
        uart.write_str(", read back "); uart.write_hex32(t.read_back);
        uart.write_str(", CR "); uart.write_hex32(t.cr);
        uart.write_str(", ISR "); uart.write_hex32(t.isr);
        uart.write_str(", RCC_BDCR "); uart.write_hex32(t.bdcr);
        uart.write_str("; the counter moved "); uart.write_u32(u32::from(t.counter_ticks));
        uart.write_line(" ticks while the second write was waited on");
    }
    uart.write_str("[lptim] alarms handled over 5 ms late "); uart.write_u32(s.late_alarms);
    uart.write_str(", the latest by at most "); uart.write_u32(ms(s.late_max_ticks));
    uart.write_line(" ms");
    if s.late.iter().any(|l| l.late_ticks != 0) {
        uart.write_str("[lptim] the last (uptime ms: late ms, alarm's low 16 bits, flags):");
        for l in s.late.iter().filter(|l| l.late_ticks != 0) {
            uart.write_str(" "); uart.write_u32(ms(l.at_ticks));
            uart.write_str(":"); uart.write_u32(ms(l.late_ticks));
            uart.write_str(","); uart.write_hex32(u32::from(l.alarm_lo));
            uart.write_str(","); uart.write_hex32(u32::from(l.flags));
        }
        uart.write_line("");
        uart.write_line("[lptim] flags: 1 far, 2 CCIE written before DIEROK, 4 Stop before its DIEROK, 8 CMPOK at first poll, 10 at an overflow, 20 CCIE read clear, 40 CCR wrong, 80 found by schedule_wake, 100 its arming turned CCIE on");
    }
}
