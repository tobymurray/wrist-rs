//! The daily alarm in `BKP4R` (`wrist_time::alarm`), its task, and the console's `alarm`.
//!
//! [`run`] checks the record at boot, when RTC alarm A matches, and when [`WAKE`] says
//! something moved the calendar or changed the record; then it arms alarm A for the minute of
//! the next check (`Alarm::next_check`). So it wakes the watch for nothing but the ring and,
//! some hours later, the record's clearing, and no other task's work can delay it. A ring
//! lights the face, says so, and plays the motor's alarm until a press or a minute stops it.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, Ordering};

use embassy_futures::select::select;
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::InterruptExt;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use wrist_haptic::{Pattern, ALARM_FOR_MS};
use wrist_time::alarm::{minute_of_day, parse_hhmm, Alarm, Found, DAY};

use crate::rtc::{self, Civil};
use crate::uart::Uart;

/// How long it rings with no press: the motor's own limit.
const RING_FOR: Duration = Duration::from_millis(ALARM_FOR_MS as u64);

/// When the ring in progress began.
static RINGING_SINCE: Mutex<CriticalSectionRawMutex, Cell<Option<Instant>>> = Mutex::new(Cell::new(None));
/// `alarm test`, for the task's next pass.
static TEST: AtomicBool = AtomicBool::new(false);
/// Set by the RTC interrupt on alarm A's match.
static MATCHED: AtomicBool = AtomicBool::new(false);
/// The forward minutes a change of zone moved the calendar, for the next pass.
static JUMPED: AtomicU16 = AtomicU16::new(0);
/// RTC interrupts this boot, for `alarm`.
static MATCHES: AtomicU32 = AtomicU32::new(0);
/// `alarm wake-test`: the uptime in ms of the second alarm A was armed for, 0 for none.
static WAKE_TEST_AT: AtomicU32 = AtomicU32::new(0);
/// How long after that second the RTC interrupt ran, in ms (to the RTC's 4 ms resolution, so
/// it can read a few ms early); `i32::MIN` before a test.
static WAKE_TEST_LATE: AtomicI32 = AtomicI32::new(i32::MIN);
/// Raised by whatever changes what [`run`] reads; it is reset before the reading.
static WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub fn ringing() -> bool {
    RINGING_SINCE.lock(Cell::get).is_some()
}

/// The alarm while it is on, for the face.
pub fn shown() -> Option<Alarm> {
    rtc::alarm().filter(|a| a.on)
}

/// True while a missed alarm waits for a press, for the face.
pub fn missed() -> bool {
    rtc::alarm().is_some_and(|a| a.missed)
}

/// The RTC's non-secure interrupt. Only alarm A has its interrupt on.
#[interrupt]
fn RTC() {
    MATCHES.fetch_add(1, Ordering::Relaxed);
    let at = WAKE_TEST_AT.swap(0, Ordering::Relaxed);
    if at != 0 {
        WAKE_TEST_LATE.store(now_ms().wrapping_sub(at).cast_signed(), Ordering::Relaxed);
    }
    if rtc::take_alarm_a() {
        MATCHED.store(true, Ordering::Relaxed);
        WAKE.signal(());
    }
}

/// The alarm's task: a check on each pass, then alarm A armed for the next, then asleep until
/// [`WAKE`] or the ring's end.
#[embassy_executor::task]
pub async fn run() -> ! {
    let uart = Uart::best_effort();
    // `park` disarmed alarm A and cleared its flag: a match while the image was down is the
    // first pass's to find, by the time.
    interrupt::RTC.unpend();
    unsafe { interrupt::RTC.enable() };
    loop {
        WAKE.reset();
        let jumped = JUMPED.swap(0, Ordering::Relaxed);
        let now = rtc::now_civil();
        if MATCHED.swap(false, Ordering::Relaxed) {
            uart.write_str("[alarm] alarm A matched at ");
            match now {
                Some(c) => rtc::write_civil(uart, c),
                None => uart.write_str("(no calendar)"),
            }
            uart.write_line("");
        }
        if TEST.swap(false, Ordering::Relaxed) {
            alert(uart);
        }
        let minute = now.map(minute_of_day);
        check(uart, minute, jumped);
        arm(minute);
        // A minute that turned during the pass may be the one just armed for.
        if rtc::now_civil().map(minute_of_day) != minute {
            continue;
        }
        stop_if_over(uart);
        match stops_at() {
            Some(at) => {
                select(WAKE.wait(), Timer::at(at)).await;
            }
            None => WAKE.wait().await,
        }
    }
}

/// The check at the calendar's `minute`, kept before it rings.
fn check(uart: Uart, minute: Option<u16>, jumped: u16) {
    let (Some(minute), Some(alarm)) = (minute, rtc::alarm()) else { return };
    let (found, after) = alarm.check(minute, jumped);
    if after != alarm {
        rtc::set_alarm(after);
    }
    match found {
        Found::Nothing => {}
        Found::Ring => alert(uart),
        Found::Missed => {
            uart.write_str("[alarm] missed: ");
            write_record(uart);
            uart.write_line("; the face says so until a press");
            crate::wake_display();
        }
    }
}

fn now_ms() -> u32 {
    u32::try_from(Instant::now().as_millis() % (1 << 31)).unwrap_or(0).max(1)
}

/// Alarm A armed for the minute of the next check, or off with none to come.
fn arm(minute: Option<u16>) {
    let next = minute.zip(rtc::alarm()).and_then(|(m, a)| a.next_check(m).map(|n| (m + n.max(1)) % DAY));
    let want = next.map(|m| (u8::try_from(m / 60).unwrap_or(0), u8::try_from(m % 60).unwrap_or(0), 0));
    if rtc::alarm_a() != want {
        match want {
            Some((h, m, s)) => rtc::arm_alarm_a(h, m, s),
            None => rtc::disarm_alarm_a(),
        }
    }
}

/// The ring: the face lit and shown, saying so, and the motor's alarm.
fn alert(uart: Uart) {
    RINGING_SINCE.lock(|r| r.set(Some(Instant::now())));
    if let Some(a) = rtc::alarm().filter(|a| a.missed) {
        rtc::set_alarm(Alarm { missed: false, ..a });
    }
    crate::haptic::start(Pattern::Alarm);
    crate::to_face();
    crate::backlight(true);
    crate::wake_display();
    uart.write_line("[alarm] ringing");
}

/// When the ring stops by itself, while it rings.
fn stops_at() -> Option<Instant> {
    RINGING_SINCE.lock(Cell::get).map(|since| since + RING_FOR)
}

/// The ring's end once [`RING_FOR`] has passed with no press.
fn stop_if_over(uart: Uart) {
    if stops_at().is_some_and(|at| Instant::now() >= at) {
        RINGING_SINCE.lock(|r| r.set(None));
        crate::haptic::stop();
        crate::backlight(false);
        crate::wake_display();
        uart.write_line("[alarm] stopped after 60 s with no press");
    }
}

/// The button task's, for a press while it rings: how long it rang, once it has stopped. The
/// press does nothing else.
pub fn dismiss() -> Option<Duration> {
    let rang = RINGING_SINCE.lock(Cell::take).map(|since| since.elapsed())?;
    crate::haptic::stop();
    WAKE.signal(());
    Some(rang)
}

/// The button task's, for any other press: the missed alarm seen, if one waited. The press
/// then does what it does.
pub fn seen() -> bool {
    let Some(a) = rtc::alarm().filter(|a| a.missed) else { return false };
    rtc::set_alarm(Alarm { missed: false, ..a });
    crate::wake_display();
    true
}

/// After a change of zone moved the calendar by `shift` minutes.
pub fn calendar_moved(shift: i32) {
    let forward = u16::try_from(shift.clamp(0, i32::from(DAY))).unwrap_or(0);
    JUMPED.fetch_max(forward, Ordering::Relaxed);
    WAKE.signal(());
}

/// After `time set`: the record as if set at the new time, so moving the clock over the
/// alarm's minute neither rings it nor marks it missed.
pub fn clock_set(now: Civil) {
    if let Some(alarm) = rtc::alarm() {
        let settled = alarm.settled(minute_of_day(now));
        if settled != alarm {
            rtc::set_alarm(settled);
        }
    }
    WAKE.signal(());
}

/// `alarm [HH:MM|on|off|test]`.
pub fn command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    let now = rtc::now_civil().map(minute_of_day);
    let settle = |a: Alarm| {
        let a = Alarm { missed: false, ..a };
        now.map_or(Alarm { spent: false, ..a }, |n| a.settled(n))
    };
    match text {
        "" => {}
        "test" => {
            TEST.store(true, Ordering::Relaxed);
            WAKE.signal(());
            uart.write_line("[alarm] test: rings now, as at its time");
            return;
        }
        wake if wake.starts_with("wake-test") => return wake_test(wake[9..].trim(), uart),
        "on" => match rtc::alarm() {
            Some(a) => rtc::set_alarm(settle(Alarm { on: true, ..a })),
            None => return uart.write_line("[alarm] no time set: alarm HH:MM"),
        },
        "off" => {
            if let Some(a) = rtc::alarm() {
                rtc::set_alarm(Alarm { on: false, missed: false, ..a });
            }
            if dismiss().is_some() {
                crate::backlight(false);
                uart.write_line("[alarm] ringing stopped");
            }
        }
        time => match parse_hhmm(time).and_then(|(h, m)| Alarm::new(h, m)) {
            Some(a) => rtc::set_alarm(settle(a)),
            None => return uart.write_line("[alarm] usage: alarm [HH:MM|on|off|test] (24-hour, local)"),
        },
    }
    // The task arms alarm A for it.
    WAKE.signal(());
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
    if text.is_empty() {
        write_alarm_a(uart);
    }
}

/// `07:00 on (BKP4R 0x414C09A4)`, or that there is none.
pub fn write_record(uart: Uart) {
    let word = rtc::alarm_word();
    match Alarm::decode(word) {
        Some(a) => {
            write_hm(uart, a.hour(), a.minute());
            uart.write_str(if a.on { " on" } else { " off" });
            if a.missed {
                uart.write_str(", missed and not yet seen");
            } else if a.on && a.spent {
                uart.write_str(", today's ring spent");
            }
        }
        None => uart.write_str("none set"),
    }
    uart.write_str(" (BKP4R ");
    uart.write_hex32(word);
    uart.write_str(")");
}

/// `alarm wake-test <s>`: alarm A armed for the second `s` from now, off the minute's wakes,
/// so the delay `alarm` then reports says whether its interrupt woke the core. The task arms
/// it back for the alarm on the match.
fn wake_test(args: &str, uart: Uart) {
    let (Some(s), Some(now), Some(into)) = (args.parse::<u32>().ok().filter(|s| (2..=3600).contains(s)), rtc::now_civil(), rtc::ms_into_minute()) else {
        return uart.write_line("[alarm] usage: alarm wake-test <2 to 3600 seconds> (the calendar must be set)");
    };
    let of_day = u32::from(now.hour) * 3600 + u32::from(now.minute) * 60 + into / 1000;
    let at = (of_day + s) % 86_400;
    let (h, m, sec) = (at / 3600, at / 60 % 60, at % 60);
    let byte = |v: u32| u8::try_from(v).unwrap_or(0);
    WAKE_TEST_LATE.store(i32::MIN, Ordering::Relaxed);
    WAKE_TEST_AT.store(now_ms().wrapping_add(s * 1000 - into % 1000).max(1), Ordering::Relaxed);
    rtc::arm_alarm_a(byte(h), byte(m), byte(sec));
    uart.write_str("[alarm] wake test: alarm A armed for ");
    write_hm(uart, byte(h), byte(m));
    uart.write_str(":");
    if sec < 10 {
        uart.write_str("0");
    }
    uart.write_u32(sec);
    uart.write_line("; `alarm` after it says how late its interrupt ran");
}

/// What alarm A holds, and the RTC's interrupts this boot.
fn write_alarm_a(uart: Uart) {
    uart.write_str("[alarm] alarm A ");
    match rtc::alarm_a() {
        Some((h, m, s)) => {
            uart.write_str("armed for ");
            write_hm(uart, h, m);
            if s != 0 {
                uart.write_str(if s < 10 { ":0" } else { ":" });
                uart.write_u32(u32::from(s));
            }
        }
        None => uart.write_str("off"),
    }
    uart.write_str(if interrupt::RTC.is_enabled() { ", RTC interrupt on" } else { ", RTC interrupt off" });
    uart.write_str("; ");
    uart.write_u32(MATCHES.load(Ordering::Relaxed));
    uart.write_line(" RTC interrupts this boot");
    let late = WAKE_TEST_LATE.load(Ordering::Relaxed);
    if late != i32::MIN {
        uart.write_str("[alarm] wake test: the interrupt ran ");
        if late < 0 {
            uart.write_str("-");
        }
        uart.write_u32(late.unsigned_abs());
        uart.write_line(" ms after its second");
    }
}

fn write_hm(uart: Uart, h: u8, m: u8) {
    for (sep, v) in [("", h), (":", m)] {
        uart.write_str(sep);
        if v < 10 {
            uart.write_str("0");
        }
        uart.write_u32(u32::from(v));
    }
}
