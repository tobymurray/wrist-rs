//! The BMI270's step counter. The chip keeps its state across the MCU's resets and updates
//! (`HARDWARE.md`, "Modules and slots"), so a boot adopts a chip already counting and loads only
//! one that is not. Today's steps come from its count through a [`Ledger`] kept in backup
//! registers. `imu record` keeps the raw accelerometer, with the chip's count, for scoring a walk.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_futures::select::{select3, Either3};
use embassy_futures::yield_now;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::{Async, Blocking};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use wrist_imu::record::{self, Recording};
use wrist_steps::Ledger;
use wrist_imu::{reg, Activity, Found, Outputs, ADDR, CHIP_ID, OUT_LEN, OUT_PAGE, PAGE_LEN, STEP_PAGE};

use crate::uart::Uart;

type Bus = I2c<'static, Blocking, Master>;

const STARTING: u8 = 0;
const ADOPTED: u8 = 1;
const LOADED: u8 = 2;
const FAILED: u8 = 3;

static STATE: AtomicU8 = AtomicU8::new(STARTING);
static STEPS: AtomicU32 = AtomicU32::new(0);
static ACTIVITY: AtomicU8 = AtomicU8::new(Activity::Unknown as u8);
static REPORT: AtomicBool = AtomicBool::new(false);
/// The task runs only when the GNSS slot holds the module that carries the IMU.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// `INTERNAL_STATUS`, `PWR_CTRL`, `ACC_CONF` and `PWR_CONF` as the boot found them.
static FOUND: AtomicU32 = AtomicU32::new(u32::MAX);
static FOUND_STEPS_ON: AtomicBool = AtomicBool::new(false);
static READ: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static DONE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static TODAY: AtomicU32 = AtomicU32::new(0);
/// Set by the first reading, so the face never shows a count not yet read.
static TODAY_KNOWN: AtomicBool = AtomicBool::new(false);
static RAISES: AtomicU32 = AtomicU32::new(0);
/// Set by `imu raise on|off`, for the task to apply.
static RAISE_CHANGED: AtomicBool = AtomicBool::new(false);
static YESTERDAY: AtomicU32 = AtomicU32::new(u32::MAX);

const REC_STATUS: u8 = 1;
const REC_START: u8 = 2;
const REC_STOP: u8 = 3;
const REC_DUMP: u8 = 4;
const REC_CLEAR: u8 = 5;
/// `imu record …`, for the task to carry out.
static REC_REQUEST: AtomicU8 = AtomicU8::new(0);
/// The FIFO holds 6.8 s at 50 Hz.
const DRAIN_EVERY: Duration = Duration::from_secs(4);
const SAMPLE_HZ: u32 = 50;

/// Today's steps, once the chip is counting.
pub fn today() -> Option<u32> {
    TODAY_KNOWN.load(Ordering::Relaxed).then(|| TODAY.load(Ordering::Relaxed))
}

pub fn counting() -> bool {
    matches!(STATE.load(Ordering::Relaxed), ADOPTED | LOADED)
}

/// Reads the count and waits a little for it, so the face drawn next shows it.
pub async fn read_now() {
    DONE.reset();
    READ.signal(());
    let _ = with_timeout(Duration::from_millis(50), DONE.wait()).await;
}

/// Reads the count now, and with `report` prints it.
pub fn request_read(report: bool) {
    if report {
        REPORT.store(true, Ordering::Relaxed);
    }
    READ.signal(());
}

pub fn command(args: &[u8], uart: Uart) {
    if !RUNNING.load(Ordering::Relaxed) {
        uart.write_line("[imu] no IMU: the GNSS slot holds another module (`modules`)");
        return;
    }
    let rec = match core::str::from_utf8(args).unwrap_or("").trim() {
        "record" => REC_STATUS,
        "record start" => REC_START,
        "record stop" => REC_STOP,
        "record dump" => REC_DUMP,
        "record clear" => REC_CLEAR,
        _ => 0,
    };
    if rec != 0 {
        REC_REQUEST.store(rec, Ordering::Relaxed);
        READ.signal(());
        return;
    }
    let on = match core::str::from_utf8(args).unwrap_or("").trim() {
        "" => None,
        "raise on" => Some(true),
        "raise off" => Some(false),
        _ => {
            uart.write_line("[imu] usage: imu [raise on|off | record [start|stop|dump|clear]]");
            return;
        }
    };
    if let Some(on) = on {
        crate::rtc::set_raise_wanted(on);
        RAISE_CHANGED.store(true, Ordering::Relaxed);
    }
    request_read(true);
}

#[embassy_executor::task]
pub async fn run(mut bus: Bus, mut irq: ExtiInput<'static, Async>) -> ! {
    RUNNING.store(true, Ordering::Relaxed);
    let uart = Uart::best_effort();
    let found = found(&mut bus).await;
    if let Ok(f) = &found {
        FOUND.store(
            u32::from_le_bytes([f.internal_status, f.pwr_ctrl, f.acc_conf, f.pwr_conf]),
            Ordering::Relaxed,
        );
        FOUND_STEPS_ON.store(wrist_imu::steps_enabled(&f.step_page), Ordering::Relaxed);
    }
    let state = match found {
        Ok(f) if f.adoptable() => ADOPTED,
        _ => match load(&mut bus).await {
            Ok(()) => LOADED,
            Err(why) => {
                uart.write_str("[imu] not counting: ");
                uart.write_line(why);
                FAILED
            }
        },
    };
    STATE.store(state, Ordering::Relaxed);
    if state != FAILED {
        uart.write_line(if state == ADOPTED { "[imu] counting, carried on" } else { "[imu] counting from 0" });
    }
    let mut ledger = Ledger::restore(&crate::rtc::steps_words(), state == ADOPTED);
    RAISE_CHANGED.store(state != FAILED, Ordering::Relaxed);
    // A recording cut short by a reset leaves the FIFO filling.
    if state != FAILED && fifo(&mut bus, wrist_imu::FIFO_OFF, false).await.is_err() {
        uart.write_line("[imu] FIFO not turned off");
    }
    let mut rec: Option<Recording<'static>> = None;
    let mut next_drain: Option<Instant> = None;
    loop {
        match REC_REQUEST.swap(0, Ordering::Relaxed) {
            0 => {}
            _ if state == FAILED => uart.write_line("[rec] the IMU is not counting"),
            req => rec_command(&mut bus, req, &mut rec, &mut next_drain, uart).await,
        }
        if RAISE_CHANGED.swap(false, Ordering::Relaxed) && state != FAILED {
            let on = crate::rtc::raise_wanted();
            if route_raise(&mut bus, on).await.is_err() {
                uart.write_line("[imu] raise setting not applied");
                RAISE_CHANGED.store(true, Ordering::Relaxed);
            }
        }
        if state != FAILED {
            match outputs(&mut bus).await {
                Ok(out) => {
                    STEPS.store(out.steps, Ordering::Relaxed);
                    ACTIVITY.store(out.activity as u8, Ordering::Relaxed);
                    let day = crate::rtc::now_civil().and_then(|c| wrist_steps::day_of(wrist_time::seconds_of(c)));
                    let l = ledger.get_or_insert_with(|| Ledger::new(day, out.steps));
                    TODAY.store(l.observe(day, out.steps), Ordering::Relaxed);
                    if !TODAY_KNOWN.swap(true, Ordering::Relaxed) {
                        crate::wake_display();
                    }
                    YESTERDAY.store(l.yesterday().unwrap_or(u32::MAX), Ordering::Relaxed);
                    crate::rtc::set_steps_words(&l.save());
                }
                Err(()) => uart.write_line("[imu] read failed"),
            }
        }
        DONE.signal(());
        if REPORT.swap(false, Ordering::Relaxed) {
            write_report(uart);
        }
        loop {
            // A raise latched while the task was busy left the line high, with no edge to wait for.
            for _ in 0..3 {
                if !irq.is_high() {
                    break;
                }
                service_raise(&mut bus).await;
            }
            let tick = async {
                match next_drain {
                    Some(at) => Timer::at(at).await,
                    None => core::future::pending().await,
                }
            };
            let raise = async {
                if irq.is_high() {
                    irq.wait_for_rising_edge().await;
                } else {
                    irq.wait_for_high().await;
                }
            };
            match select3(READ.wait(), raise, tick).await {
                Either3::First(()) => break,
                Either3::Second(()) => service_raise(&mut bus).await,
                Either3::Third(()) => {
                    next_drain = Some(Instant::now() + DRAIN_EVERY);
                    if let Some(r) = rec.as_mut() {
                        if drain(&mut bus, r).await == Err(true) {
                            let _ = fifo(&mut bus, wrist_imu::FIFO_OFF, false).await;
                            next_drain = None;
                            uart.write_line("[rec] full; stopped");
                        }
                    }
                }
            }
        }
    }
}

/// Reading `INT_STATUS_0` clears the latched line; a failed read waits a little to be tried again.
async fn service_raise(bus: &mut Bus) {
    match get(bus, reg::INT_STATUS_0) {
        Ok(status) if wrist_imu::raised(status) && crate::rtc::raise_wanted() => {
            RAISES.fetch_add(1, Ordering::Relaxed);
            crate::light_for_raise();
        }
        Ok(_) => {}
        Err(()) => Timer::after_millis(10).await,
    }
}

async fn rec_command(bus: &mut Bus, req: u8, rec: &mut Option<Recording<'static>>, next_drain: &mut Option<Instant>, uart: Uart) {
    match req {
        REC_START => match (&*rec, crate::update::claim_staging()) {
            (Some(_), _) => uart.write_line("[rec] a recording is held; `imu record clear` first"),
            (None, None) => uart.write_line("[rec] the staging RAM is busy with a transfer"),
            (None, Some(buf)) => {
                let mut r = Recording::new(buf);
                let started = fifo(bus, wrist_imu::FIFO_RECORDING, true).await.is_ok() && mark(bus, &mut r).await.is_ok();
                if started {
                    uart.write_line("[rec] recording at 50 Hz, for at most 28 minutes");
                    *next_drain = Some(Instant::now() + DRAIN_EVERY);
                    *rec = Some(r);
                } else {
                    let _ = fifo(bus, wrist_imu::FIFO_OFF, false).await;
                    crate::update::release_staging();
                    uart.write_line("[rec] not started: the IMU did not answer");
                }
            }
        },
        REC_STOP => {
            if let (Some(r), Some(_)) = (rec.as_mut(), *next_drain) {
                let _ = drain(bus, r).await;
                let _ = fifo(bus, wrist_imu::FIFO_OFF, false).await;
                *next_drain = None;
            }
            write_rec(uart, rec.as_ref(), next_drain.is_some());
        }
        REC_DUMP => match &*rec {
            Some(r) => dump(bus, r).await,
            None => uart.write_line("[rec] nothing recorded"),
        },
        REC_CLEAR => {
            if next_drain.take().is_some() {
                let _ = fifo(bus, wrist_imu::FIFO_OFF, false).await;
            }
            if rec.take().is_some() {
                crate::update::release_staging();
            }
            uart.write_line("[rec] cleared; the staging RAM is free");
        }
        _ => write_rec(uart, rec.as_ref(), next_drain.is_some()),
    }
}

/// `FIFO_CONFIG_0` and `FIFO_CONFIG_1` set to `config`, and with `flush` the FIFO emptied.
async fn fifo(bus: &mut Bus, config: [u8; 2], flush: bool) -> Result<(), ()> {
    put(bus, reg::PWR_CONF, 0).await?;
    put(bus, reg::FIFO_CONFIG_0, config[0]).await?;
    put(bus, reg::FIFO_CONFIG_1, config[1]).await?;
    if flush {
        put(bus, reg::CMD, wrist_imu::FIFO_FLUSH).await?;
    }
    put(bus, reg::PWR_CONF, wrist_imu::PWR_CONF).await
}

/// The chip's count and activity, after the samples recorded so far.
async fn mark(bus: &mut Bus, r: &mut Recording<'static>) -> Result<(), ()> {
    let out = outputs(bus).await?;
    if r.push_mark(out.steps, out.activity as u8) { Ok(()) } else { Err(()) }
}

/// The FIFO into the recording, then a mark. `Err(true)` when the recording is full.
async fn drain(bus: &mut Bus, r: &mut Recording<'static>) -> Result<(), bool> {
    put(bus, reg::PWR_CONF, 0).await.map_err(|()| false)?;
    let mut length = [0; 2];
    let read = bus.blocking_write_read(ADDR, &[reg::FIFO_LENGTH_0], &mut length).map_err(|_| false);
    let mut left = record::frames(record::fill(length));
    let mut full = false;
    let mut buf = [0; record::READ];
    while read.is_ok() && left > 0 {
        let n = left.min(record::READ / record::FRAME);
        if !r.has_room(n) {
            full = true;
            break;
        }
        let bytes = &mut buf[..n * record::FRAME];
        if bus.blocking_write_read(ADDR, &[reg::FIFO_DATA], bytes).is_err() || !r.push_frames(bytes) {
            break;
        }
        left -= n;
        yield_now().await;
    }
    put(bus, reg::PWR_CONF, wrist_imu::PWR_CONF).await.map_err(|()| false)?;
    read?;
    mark(bus, r).await.map_err(|()| true)?;
    if full { Err(true) } else { Ok(()) }
}

fn write_rec(uart: Uart, rec: Option<&Recording<'static>>, running: bool) {
    let Some(r) = rec else {
        uart.write_line("[rec] nothing recorded");
        return;
    };
    uart.write_str(if running { "[rec] recording: " } else { "[rec] stopped: " });
    uart.write_u32(u32::try_from(r.samples()).unwrap_or(u32::MAX));
    uart.write_str(" samples, ");
    uart.write_u32(u32::try_from(r.samples()).unwrap_or(u32::MAX) / SAMPLE_HZ);
    uart.write_str(" s, ");
    uart.write_u32(u32::try_from(r.marks()).unwrap_or(u32::MAX));
    uart.write_str(" marks");
    match (r.mark(0), r.marks().checked_sub(1).and_then(|i| r.mark(i))) {
        (Some(first), Some(last)) => {
            uart.write_str("; the chip counted ");
            uart.write_u32(last.steps.wrapping_sub(first.steps));
            uart.write_line(" steps");
        }
        _ => uart.write_line(""),
    }
}

/// The recording over the console (`tools/wrist-imu-record.py`): a header, the samples as hex,
/// the marks, and a CRC-32 of the samples' bytes.
async fn dump(bus: &mut Bus, r: &Recording<'static>) {
    const PER_LINE: usize = 20;
    let uart = Uart::best_effort();
    let mut line = [0u8; 32 + 2 * PER_LINE * record::FRAME];
    let mut n = 0;
    let range = get(bus, reg::ACC_RANGE).unwrap_or(0xFF);
    put_text(&mut line, &mut n, b"[rec] begin samples ");
    put_dec(&mut line, &mut n, u32::try_from(r.samples()).unwrap_or(u32::MAX));
    put_text(&mut line, &mut n, b" marks ");
    put_dec(&mut line, &mut n, u32::try_from(r.marks()).unwrap_or(u32::MAX));
    put_text(&mut line, &mut n, b" hz 50 range ");
    put_dec(&mut line, &mut n, u32::from(range));
    put_text(&mut line, &mut n, b"\r\n");
    let mut sent = crate::console::write_whole(&line[..n]).await;
    let mut crc = 0xFFFF_FFFF;
    let mut at = 0;
    while sent && at < r.samples() {
        let bytes = r.sample_bytes(at..at + PER_LINE);
        crc = wrist_flash::plan::crc32_continue(crc, bytes);
        n = 0;
        put_text(&mut line, &mut n, b"[rec] a ");
        put_dec(&mut line, &mut n, u32::try_from(at).unwrap_or(u32::MAX));
        put_text(&mut line, &mut n, b" ");
        for &b in bytes {
            put_text(&mut line, &mut n, &hex8(b));
        }
        put_text(&mut line, &mut n, b"\r\n");
        sent = crate::console::write_whole(&line[..n]).await;
        at += PER_LINE;
        yield_now().await;
    }
    for i in 0..r.marks() {
        let Some(m) = r.mark(i) else { break };
        if !sent {
            break;
        }
        n = 0;
        put_text(&mut line, &mut n, b"[rec] m ");
        put_dec(&mut line, &mut n, m.at);
        put_text(&mut line, &mut n, b" ");
        put_dec(&mut line, &mut n, m.steps);
        put_text(&mut line, &mut n, b" ");
        put_dec(&mut line, &mut n, u32::from(m.activity));
        put_text(&mut line, &mut n, b"\r\n");
        sent = crate::console::write_whole(&line[..n]).await;
    }
    if !sent {
        uart.write_line("[rec] abandoned: the console closed or stayed full");
        return;
    }
    uart.write_str("[rec] end crc32 ");
    uart.write_hex32(!crc);
    uart.write_line("");
}

fn put_text(line: &mut [u8], n: &mut usize, s: &[u8]) {
    line[*n..*n + s.len()].copy_from_slice(s);
    *n += s.len();
}

fn put_dec(line: &mut [u8], n: &mut usize, v: u32) {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    let mut v = v;
    loop {
        i -= 1;
        #[allow(clippy::cast_possible_truncation)]
        let d = (v % 10) as u8;
        digits[i] = b'0' + d;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    put_text(line, n, &digits[i..]);
}

fn hex8(b: u8) -> [u8; 2] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    [HEX[usize::from(b >> 4)], HEX[usize::from(b & 0x0F)]]
}

/// The wrist-wear wake-up on or off, and its output on `INT2`.
async fn route_raise(bus: &mut Bus, on: bool) -> Result<(), ()> {
    put(bus, reg::PWR_CONF, 0).await?;
    put(bus, reg::FEAT_PAGE, wrist_imu::RAISE_PAGE).await?;
    let mut page = [0; PAGE_LEN];
    bus.blocking_write_read(ADDR, &[reg::FEATURES], &mut page).map_err(|_| ())?;
    wrist_imu::enable_raise(&mut page, on);
    let mut write = [0; 1 + PAGE_LEN];
    write[0] = reg::FEATURES;
    write[1..].copy_from_slice(&page);
    bus.blocking_write(ADDR, &write).map_err(|_| ())?;
    Timer::after_millis(1).await;
    put(bus, reg::FEAT_PAGE, OUT_PAGE).await?;
    for (r, v) in wrist_imu::raise_routing(on) {
        put(bus, r, v).await?;
    }
    put(bus, reg::PWR_CONF, wrist_imu::PWR_CONF).await?;
    // A raise latched before this would light the face as soon as the setting changed.
    get(bus, reg::INT_STATUS_0).map(|_| ())
}

fn write_report(uart: Uart) {
    match STATE.load(Ordering::Relaxed) {
        FAILED => uart.write_line("[imu] not counting (see the boot log)"),
        state => {
            uart.write_str("[imu] today ");
            uart.write_u32(TODAY.load(Ordering::Relaxed));
            match YESTERDAY.load(Ordering::Relaxed) {
                u32::MAX => uart.write_str(", yesterday unknown"),
                y => {
                    uart.write_str(", yesterday ");
                    uart.write_u32(y);
                }
            }
            uart.write_str(if state == ADOPTED { "; the chip's count, carried on: " } else { "; the chip's count since boot: " });
            uart.write_u32(STEPS.load(Ordering::Relaxed));
            uart.write_str(", ");
            let activity = match ACTIVITY.load(Ordering::Relaxed) {
                a if a == Activity::Still as u8 => Activity::Still,
                a if a == Activity::Walking as u8 => Activity::Walking,
                a if a == Activity::Running as u8 => Activity::Running,
                _ => Activity::Unknown,
            };
            uart.write_line(activity.name());
            uart.write_str(if crate::rtc::raise_wanted() { "[imu] raise lights the face; raises since boot " } else { "[imu] raise off; raises since boot " });
            uart.write_u32(RAISES.load(Ordering::Relaxed));
            uart.write_line("");
        }
    }
    write_found(uart);
}

fn write_found(uart: Uart) {
    let [internal, pwr_ctrl, acc_conf, pwr_conf] = FOUND.load(Ordering::Relaxed).to_le_bytes();
    uart.write_str("[imu] found at boot: internal ");
    uart.write_hex8(internal);
    uart.write_str(" pwr_ctrl ");
    uart.write_hex8(pwr_ctrl);
    uart.write_str(" acc_conf ");
    uart.write_hex8(acc_conf);
    uart.write_str(" pwr_conf ");
    uart.write_hex8(pwr_conf);
    uart.write_line(if FOUND_STEPS_ON.load(Ordering::Relaxed) { ", steps on" } else { ", steps off" });
}

async fn found(bus: &mut Bus) -> Result<Found, ()> {
    let internal_status = get(bus, reg::INTERNAL_STATUS)?;
    let pwr_ctrl = get(bus, reg::PWR_CTRL)?;
    let acc_conf = get(bus, reg::ACC_CONF)?;
    let pwr_conf = get(bus, reg::PWR_CONF)?;
    let mut step_page = [0; PAGE_LEN];
    feature_read(bus, STEP_PAGE, &mut step_page, pwr_conf).await?;
    Ok(Found { internal_status, pwr_ctrl, acc_conf, pwr_conf, step_page })
}

async fn outputs(bus: &mut Bus) -> Result<Outputs, ()> {
    let mut out = [0; OUT_LEN];
    feature_read(bus, OUT_PAGE, &mut out, wrist_imu::PWR_CONF).await?;
    Ok(Outputs::parse(&out))
}

/// Bosch's driver turns advanced power save off around every feature-page access; this does
/// the same, leaving the chip on page 0 and `PWR_CONF` at `restore`.
async fn feature_read(bus: &mut Bus, page: u8, out: &mut [u8], restore: u8) -> Result<(), ()> {
    put(bus, reg::PWR_CONF, 0).await?;
    let read = put(bus, reg::FEAT_PAGE, page)
        .await
        .and_then(|()| bus.blocking_write_read(ADDR, &[reg::FEATURES], out).map_err(|_| ()));
    let back = put(bus, reg::FEAT_PAGE, OUT_PAGE).await;
    put(bus, reg::PWR_CONF, restore).await?;
    read.and(back)
}

/// Bosch's sequence (datasheet section 4.2): reset, power save off, the image in bursts, the
/// engine's check, then the counter, the accelerometer and power save on. A failure after the
/// reset resets it again, into suspend.
async fn load(bus: &mut Bus) -> Result<(), &'static str> {
    if get(bus, reg::CHIP_ID) != Ok(CHIP_ID) {
        return Err("no BMI270 at 0x68");
    }
    put(bus, reg::CMD, wrist_imu::SOFT_RESET).await.map_err(|()| "reset")?;
    Timer::after_millis(2).await;
    let set_up = set_up(bus).await;
    if set_up.is_err() {
        let _ = put(bus, reg::CMD, wrist_imu::SOFT_RESET).await;
    }
    set_up
}

async fn set_up(bus: &mut Bus) -> Result<(), &'static str> {
    put(bus, reg::PWR_CONF, 0).await.map_err(|()| "power save")?;
    put(bus, reg::INIT_CTRL, 0).await.map_err(|()| "upload")?;
    for chunk in wrist_imu::chunks() {
        bus.blocking_write(ADDR, &chunk.addr).map_err(|_| "upload")?;
        bus.blocking_write(ADDR, &chunk.data).map_err(|_| "upload")?;
        yield_now().await;
    }
    put(bus, reg::INIT_CTRL, 1).await.map_err(|()| "upload")?;
    let mut up = false;
    for _ in 0..5 {
        Timer::after_millis(20).await;
        if get(bus, reg::INTERNAL_STATUS).is_ok_and(wrist_imu::engine_ok) {
            up = true;
            break;
        }
    }
    if !up {
        return Err("the configuration image was not accepted");
    }
    let mut page = [0; PAGE_LEN];
    put(bus, reg::FEAT_PAGE, STEP_PAGE).await.map_err(|()| "feature page")?;
    bus.blocking_write_read(ADDR, &[reg::FEATURES], &mut page).map_err(|_| "feature page")?;
    wrist_imu::enable_steps(&mut page);
    let mut write = [0; 1 + PAGE_LEN];
    write[0] = reg::FEATURES;
    write[1..].copy_from_slice(&page);
    bus.blocking_write(ADDR, &write).map_err(|_| "feature page")?;
    Timer::after_millis(1).await;
    put(bus, reg::FEAT_PAGE, OUT_PAGE).await.map_err(|()| "feature page")?;
    put(bus, reg::ACC_CONF, wrist_imu::ACC_CONF).await.map_err(|()| "accelerometer")?;
    put(bus, reg::PWR_CTRL, wrist_imu::PWR_CTRL).await.map_err(|()| "accelerometer")?;
    put(bus, reg::PWR_CONF, wrist_imu::PWR_CONF).await.map_err(|()| "power save")?;
    Ok(())
}

fn get(bus: &mut Bus, reg: u8) -> Result<u8, ()> {
    let mut b = [0];
    bus.blocking_write_read(ADDR, &[reg], &mut b).map_err(|_| ())?;
    Ok(b[0])
}

/// A write, then the gap power save needs before the next access.
async fn put(bus: &mut Bus, reg: u8, value: u8) -> Result<(), ()> {
    bus.blocking_write(ADDR, &[reg, value]).map_err(|_| ())?;
    Timer::after_micros(wrist_imu::POWER_SAVE_GAP_US + 50).await;
    Ok(())
}
