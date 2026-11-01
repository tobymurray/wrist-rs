//! The DRV2625 haptic driver (`HARDWARE.md`, "Haptics"), held in reset by PF2 except while a
//! pattern or the probe runs.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_futures::select::{select, Either};
use embassy_stm32::i2c::Error;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::mutex::MutexGuard;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};
pub use wrist_haptic::Pattern;
use wrist_haptic::{reg, Ending, Pulse, Status, BRING_UP};

use crate::mmio::{read32, write32, GPIOF_BASE, GPIO_BSRR, GPIO_IDR, GPIO_MODER};
use crate::pmic::AsyncBus;
use crate::uart::{Lines, Uart};

const RST_PIN: u32 = 2;

/// PF2 as the pin reads, and whether it is still an output.
fn rst_state() -> (bool, bool) {
    let high = unsafe { read32(GPIOF_BASE + GPIO_IDR) } & (1 << RST_PIN) != 0;
    let output = unsafe { read32(GPIOF_BASE + GPIO_MODER) } >> (RST_PIN * 2) & 0b11 == 0b01;
    (high, output)
}

pub fn write_pin(uart: Uart) {
    let (high, output) = rst_state();
    uart.write_str("     HAPTIC_RST_N (PF2) ");
    uart.write_str(if high { "high" } else { "low" });
    uart.write_str(if output { ", output" } else { ", NOT an output" });
    uart.write_line(match (high, running()) {
        (false, _) => ": the haptic driver in reset between patterns",
        (true, Some(Request::Play(_))) => ": released for a pattern",
        (true, Some(Request::Probe)) => ": released for the probe",
        (true, None) => ": RELEASED, though nothing plays",
    });
}

const DRV2625: u8 = wrist_board::i2c::DRV2625_HAPTIC;
/// `t(on)` (SLOS879C 5.6), with margin.
const STARTUP_MS: u64 = 2;

/// How long a transfer waits for I2C1 before PF2 ends the pulse instead.
const BUS_WAIT: Duration = Duration::from_millis(2);
const BUSY: &str = "I2C1 busy";

static STOP: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Set by [`stop`] until the next pattern or probe begins.
static HALTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
enum Request {
    Play(Pattern),
    Probe,
}

/// What [`serve`] is running, from before PF2 rises until after it falls.
static RUNNING: Mutex<CriticalSectionRawMutex, Cell<Option<Request>>> = Mutex::new(Cell::new(None));

fn running() -> Option<Request> {
    RUNNING.lock(Cell::get)
}

fn is_playing() -> bool {
    matches!(running(), Some(Request::Play(_)))
}

/// A newer request replaces one not yet taken.
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
    /// Read once per bring-up, while the first pulse drives.
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

type Shared = MutexGuard<'static, CriticalSectionRawMutex, Option<AsyncBus>>;

/// I2C1 for one transfer, or PF2 low and a [`BUSY`] fault once it is held past
/// [`BUS_WAIT`].
async fn bus(step: &'static str) -> Result<Shared, Fault> {
    if HALTED.load(Ordering::Relaxed) {
        return Err(Fault { step, why: "stopped" });
    }
    with_timeout(BUS_WAIT, crate::pmic::BUS.lock()).await.map_err(|_| {
        hold_reset(true);
        Fault { step, why: BUSY }
    })
}

async fn write(reg: u8, value: u8, step: &'static str) -> Result<(), Fault> {
    let mut shared = bus(step).await?;
    let Some(bus) = shared.as_mut() else { return Err(Fault { step, why: "I2C1 not shared" }) };
    bus.write(DRV2625, &[reg, value]).await.map_err(|e| Fault { step, why: i2c_error(e) })
}

async fn read(reg: u8, step: &'static str) -> Result<u8, Fault> {
    let mut shared = bus(step).await?;
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

/// Out of reset, and configured for the motor.
async fn bring_up() -> Result<(), Fault> {
    record(|r| {
        r.id = None;
        r.readback = [None; BRING_UP.len()];
        r.vbat = None;
        r.status = None;
        r.at_s = u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX);
    });
    if HALTED.load(Ordering::Relaxed) {
        return Err(Fault { step: "release", why: "stopped" });
    }
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

/// One pulse, out of reset first if the chip is there; true if [`stop`] came first.
async fn pulse(p: Pulse, up: &mut bool) -> Result<bool, Fault> {
    if !*up {
        bring_up().await?;
        *up = true;
    }
    write(reg::RTP_INPUT, p.level, "level").await?;
    write(reg::GO, 1, "go").await?;
    let stopped = wait(p.on_ms).await;
    let mut ended = Ok(());
    if RECORD.lock(Cell::get).vbat.is_none() {
        ended = read(reg::VBAT, "VBAT").await.map(|v| record(|r| r.vbat = Some(v)));
    }
    if ended.is_ok() {
        ended = write(reg::GO, 0, "stop").await;
    }
    match ended {
        Err(fault) if fault.why == BUSY => *up = false,
        ended => ended?,
    }
    Ok(stopped || wait(p.off_ms).await)
}

/// Plays each pulse, bringing the chip back up after one a held bus cost.
async fn drive(pattern: Pattern) -> Result<Driven, Fault> {
    let begun = Instant::now();
    let mut up = false;
    let mut d = Driven { tried: 0, missed: 0, last: None };
    while let Some(p) = pattern.next_pulse(d.tried, ms_since(begun)) {
        d.tried += 1;
        match pulse(p, &mut up).await {
            Ok(false) => {}
            Ok(true) => return Ok(d),
            Err(fault) if fault.why == BUSY => {
                (up, d.missed, d.last) = (false, d.missed + 1, Some(fault));
                if wait(p.off_ms).await {
                    return Ok(d);
                }
            }
            Err(fault) => return Err(fault),
        }
    }
    if up {
        match read(reg::STATUS, "status").await {
            Ok(status) => record(|r| r.status = Some(status)),
            Err(fault) if fault.why == BUSY => {}
            Err(fault) => return Err(fault),
        }
    }
    Ok(d)
}

/// The pulses a pattern began, those a held bus skipped, and the last such fault.
struct Driven {
    tried: u32,
    missed: u32,
    last: Option<Fault>,
}

fn ms_since(t: Instant) -> u32 {
    u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// A stop from before this pattern or probe is not for it.
fn begin(request: Request) {
    STOP.reset();
    HALTED.store(false, Ordering::Relaxed);
    RUNNING.lock(|r| r.set(Some(request)));
}

/// The chip back in reset, whatever happened.
fn end() {
    hold_reset(true);
    RUNNING.lock(|r| r.set(None));
}

/// Plays `pattern` to its end or until [`stop`], puts the chip back in reset, and
/// reports how it ended.
async fn play(pattern: Pattern) {
    begin(Request::Play(pattern));
    let start = Instant::now();
    let outcome = drive(pattern).await;
    end();
    let ms = ms_since(start);
    let mut uart = Uart::best_effort().lines();
    uart.write_str("[haptic] ");
    uart.write_str(pattern.name());
    let halted = HALTED.load(Ordering::Relaxed);
    let (ending, missed, fault) = match outcome {
        Ok(d) => (Ending::of(halted, false, d.tried, d.missed), d.missed, d.last),
        Err(fault) => (Ending::of(halted, true, 0, 0), 0, Some(fault)),
    };
    if let (Ending::Failed, Some(fault)) = (ending, fault) {
        FAILED.fetch_add(1, Ordering::Relaxed);
        record(|r| r.last_error = Some((pattern, fault)));
        uart.write_str(" not played: ");
        uart.write_str(fault.step);
        uart.write_str(": ");
        uart.write_str(fault.why);
        uart.write_line("; PF2 low");
    } else {
        if ending == Ending::Stopped {
            STOPPED.fetch_add(1, Ordering::Relaxed);
            uart.write_str(" stopped after ");
        } else {
            PLAYED.fetch_add(1, Ordering::Relaxed);
            uart.write_str(" played in ");
        }
        uart.write_u32(ms);
        uart.write_str(" ms");
        if missed > 0 {
            uart.write_str(", ");
            uart.write_u32(missed);
            uart.write_str(" pulses missed: ");
            uart.write_str(BUSY);
        }
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
}

/// Plays `pattern` on the [`serve`] task and returns at once; a newer request replaces
/// one not yet taken.
pub fn start(pattern: Pattern) {
    REQUEST.signal(Request::Play(pattern));
}

/// Stops the motor at once from any task, by PF2 rather than the bus, and drops a
/// pattern not yet begun.
pub fn stop() {
    REQUEST.reset();
    HALTED.store(true, Ordering::Relaxed);
    hold_reset(true);
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
    begin(Request::Probe);
    let outcome = match bring_up().await {
        Ok(()) => read(reg::STATUS, "status").await.map(|s| record(|r| r.status = Some(s))),
        Err(f) => Err(f),
    };
    end();
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

/// Plays what [`start`] and the console ask for, so no caller waits out a pattern.
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
            if is_playing() {
                uart.write_line("[haptic] a pattern is playing; the probe follows it");
            }
            REQUEST.signal(Request::Probe);
        }
        "stop" => {
            uart.write_line(match running() {
                Some(Request::Play(_)) => "[haptic] stopping",
                Some(Request::Probe) => "[haptic] stopping the probe",
                None => "[haptic] nothing playing",
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
                start(pattern);
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
