//! The DRV2625 haptic driver on I2C1 and its ERM motor (`HARDWARE.md`, "Haptics").
//!
//! `HAPTIC_RST_N` (PF2), which park leaves an output driven low, holds the chip in shutdown
//! (about 0.1 uA, SLOS879C) whenever no pattern plays. A pattern releases it, configures the
//! registers the reset lost, drives the motor through real-time playback started and stopped
//! by the `GO` bit, and puts it back in reset whatever happened. Nothing here runs while idle:
//! the console's task waits on a signal and arms no timer.
//!
//! Without the `haptic` feature the API is the same and plays nothing, so a caller need not
//! know how the image was built.

pub use wrist_haptic::Pattern;

use crate::mmio::{read32, GPIOF_BASE, GPIO_IDR, GPIO_MODER};
use crate::uart::Uart;

const RST_PIN: u32 = 2;

/// PF2 as the pin reads, and whether it is still an output.
fn rst_state() -> (bool, bool) {
    let high = unsafe { read32(GPIOF_BASE + GPIO_IDR) } & (1 << RST_PIN) != 0;
    let output = unsafe { read32(GPIOF_BASE + GPIO_MODER) } >> (RST_PIN * 2) & 0b11 == 0b01;
    (high, output)
}

/// The park report's line.
pub fn write_pin(uart: Uart) {
    let (high, output) = rst_state();
    uart.write_str("     HAPTIC_RST_N (PF2) ");
    uart.write_str(if high { "high" } else { "low" });
    uart.write_str(if output { ", output" } else { ", NOT an output" });
    uart.write_line(match (high, cfg!(feature = "haptic")) {
        (false, true) => ": the haptic driver in reset between patterns",
        (false, false) => ": the haptic driver in reset (no `haptic` feature)",
        (true, true) => ": a pattern is playing",
        (true, false) => ": RELEASED, though this build never plays",
    });
}

/// Until the alarm calls it; the expectation then fails, and goes.
#[cfg(not(feature = "haptic"))]
#[expect(dead_code, clippy::unused_async, reason = "the API the alarm calls")]
pub async fn play(pattern: Pattern) {
    let mut uart = Uart::best_effort().lines();
    uart.write_str("[haptic] ");
    uart.write_str(pattern.name());
    uart.write_line(" not played: this build has no `haptic` feature");
}

#[cfg(not(feature = "haptic"))]
#[expect(dead_code, reason = "the API the alarm calls")]
pub fn stop() {}

#[cfg(not(feature = "haptic"))]
pub fn command(_args: &[u8], uart: Uart) {
    uart.write_line("[haptic] not in this build: cargo build --release --features haptic");
    write_pin(uart);
}

#[cfg(feature = "haptic")]
pub use driver::{command, serve};
#[cfg(feature = "haptic")]
#[expect(unused_imports, reason = "the API the alarm calls")]
pub use driver::{play, stop};

#[cfg(feature = "haptic")]
mod driver {
    use core::cell::Cell;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use embassy_futures::select::{select, Either};
    use embassy_stm32::i2c::Error;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::mutex::Mutex as AsyncMutex;
    use embassy_sync::signal::Signal;
    use embassy_time::{Instant, Timer};
    use wrist_haptic::{reg, Status, BRING_UP};

    use super::{write_pin, Pattern, RST_PIN};
    use crate::mmio::{write32, GPIOF_BASE, GPIO_BSRR};
    use crate::uart::{Lines, Uart};

    const DRV2625: u8 = wrist_board::i2c::DRV2625_HAPTIC;
    /// `t(on)`, startup from shutdown, is 1 ms (SLOS879C 5.6).
    const STARTUP_MS: u64 = 2;

    /// One pattern at a time; a second waits for the first to end.
    static PLAYER: AsyncMutex<CriticalSectionRawMutex, ()> = AsyncMutex::new(());
    static STOP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    static PLAYING: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy)]
    enum Request {
        Play(Pattern),
        Probe,
    }

    /// The console's; a newer request replaces one not yet taken.
    static REQUEST: Signal<CriticalSectionRawMutex, Request> = Signal::new();

    #[derive(Clone, Copy)]
    struct Fault {
        step: &'static str,
        why: &'static str,
    }

    /// What the last bring-up read; `None` is a read that failed or never ran.
    #[derive(Clone, Copy)]
    struct Record {
        id: Option<u8>,
        readback: [Option<u8>; BRING_UP.len()],
        /// Read before the first pulse stops, the only time it holds a value.
        vbat: Option<u8>,
        status: Option<u8>,
        last_error: Option<(Pattern, Fault)>,
        at_s: u32,
    }

    static RECORD: Mutex<CriticalSectionRawMutex, Cell<Record>> = Mutex::new(Cell::new(Record {
        id: None,
        readback: [None; BRING_UP.len()],
        vbat: None,
        status: None,
        last_error: None,
        at_s: 0,
    }));
    static PLAYED: AtomicU32 = AtomicU32::new(0);
    static STOPPED: AtomicU32 = AtomicU32::new(0);
    static FAILED: AtomicU32 = AtomicU32::new(0);

    fn record(f: impl FnOnce(&mut Record)) {
        RECORD.lock(|r| {
            let mut v = r.get();
            f(&mut v);
            r.set(v);
        });
    }

    fn hold_reset(held: bool) {
        let bit = if held { 1 << (RST_PIN + 16) } else { 1 << RST_PIN };
        unsafe { write32(GPIOF_BASE + GPIO_BSRR, bit) };
    }

    fn i2c_error(e: Error) -> &'static str {
        match e {
            Error::Bus => "bus error",
            Error::Arbitration => "arbitration lost",
            Error::Nack => "no ACK",
            Error::Timeout => "timed out",
            Error::Crc => "CRC mismatch",
            Error::Overrun => "overrun",
            Error::ZeroLengthTransfer => "zero-length transfer",
        }
    }

    /// The bus is held a transfer at a time, so the display's readings go on during a pattern.
    async fn write(reg: u8, value: u8, step: &'static str) -> Result<(), Fault> {
        let mut shared = crate::pmic::BUS.lock().await;
        let Some(bus) = shared.as_mut() else { return Err(Fault { step, why: "I2C1 not shared" }) };
        bus.write(DRV2625, &[reg, value]).await.map_err(|e| Fault { step, why: i2c_error(e) })
    }

    async fn read(reg: u8, step: &'static str) -> Result<u8, Fault> {
        let mut shared = crate::pmic::BUS.lock().await;
        let Some(bus) = shared.as_mut() else { return Err(Fault { step, why: "I2C1 not shared" }) };
        let mut b = [0];
        bus.write_read(DRV2625, &[reg], &mut b).await.map_err(|e| Fault { step, why: i2c_error(e) })?;
        Ok(b[0])
    }

    /// True if [`stop`] came first.
    async fn wait(ms: u16) -> bool {
        if ms == 0 {
            return STOP.signaled();
        }
        matches!(select(Timer::after_millis(u64::from(ms)), STOP.wait()).await, Either::Second(()))
    }

    /// Out of reset, and configured for the motor: the reset lost every register.
    async fn bring_up() -> Result<(), Fault> {
        record(|r| {
            r.id = None;
            r.readback = [None; BRING_UP.len()];
            r.vbat = None;
            r.status = None;
            r.at_s = u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX);
        });
        hold_reset(false);
        Timer::after_millis(STARTUP_MS).await;
        let id = read(reg::CHIP_ID, "chip ID").await?;
        record(|r| r.id = Some(id));
        if !wrist_haptic::is_drv2625(id) {
            return Err(Fault { step: "chip ID", why: "not a DRV2625" });
        }
        for &(reg, value) in &BRING_UP {
            write(reg, value, "configure").await?;
        }
        for (i, &(reg, value)) in BRING_UP.iter().enumerate() {
            let got = read(reg, "read back").await?;
            record(|r| r.readback[i] = Some(got));
            if got != value {
                return Err(Fault { step: "read back", why: "a register differs from what was written" });
            }
        }
        Ok(())
    }

    /// True if stopped before the end.
    async fn drive(pattern: Pattern) -> Result<bool, Fault> {
        bring_up().await?;
        for (i, pulse) in pattern.pulses().iter().enumerate() {
            write(reg::RTP_INPUT, pulse.level, "level").await?;
            write(reg::GO, 1, "go").await?;
            let stopped = wait(pulse.on_ms).await;
            if i == 0 {
                let vbat = read(reg::VBAT, "VBAT").await?;
                record(|r| r.vbat = Some(vbat));
            }
            write(reg::GO, 0, "stop").await?;
            if stopped || wait(pulse.off_ms).await {
                return Ok(true);
            }
        }
        let status = read(reg::STATUS, "status").await?;
        record(|r| r.status = Some(status));
        Ok(false)
    }

    /// Plays `pattern` to its end or until [`stop`], then puts the chip back in reset. Never
    /// panics: an I2C error ends it with a `[haptic]` line, and `haptic` shows the last one.
    pub async fn play(pattern: Pattern) {
        let _one = PLAYER.lock().await;
        // A stop from before this pattern is not for it.
        STOP.reset();
        PLAYING.store(true, Ordering::Relaxed);
        let start = Instant::now();
        let outcome = drive(pattern).await;
        hold_reset(true);
        PLAYING.store(false, Ordering::Relaxed);
        let ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
        let mut uart = Uart::best_effort().lines();
        uart.write_str("[haptic] ");
        uart.write_str(pattern.name());
        match outcome {
            Ok(stopped) => {
                if stopped {
                    STOPPED.fetch_add(1, Ordering::Relaxed);
                    uart.write_str(" stopped after ");
                } else {
                    PLAYED.fetch_add(1, Ordering::Relaxed);
                    uart.write_str(" played in ");
                }
                uart.write_u32(ms);
                uart.write_str(" ms");
                let r = RECORD.lock(Cell::get);
                if let Some(v) = r.vbat {
                    uart.write_str(", VDD ");
                    uart.write_u32(wrist_haptic::vbat_mv(v));
                    uart.write_str(" mV");
                }
                if let Some(s) = r.status {
                    write_status(&mut uart, s);
                }
                uart.write_line("; PF2 low");
            }
            Err(fault) => {
                FAILED.fetch_add(1, Ordering::Relaxed);
                record(|r| r.last_error = Some((pattern, fault)));
                uart.write_str(" not played: ");
                uart.write_str(fault.step);
                uart.write_str(": ");
                uart.write_str(fault.why);
                uart.write_line("; PF2 low");
            }
        }
    }

    /// Ends a playing pattern within a few milliseconds, and drops one the console asked for
    /// that has not started; nothing if none plays. A `play` already waiting for the one
    /// playing still plays.
    pub fn stop() {
        REQUEST.reset();
        STOP.signal(());
    }

    fn write_status(uart: &mut Lines, s: u8) {
        uart.write_str(", STATUS ");
        uart.write_hex8(s);
        for name in Status(s).faults() {
            uart.write_str(" ");
            uart.write_str(name);
        }
    }

    /// Out of reset long enough to read the chip and its configuration back.
    async fn probe() {
        let _one = PLAYER.lock().await;
        let outcome = match bring_up().await {
            Ok(()) => read(reg::STATUS, "status").await.map(|s| record(|r| r.status = Some(s))),
            Err(f) => Err(f),
        };
        hold_reset(true);
        let mut uart = Uart::best_effort().lines();
        uart.write_str("[haptic] probe: ");
        let r = RECORD.lock(Cell::get);
        match r.id {
            Some(id) => {
                uart.write_str("CHIP_ID ");
                uart.write_hex8(id);
                uart.write_str(if wrist_haptic::is_drv2625(id) { " (DRV2625 rev " } else { " (NOT a DRV2625, rev " });
                uart.write_u32(u32::from(wrist_haptic::revision(id)));
                uart.write_str(")");
            }
            None => uart.write_str("CHIP_ID --"),
        }
        for (&(reg, wrote), got) in BRING_UP.iter().zip(r.readback) {
            uart.write_str(", ");
            uart.write_hex8(reg);
            uart.write_str("=");
            match got {
                Some(v) => uart.write_hex8(v),
                None => uart.write_str("--"),
            }
            if got.is_some_and(|v| v != wrote) {
                uart.write_str("(wrote ");
                uart.write_hex8(wrote);
                uart.write_str(")");
            }
        }
        if let Some(s) = r.status {
            write_status(&mut uart, s);
        }
        match outcome {
            Ok(()) => uart.write_line("; PF2 low"),
            Err(fault) => {
                uart.write_str("; failed at ");
                uart.write_str(fault.step);
                uart.write_str(": ");
                uart.write_str(fault.why);
                uart.write_line("; PF2 low");
            }
        }
    }

    /// Plays what the console asks for, so the console never waits out a pattern.
    #[embassy_executor::task]
    pub async fn serve() -> ! {
        loop {
            match REQUEST.wait().await {
                Request::Play(pattern) => play(pattern).await,
                Request::Probe => probe().await,
            }
        }
    }

    pub fn command(args: &[u8], uart: Uart) {
        match core::str::from_utf8(args).unwrap_or("").trim() {
            "" => {
                write_summary(uart);
                write_pin(uart);
                if PLAYING.load(Ordering::Relaxed) {
                    uart.write_line("[haptic] a pattern is playing; the probe follows it");
                }
                REQUEST.signal(Request::Probe);
            }
            "stop" => {
                uart.write_line(if PLAYING.load(Ordering::Relaxed) {
                    "[haptic] stopping"
                } else {
                    "[haptic] nothing playing"
                });
                stop();
            }
            word => match Pattern::parse(word) {
                Some(pattern) => {
                    let mut lines = uart.lines();
                    lines.write_str("[haptic] ");
                    lines.write_str(pattern.name());
                    lines.write_str(" queued, ");
                    lines.write_u32(pattern.total_ms());
                    lines.write_line(" ms");
                    REQUEST.signal(Request::Play(pattern));
                }
                None => uart.write_line("[haptic] usage: haptic [tap|alarm|stop]"),
            },
        }
    }

    fn write_summary(uart: Uart) {
        let r = RECORD.lock(Cell::get);
        let mut lines = uart.lines();
        lines.write_str("[haptic] played ");
        lines.write_u32(PLAYED.load(Ordering::Relaxed));
        lines.write_str(", stopped ");
        lines.write_u32(STOPPED.load(Ordering::Relaxed));
        lines.write_str(", failed ");
        lines.write_u32(FAILED.load(Ordering::Relaxed));
        lines.write_str("; full scale ");
        lines.write_u32(wrist_haptic::od_clamp_mv(wrist_haptic::od_clamp_code(wrist_haptic::RATED_MV)));
        lines.write_str(" mV, ERM open loop; last bring-up at ");
        lines.write_u32(r.at_s);
        lines.write_line(" s");
        lines.write_str("[haptic] last error: ");
        match r.last_error {
            None => lines.write_line("none"),
            Some((pattern, fault)) => {
                lines.write_str(pattern.name());
                lines.write_str(", ");
                lines.write_str(fault.step);
                lines.write_str(": ");
                lines.write_line(fault.why);
            }
        }
    }
}
