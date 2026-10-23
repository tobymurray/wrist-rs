//! The face's status, what was last drawn, and the console's `page`.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use wrist_glass::dump::{self, MAX_ROW_CODE, MAX_ROW_HEX};
use wrist_glass::pages::{Clock, Date, Status};
use wrist_glass::text::Glass;

use crate::uart::Uart;

static SHOWN: Mutex<CriticalSectionRawMutex, RefCell<Option<Glass>>> = Mutex::new(RefCell::new(None));

pub static FRAMES: AtomicU32 = AtomicU32::new(0);
pub static UNCHANGED: AtomicU32 = AtomicU32::new(0);
pub static LAST_FRAME_MS: AtomicU32 = AtomicU32::new(0);
pub static LAST_FRAME_COLUMNS: AtomicU32 = AtomicU32::new(0);
pub static MAX_FRAME_MS: AtomicU32 = AtomicU32::new(0);
pub static MAX_RENDER_US: AtomicU32 = AtomicU32::new(0);
pub static MAX_COMPOSE_US: AtomicU32 = AtomicU32::new(0);
static MAX_LAG_US: AtomicU32 = AtomicU32::new(0);
static LAGS_OVER_5MS: AtomicU32 = AtomicU32::new(0);
static MAX_LAG_AT_MS: AtomicU32 = AtomicU32::new(0);
/// (uptime ms, µs late), newest last.
static LAGS: Mutex<CriticalSectionRawMutex, RefCell<[(u32, u32); 8]>> = Mutex::new(RefCell::new([(0, 0); 8]));
static LAG_UNTIL_MS: AtomicU32 = AtomicU32::new(0);
static LAG_PROBED_US: AtomicU32 = AtomicU32::new(0);
static LAG_ARM: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static DUMP_WANTED: AtomicBool = AtomicBool::new(false);
pub const LAG_BOOT_S: u32 = 60;
const LAG_MAX_S: u32 = 3600;

pub fn now_ms() -> u32 {
    u32::try_from(Instant::now().as_millis()).unwrap_or(u32::MAX)
}

pub fn status(soc_pct: Option<u8>) -> Status {
    let civil = crate::rtc::now_civil();
    Status {
        clock: civil.map(|c| Clock { h: c.hour, m: c.minute, s: c.second }),
        date: civil.map(|c| Date { year: c.year, month: c.month, day: c.day }),
        soc_pct,
    }
}

pub fn shown(glass: &Glass) {
    SHOWN.lock(|s| *s.borrow_mut() = Some(*glass));
}

pub fn note_max(max: &AtomicU32, v: u32) {
    max.fetch_max(v, Ordering::Relaxed);
}

pub fn arm_lag_probe(seconds: u32) {
    LAG_UNTIL_MS.store(now_ms().saturating_add(seconds.min(LAG_MAX_S) * 1000), Ordering::Relaxed);
    LAG_ARM.signal(());
}

/// While armed, how late its own timer wakes: the longest any task held the executor.
#[embassy_executor::task]
pub async fn lag_probe() -> ! {
    const PERIOD_MS: u64 = 10;
    loop {
        if now_ms() >= LAG_UNTIL_MS.load(Ordering::Relaxed) {
            LAG_ARM.wait().await;
            continue;
        }
        let t = Instant::now();
        Timer::after_millis(PERIOD_MS).await;
        let took = t.elapsed();
        let late = u32::try_from(took.as_micros().saturating_sub(PERIOD_MS * 1000)).unwrap_or(u32::MAX);
        let took_us = u32::try_from(took.as_micros()).unwrap_or(u32::MAX);
        let _ = LAG_PROBED_US.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_add(took_us)));
        if late > MAX_LAG_US.load(Ordering::Relaxed) {
            MAX_LAG_US.store(late, Ordering::Relaxed);
            MAX_LAG_AT_MS.store(now_ms(), Ordering::Relaxed);
        }
        if late > 5_000 {
            LAGS_OVER_5MS.fetch_add(1, Ordering::Relaxed);
            LAGS.lock(|l| {
                let mut l = l.borrow_mut();
                l.copy_within(1.., 0);
                l[7] = (now_ms(), late);
            });
        }
    }
}

pub fn command(args: &[u8], uart: Uart) {
    match core::str::from_utf8(args).unwrap_or("").trim() {
        "" => write_page(uart),
        "lag" => arm_lag_command(uart, LAG_BOOT_S),
        lag if lag.starts_with("lag ") => match lag[4..].trim().parse::<u32>() {
            Ok(s) if (1..=LAG_MAX_S).contains(&s) => arm_lag_command(uart, s),
            _ => uart.write_line("[glass] usage: page lag [1-3600 seconds; default 60]"),
        },
        "vcom" => crate::lcd::write_vcom_status(uart),
        v if v.starts_with("vcom ") => match wrist_vcom::Mode::parse(v[5..].trim()) {
            Some(mode) => {
                crate::lcd::set_vcom_mode(mode);
                uart.write_str("[lcd] vcom mode ");
                uart.write_str(mode.name());
                uart.write_line("; `page vcom` reads the rate in force");
            }
            None => uart.write_line("[glass] usage: page vcom [auto|low|high]"),
        },
        "dump" => {
            DUMP_WANTED.store(true, Ordering::Relaxed);
            crate::wake_display();
            uart.write_line("[glass] frame dump requested; the display task writes it between frames");
        }
        "light on" => {
            crate::backlight(true);
            uart.write_line("[glass] front light on (BACKLIGHT_ON driven); VCOM follows in auto");
        }
        "light off" => {
            crate::backlight(false);
            uart.write_line("[glass] front light off");
        }
        _ => uart.write_line("[glass] usage: page [lag [seconds] | vcom [auto|low|high] | light on|off | dump]"),
    }
}

/// Whether `page dump` asked for a dump since the last call.
pub fn dump_wanted() -> bool {
    DUMP_WANTED.swap(false, Ordering::Relaxed)
}

/// `fb`, the frame last drawn, to the console only: a `begin` line, one line per row of
/// run-length pairs in hex (`wrist_glass::dump`), and an `end` line with the frame's CRC-32.
/// A row at a time, yielding after each. `tools/wrist-frame.py` reads it.
pub async fn dump(fb: &[u8]) {
    let uart = Uart::best_effort();
    let t = Instant::now();
    let mut line = [0u8; 16 + MAX_ROW_HEX];
    let mut code = [0u8; MAX_ROW_CODE];
    let mut longest_us = 0;
    let mut crc = 0xFFFF_FFFF;
    let mut n = 0;
    put(&mut line, &mut n, b"[frame] begin 240 240 abgr2222 frame ");
    put_u32(&mut line, &mut n, FRAMES.load(Ordering::Relaxed));
    put(&mut line, &mut n, b"\r\n");
    let mut sent = crate::console::write_whole(&line[..n]).await;
    for (y, row) in fb.chunks(crate::lcd::WIDTH as usize).enumerate() {
        if !sent {
            break;
        }
        let step = Instant::now();
        crc = wrist_flash::plan::crc32_continue(crc, row);
        n = 0;
        put(&mut line, &mut n, b"[frame] ");
        put_u32(&mut line, &mut n, u32::try_from(y).unwrap_or(0));
        put(&mut line, &mut n, b" ");
        let used = dump::encode_row(row, &mut code).unwrap_or(0);
        n += dump::hex(&code[..used], &mut line[n..]).unwrap_or(0);
        put(&mut line, &mut n, b"\r\n");
        longest_us = longest_us.max(crate::elapsed_us(step));
        sent = crate::console::write_whole(&line[..n]).await;
        embassy_futures::yield_now().await;
    }
    if !sent {
        uart.write_line("[frame] abandoned: the console closed or stayed full");
        return;
    }
    n = 0;
    put(&mut line, &mut n, b"[frame] end crc32 ");
    line[n..n + 8].copy_from_slice(&hex32(!crc));
    n += 8;
    put(&mut line, &mut n, b" ms ");
    put_u32(&mut line, &mut n, u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX));
    put(&mut line, &mut n, b" longest step us ");
    put_u32(&mut line, &mut n, longest_us);
    put(&mut line, &mut n, b"\r\n");
    let _ = crate::console::write_whole(&line[..n]).await;
}

fn put(line: &mut [u8], n: &mut usize, s: &[u8]) {
    if let Some(dst) = line.get_mut(*n..*n + s.len()) {
        dst.copy_from_slice(s);
        *n += s.len();
    }
}

fn put_u32(line: &mut [u8], n: &mut usize, v: u32) {
    let mut buf = [0u8; 11];
    put(line, n, wrist_glass::text::fmt_u32(&mut buf, v).as_bytes());
}

fn hex32(v: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    let _ = dump::hex(&v.to_be_bytes(), &mut out);
    out
}

fn arm_lag_command(uart: Uart, seconds: u32) {
    arm_lag_probe(seconds);
    uart.write_str("[glass] executor-latency probe armed for ");
    uart.write_u32(seconds);
    uart.write_line(" s (10 ms wakes); `page` reads it");
}

fn write_ms(uart: Uart, label: &str, v: u32) {
    uart.write_str(label);
    uart.write_u32(v);
    uart.write_str(" ms");
}

fn write_us_as_ms(uart: Uart, label: &str, us: u32) {
    uart.write_str(label);
    uart.write_u32(us / 1000);
    uart.write_str(".");
    uart.write_u32(us / 100 % 10);
    uart.write_str(" ms");
}

fn write_page(uart: Uart) {
    let (page, back) = crate::nav_now();
    uart.write_str("[glass] page ");
    uart.write_str(page.name());
    if let Some(at) = back {
        let ms = at.saturating_duration_since(Instant::now()).as_millis();
        write_ms(uart, ", back to the face in ", u32::try_from(ms).unwrap_or(u32::MAX));
    }
    uart.write_line("");
    match SHOWN.lock(|s| *s.borrow()) {
        None => uart.write_line("[glass] nothing sent to the panel yet"),
        Some(glass) => {
            uart.write_line("[glass] on the glass:");
            for (i, l) in glass.lines().iter().enumerate() {
                uart.write_str("[glass]   ");
                uart.write_u32(u32::try_from(i).unwrap_or(0));
                uart.write_str(" (row ");
                uart.write_u32(u32::try_from(l.top).unwrap_or(0));
                uart.write_str(", x");
                uart.write_u32(l.scale);
                uart.write_str(") \"");
                uart.write_str(core::str::from_utf8(l.text()).unwrap_or("?"));
                uart.write_line("\"");
            }
        }
    }
    uart.write_str("[glass] frames ");
    uart.write_u32(FRAMES.load(Ordering::Relaxed));
    uart.write_str(", unchanged recompositions ");
    uart.write_u32(UNCHANGED.load(Ordering::Relaxed));
    uart.write_line("");
    write_ms(uart, "[glass] frame: last ", LAST_FRAME_MS.load(Ordering::Relaxed));
    uart.write_str(" for ");
    uart.write_u32(LAST_FRAME_COLUMNS.load(Ordering::Relaxed));
    uart.write_str(" of 240 columns");
    write_ms(uart, ", max ", MAX_FRAME_MS.swap(0, Ordering::Relaxed));
    uart.write_line("");
    write_us_as_ms(uart, "[glass] longest compose ", MAX_COMPOSE_US.swap(0, Ordering::Relaxed));
    write_us_as_ms(uart, ", render ", MAX_RENDER_US.swap(0, Ordering::Relaxed));
    write_us_as_ms(uart, "; executor late by at most ", MAX_LAG_US.swap(0, Ordering::Relaxed));
    write_ms(uart, " at ", MAX_LAG_AT_MS.swap(0, Ordering::Relaxed));
    uart.write_str(", over 5 ms ");
    uart.write_u32(LAGS_OVER_5MS.swap(0, Ordering::Relaxed));
    uart.write_line(" times (maxima since the last `page`)");
    let probed = LAG_PROBED_US.swap(0, Ordering::Relaxed) / 1000;
    let until = LAG_UNTIL_MS.load(Ordering::Relaxed);
    uart.write_str("[glass] latency probe ran ");
    uart.write_u32(probed / 1000);
    uart.write_str(" s since the last `page`; ");
    if now_ms() < until {
        write_ms(uart, "armed until ", until);
    } else {
        uart.write_str("off");
        if until != 0 {
            write_ms(uart, " since ", until);
        }
    }
    uart.write_line(" (`page lag [seconds]` arms it; from boot for 60 s)");
    let lags = LAGS.lock(|l| core::mem::take(&mut *l.borrow_mut()));
    if lags.iter().any(|&(at, _)| at != 0) {
        uart.write_str("[glass] the last stalls over 5 ms (uptime ms: late us):");
        for (at, us) in lags.iter().filter(|&&(at, _)| at != 0) {
            uart.write_str(" ");
            uart.write_u32(*at);
            uart.write_str(":");
            uart.write_u32(*us);
        }
        uart.write_line("");
    }
}
