//! The daily alarm in `BKP4R` (`wrist_time::alarm`), and the console's `alarm`.
//!
//! The display task checks it on every pass, after any zone change, so on battery it is checked
//! as the minute turns, on the wake that redraws the face, and costs no wake of its own; on USB
//! the task also runs every 10 s. A ring lights the face and says so until a press dismisses it
//! or a minute passes.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::{Duration, Instant};
use wrist_time::alarm::{minute_of_day, parse_hhmm, Alarm};

use crate::rtc::{self, Civil};
use crate::uart::Uart;

/// How long it rings with no press.
const RING_FOR: Duration = Duration::from_secs(60);

/// When the ring in progress began.
static RINGING_SINCE: Mutex<CriticalSectionRawMutex, Cell<Option<Instant>>> = Mutex::new(Cell::new(None));
/// `alarm test`, for the display task's next pass.
static TEST: AtomicBool = AtomicBool::new(false);

pub fn ringing() -> bool {
    RINGING_SINCE.lock(Cell::get).is_some()
}

/// The alarm while it is on, for the face.
pub fn shown() -> Option<Alarm> {
    rtc::alarm().filter(|a| a.on)
}

/// The display task's, on every pass after any zone change and before the page is composed:
/// the ring started when it is due, and stopped once it is over.
pub fn service(uart: Uart) {
    if due(rtc::now_civil()) {
        alert(uart);
    }
    stop_if_over(uart);
}

/// True when it rings now, against the calendar's `now`. The record is kept before the ring, so
/// a reset during it does not ring it again.
fn due(now: Option<Civil>) -> bool {
    if TEST.swap(false, Ordering::Relaxed) {
        return true;
    }
    let (Some(now), Some(alarm)) = (now, rtc::alarm()) else { return false };
    let (ring, after) = alarm.check(minute_of_day(now));
    if after != alarm {
        rtc::set_alarm(after);
    }
    ring
}

/// The ring: the face lit and shown, saying so. The display task draws it in the same pass.
fn alert(uart: Uart) {
    RINGING_SINCE.lock(|r| r.set(Some(Instant::now())));
    crate::to_face();
    crate::backlight(true);
    uart.write_line("[alarm] ringing");
    // `feat/haptic`: `haptic::play(Pattern::Alarm).await` goes here, the one place a ring
    // starts. This becomes `async`, and the display task awaits it; if `play` returns only
    // when the pattern ends, it moves to a task of its own so the face still draws.
}

/// When the ring stops by itself, while it rings.
pub fn stops_at() -> Option<Instant> {
    RINGING_SINCE.lock(Cell::get).map(|since| since + RING_FOR)
}

/// The ring's end once [`RING_FOR`] has passed with no press.
fn stop_if_over(uart: Uart) {
    if stops_at().is_some_and(|at| Instant::now() >= at) {
        RINGING_SINCE.lock(|r| r.set(None));
        // `feat/haptic`: `haptic::stop()`, should the pattern outlast the ring.
        crate::backlight(false);
        uart.write_line("[alarm] stopped after 60 s with no press");
    }
}

/// The button task's, for a press while it rings: how long it rang, once it has stopped. The
/// press does nothing else.
pub fn dismiss() -> Option<Duration> {
    RINGING_SINCE.lock(Cell::take).map(|since| since.elapsed())
}

/// After `time set`: the record as if set at the new time, so moving the clock over the
/// alarm's minute does not ring it.
pub fn clock_set(now: Civil) {
    if let Some(alarm) = rtc::alarm() {
        let settled = alarm.settled(minute_of_day(now));
        if settled != alarm {
            rtc::set_alarm(settled);
        }
    }
}

/// `alarm [HH:MM|on|off|test]`.
pub fn command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    let now = rtc::now_civil().map(minute_of_day);
    let settle = |a: Alarm| now.map_or(Alarm { spent: false, ..a }, |n| a.settled(n));
    match text {
        "" => {}
        "test" => {
            TEST.store(true, Ordering::Relaxed);
            crate::wake_display();
            uart.write_line("[alarm] test: rings now, as at its time");
            return;
        }
        "on" => match rtc::alarm() {
            Some(a) => rtc::set_alarm(settle(Alarm { on: true, ..a })),
            None => return uart.write_line("[alarm] no time set: alarm HH:MM"),
        },
        "off" => {
            if let Some(a) = rtc::alarm() {
                rtc::set_alarm(Alarm { on: false, ..a });
            }
            if dismiss().is_some() {
                // `feat/haptic`: `haptic::stop()`.
                crate::backlight(false);
                uart.write_line("[alarm] ringing stopped");
            }
        }
        time => match parse_hhmm(time).and_then(|(h, m)| Alarm::new(h, m)) {
            Some(a) => rtc::set_alarm(settle(a)),
            None => return uart.write_line("[alarm] usage: alarm [HH:MM|on|off|test] (24-hour, local)"),
        },
    }
    crate::wake_display();
    uart.write_str("[alarm] ");
    write_record(uart);
    match (rtc::alarm().and_then(|a| now.and_then(|n| a.minutes_to_ring(n))), now) {
        (Some(0), _) => uart.write_str("; rings at the next check"),
        (Some(min), _) => {
            uart.write_str("; rings in ");
            if min >= 60 {
                uart.write_u32(u32::from(min / 60));
                uart.write_str(" h ");
            }
            uart.write_u32(u32::from(min % 60));
            uart.write_str(" min on the clock");
        }
        (None, None) => uart.write_str("; the calendar is not set, so it cannot ring"),
        (None, Some(_)) => {}
    }
    if ringing() {
        uart.write_str("; ringing");
    }
    uart.write_line("");
}

/// `07:00 on (BKP4R 0x414C09A4)`, or that there is none.
pub fn write_record(uart: Uart) {
    let word = rtc::alarm_word();
    match Alarm::decode(word) {
        Some(a) => {
            for (sep, v) in [("", a.hour()), (":", a.minute())] {
                uart.write_str(sep);
                if v < 10 {
                    uart.write_str("0");
                }
                uart.write_u32(u32::from(v));
            }
            uart.write_str(if a.on { " on" } else { " off" });
            if a.on && a.spent {
                uart.write_str(", today's ring spent");
            }
        }
        None => uart.write_str("none set"),
    }
    uart.write_str(" (BKP4R ");
    uart.write_hex32(word);
    uart.write_str(")");
}
