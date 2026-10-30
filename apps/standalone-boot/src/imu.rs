//! The BMI270's step counter. The chip keeps its state across the MCU's resets and updates
//! (`HARDWARE.md`, "Modules and slots"), so a boot adopts a chip already counting and loads only
//! one that is not. Today's steps come from its count through a [`Ledger`] kept in backup
//! registers.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_futures::yield_now;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Blocking;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Timer};
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
static YESTERDAY: AtomicU32 = AtomicU32::new(u32::MAX);

/// Today's steps, once the chip is counting.
pub fn today() -> Option<u32> {
    matches!(STATE.load(Ordering::Relaxed), ADOPTED | LOADED).then(|| TODAY.load(Ordering::Relaxed))
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
    } else if core::str::from_utf8(args).unwrap_or("").trim().is_empty() {
        request_read(true);
    } else {
        uart.write_line("[imu] usage: imu");
    }
}

#[embassy_executor::task]
pub async fn run(mut bus: Bus) -> ! {
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
    loop {
        if state != FAILED {
            match outputs(&mut bus).await {
                Ok(out) => {
                    STEPS.store(out.steps, Ordering::Relaxed);
                    ACTIVITY.store(out.activity as u8, Ordering::Relaxed);
                    let day = crate::rtc::now_civil().and_then(|c| wrist_steps::day_of(wrist_time::seconds_of(c)));
                    let l = ledger.get_or_insert_with(|| Ledger::new(day, out.steps));
                    TODAY.store(l.observe(day, out.steps), Ordering::Relaxed);
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
        READ.wait().await;
    }
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
