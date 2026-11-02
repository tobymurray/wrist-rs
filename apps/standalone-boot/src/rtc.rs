//! The calendar in the RTC (RM0456 63) and its backup domain (RM0456 11.8.49).

use core::sync::atomic::{AtomicU8, Ordering};

use crate::mmio::{
    enable_clock, modify32, read32, write32, RCC_AHB3ENR, RCC_AHB3ENR_PWREN, RCC_APB3ENR,
    RCC_APB3ENR_RTCAPBEN, RCC_BDCR, RTC_BASE,
};
use crate::uart::Uart;

const RTC_TR: u32 = RTC_BASE;
const RTC_DR: u32 = RTC_BASE + 0x04;
const RTC_SSR: u32 = RTC_BASE + 0x08;
const RTC_ICSR: u32 = RTC_BASE + 0x0C;
const RTC_PRER: u32 = RTC_BASE + 0x10;
const RTC_CR: u32 = RTC_BASE + 0x18;
const RTC_WPR: u32 = RTC_BASE + 0x24;
const RTC_SCR: u32 = RTC_BASE + 0x5C;
const WPR_KEY1: u32 = 0xCA;
const WPR_KEY2: u32 = 0x53;
const WPR_LOCK: u32 = 0xFF;
const TAMP_BKP0R: u32 = 0x4600_7C00 + 0x100;
const TAMP_BKP1R: u32 = 0x4600_7C00 + 0x104;
const TAMP_BKP2R: u32 = 0x4600_7C00 + 0x108;
/// The alarm (`wrist_time::alarm`).
const TAMP_BKP4R: u32 = 0x4600_7C00 + 0x110;
/// Not `BKP3R` or `BKP5R` (`HARDWARE.md`, "Debug and boot").
const TAMP_BKP31R: u32 = 0x4600_7C00 + 0x17C;
const SOURCE_TAG: u32 = 0x5352_0000;
/// The zone's next changes (`wrist_time::zone`).
const TAMP_BKP11R: u32 = 0x4600_7C00 + 0x12C;
const PWR_DBPR: u32 = 0x4602_0800 + 0x28;
const DBPR_DBP: u32 = 1 << 0;

const BDCR_LSEON: u32 = 1 << 0;
const BDCR_LSERDY: u32 = 1 << 1;
const BDCR_LSEBYP: u32 = 1 << 2;
const BDCR_LSESYSEN: u32 = 1 << 7;
const BDCR_RTCSEL_MASK: u32 = 0b11 << 8;
const BDCR_RTCSEL_LSE: u32 = 0b01 << 8;
const BDCR_RTCSEL_LSI: u32 = 0b10 << 8;
const BDCR_RTCEN: u32 = 1 << 15;
const BDCR_LSION: u32 = 1 << 26;
const BDCR_LSIRDY: u32 = 1 << 27;
const ICSR_INITS: u32 = 1 << 4;
const ICSR_RSF: u32 = 1 << 5;
const ICSR_INITF: u32 = 1 << 6;
const ICSR_INIT: u32 = 1 << 7;
const TR_PM: u32 = 1 << 22;
/// Alarm A: `RTC_ALRMAR`, `RTC_ALRMASSR`, `RTC_SR` (RM0456 63.6, `stm32-metapac`'s
/// `rtc_v3`); bit 0 of `SR` and `SCR` is its flag. Install 61 matched with these.
const RTC_ALRMAR: u32 = RTC_BASE + 0x40;
const RTC_ALRMASSR: u32 = RTC_BASE + 0x44;
const RTC_SR: u32 = RTC_BASE + 0x50;
const CR_ALRAE: u32 = 1 << 8;
const CR_ALRAIE: u32 = 1 << 12;
const SR_ALRAF: u32 = 1 << 0;
/// `MSK4`: the date and weekday don't count, so it matches every day.
const ALRMAR_MSK4: u32 = 1 << 31;
/// `RCC_APB3SMENR` and `RCC_SRDAMR`, bit 21 of each: the RTC's bus clock in Stop, which it
/// needs to wake the core from Stop (RM0456 11.8; `stm32-metapac` 21's `rcc_u5`).
const RCC_APB3SMENR: u32 = crate::mmio::RCC_BASE + 0xD0;
const RCC_SRDAMR: u32 = crate::mmio::RCC_BASE + 0xD8;
const RTCAPB_IN_STOP: u32 = 1 << 21;

/// `RTC_PRER` for a calendar that divides its clock by `a * s`.
const fn prer(a: u32, s: u32) -> u32 {
    ((a - 1) << 16) | (s - 1)
}
const PRER_LSE: u32 = prer(128, 256);
const PRER_LSI: u32 = prer(128, 250);

static SOURCE: AtomicU8 = AtomicU8::new(SRC_NONE);
const SRC_NONE: u8 = 0;
const SRC_LSE: u8 = 1;
const SRC_LSI: u8 = 2;

pub use wrist_time::alarm::Alarm;
pub use wrist_time::zone::Zone;
pub use wrist_time::{civil_of_seconds, parse_civil, seconds_of, Civil};

const CYCLES_PER_MS: u32 = 48_000;
const LSE_START_MS: u32 = 2000;

fn write_bcd(uart: Uart, value: u32) {
    uart.write_byte(b'0' + u8::try_from((value >> 4) & 0xF).unwrap_or(0));
    uart.write_byte(b'0' + u8::try_from(value & 0xF).unwrap_or(0));
}

fn to_bcd(v: u8) -> u32 {
    u32::from(v / 10) << 4 | u32::from(v % 10)
}

fn from_bcd(v: u32) -> u8 {
    u8::try_from(((v >> 4) & 0xF) * 10 + (v & 0xF)).unwrap_or(99)
}

fn clock_bus() {
    unsafe { modify32(RCC_APB3ENR, 0, RCC_APB3ENR_RTCAPBEN) };
    cortex_m::asm::delay(8); // RM0456 63.3.3
}

/// True if the calendar runs; its shadow registers are then a copy made after the last Stop
/// exit, before which they can be a whole sleep stale (RM0456 63.3.10).
fn calendar_ready() -> bool {
    /// A copy takes at most two LSE cycles.
    const RSF_POLLS: u32 = 10_000;
    if unsafe { read32(RCC_BDCR) } & BDCR_RTCEN == 0 {
        return false;
    }
    clock_bus();
    if unsafe { read32(RTC_ICSR) } & ICSR_INITS == 0 {
        return false;
    }
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(RTC_WPR, WPR_KEY1);
        write32(RTC_WPR, WPR_KEY2);
        modify32(RTC_ICSR, ICSR_RSF, 0);
        write32(RTC_WPR, WPR_LOCK);
        for _ in 0..RSF_POLLS {
            if read32(RTC_ICSR) & ICSR_RSF != 0 {
                break;
            }
        }
    }
    true
}

fn wait_for(reg: u32, mask: u32, ms: u32) -> bool {
    wait_for_at(reg, mask, ms, CYCLES_PER_MS)
}

fn wait_for_at(reg: u32, mask: u32, ms: u32, cycles_per_ms: u32) -> bool {
    for _ in 0..ms {
        if unsafe { read32(reg) } & mask != 0 {
            return true;
        }
        cortex_m::asm::delay(cycles_per_ms);
    }
    false
}

/// At boot, before the HAL's clock configuration copies the backup domain: the LSE in bypass if
/// it is off, and the RTC started as [`start`] would, so nothing later changes what was copied.
/// `LSESYSEN` is cleared if the LSE is not running, since the HAL would wait on it forever.
pub fn prepare(cycles_per_ms: u32) -> Result<&'static str, &'static str> {
    // `PWR_DBPR` takes writes only with the PWR clock on, which the HAL turns on later.
    unsafe { enable_clock(RCC_AHB3ENR, RCC_AHB3ENR_PWREN) };
    clock_bus();
    unsafe { write32(PWR_DBPR, DBPR_DBP) };
    // With the RTC already on, `start` leaves the LSE alone.
    if unsafe { read32(RCC_BDCR) } & (BDCR_RTCEN | BDCR_LSEON) == BDCR_RTCEN {
        unsafe { modify32(RCC_BDCR, 0, BDCR_LSEBYP) };
        unsafe { modify32(RCC_BDCR, 0, BDCR_LSEON) };
        wait_for_at(RCC_BDCR, BDCR_LSERDY, LSE_START_MS, cycles_per_ms);
    }
    let started = start_at(cycles_per_ms);
    if unsafe { read32(RCC_BDCR) } & BDCR_LSERDY == 0 {
        unsafe { modify32(RCC_BDCR, BDCR_LSESYSEN, 0) };
    }
    started
}

/// Starts the RTC once, on the LSE or else the LSI, and names the one it runs from; the first call
/// blocks while the LSE starts.
pub fn start() -> Result<&'static str, &'static str> {
    start_at(CYCLES_PER_MS)
}

fn start_at(cycles_per_ms: u32) -> Result<&'static str, &'static str> {
    clock_bus();
    unsafe { write32(PWR_DBPR, DBPR_DBP) };
    let bdcr = unsafe { read32(RCC_BDCR) };
    if bdcr & BDCR_RTCEN != 0 {
        let src = match bdcr & BDCR_RTCSEL_MASK {
            BDCR_RTCSEL_LSE => SRC_LSE,
            BDCR_RTCSEL_LSI => SRC_LSI,
            _ => return Err("RTC enabled on a clock this image did not choose"),
        };
        SOURCE.store(src, Ordering::Relaxed);
        return Ok(source_name(src));
    }
    if bdcr & BDCR_LSEON == 0 {
        // LSEBYP is writable only while the LSE is off (RM0456 11.8.49).
        unsafe { modify32(RCC_BDCR, 0, BDCR_LSEBYP) };
        unsafe { modify32(RCC_BDCR, 0, BDCR_LSEON) };
    }
    let (sel, src) = if wait_for_at(RCC_BDCR, BDCR_LSERDY, LSE_START_MS, cycles_per_ms) {
        (BDCR_RTCSEL_LSE, SRC_LSE)
    } else {
        unsafe { modify32(RCC_BDCR, 0, BDCR_LSION) };
        if !wait_for_at(RCC_BDCR, BDCR_LSIRDY, 100, cycles_per_ms) {
            return Err("neither LSE nor LSI came ready");
        }
        (BDCR_RTCSEL_LSI, SRC_LSI)
    };
    unsafe { modify32(RCC_BDCR, BDCR_RTCSEL_MASK, sel) };
    unsafe { modify32(RCC_BDCR, 0, BDCR_RTCEN) };
    SOURCE.store(src, Ordering::Relaxed);
    Ok(source_name(src))
}

fn source_name(src: u8) -> &'static str {
    match src {
        SRC_LSE => "LSE (SiT1532 through the bypass)",
        SRC_LSI => "LSI (the 32 kHz RC; no 32 kHz input came ready)",
        _ => "none",
    }
}

/// 1 = Monday to 7 = Sunday (Sakamoto's method).
fn weekday(year: u16, month: u8, day: u8) -> u32 {
    const T: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = usize::from(month.clamp(1, 12));
    let y = u32::from(year) - u32::from(month < 3);
    let w = (y + y / 4 - y / 100 + y / 400 + T[m - 1] + u32::from(day)) % 7;
    if w == 0 {
        7
    } else {
        w
    }
}

pub fn set(c: Civil) -> Result<(), &'static str> {
    if !(2000..=2099).contains(&c.year)
        || !(1..=12).contains(&c.month)
        || !(1..=31).contains(&c.day)
        || c.hour > 23
        || c.minute > 59
        || c.second > 59
    {
        return Err("out of range: YYYY-MM-DD HH:MM:SS, years 2000 to 2099");
    }
    let src = SOURCE.load(Ordering::Relaxed);
    if src == SRC_NONE {
        return Err("RTC not started");
    }
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(RTC_WPR, WPR_KEY1);
        write32(RTC_WPR, WPR_KEY2);
        modify32(RTC_ICSR, 0, ICSR_INIT);
    }
    if !wait_for(RTC_ICSR, ICSR_INITF, 100) {
        unsafe { write32(RTC_WPR, WPR_LOCK) };
        return Err("RTC never entered initialisation mode");
    }
    let year = u8::try_from(c.year - 2000).unwrap_or(0);
    unsafe {
        write32(RTC_PRER, if src == SRC_LSE { PRER_LSE } else { PRER_LSI });
        // 24-hour format, and nothing else on.
        write32(RTC_CR, 0);
        write32(
            RTC_TR,
            to_bcd(c.hour) << 16 | to_bcd(c.minute) << 8 | to_bcd(c.second),
        );
        write32(
            RTC_DR,
            to_bcd(year) << 16
                | weekday(c.year, c.month, c.day) << 13
                | to_bcd(c.month) << 8
                | to_bcd(c.day),
        );
        modify32(RTC_ICSR, ICSR_INIT, 0);
        write32(RTC_WPR, WPR_LOCK);
    }
    Ok(())
}

pub fn write_report(uart: Uart) {
    // The loader's mailbox (`apps/wrist-loader/LAYOUT.md`).
    const MAILBOX_MAGIC: u32 = 0x2026_FFF0;
    const MAILBOX_BDCR: u32 = 0x2026_FFF4;
    const MAILBOX_TAG: u32 = 0x4C44_4352;
    if unsafe { read32(MAILBOX_MAGIC) } == MAILBOX_TAG {
        uart.write_reg("  RCC_BDCR at the loader's entry = ", unsafe {
            read32(MAILBOX_BDCR)
        });
    } else {
        uart.write_line("  (no loader mailbox: a loader too old to leave one)");
    }
    let bdcr = unsafe { read32(RCC_BDCR) };
    uart.write_reg("  RCC_BDCR  = ", bdcr);
    uart.write_str("  LSE ");
    uart.write_str(match (bdcr & BDCR_LSEON != 0, bdcr & BDCR_LSERDY != 0) {
        (true, true) => "on and ready",
        (true, false) => "on, NOT ready",
        (false, _) => "off",
    });
    if bdcr & BDCR_LSEBYP != 0 {
        uart.write_str(" (bypass)");
    }
    uart.write_str("; LSI ");
    uart.write_str(match (bdcr & BDCR_LSION != 0, bdcr & BDCR_LSIRDY != 0) {
        (true, true) => "on and ready",
        (true, false) => "on, not ready",
        (false, true) => "ready (the watchdog's)",
        (false, false) => "off",
    });
    uart.write_str("; RTCSEL ");
    uart.write_str(match bdcr & BDCR_RTCSEL_MASK {
        0 => "none",
        BDCR_RTCSEL_LSE => "LSE",
        BDCR_RTCSEL_LSI => "LSI",
        _ => "HSE/32",
    });
    uart.write_str("; RTCEN ");
    uart.write_line(if bdcr & BDCR_RTCEN != 0 { "1" } else { "0" });

    clock_bus();
    let icsr = unsafe { read32(RTC_ICSR) };
    let prer = unsafe { read32(RTC_PRER) };
    let cr = unsafe { read32(RTC_CR) };
    let ssr = unsafe { read32(RTC_SSR) };
    let tr = unsafe { read32(RTC_TR) };
    let dr = unsafe { read32(RTC_DR) };
    uart.write_reg("  RTC_ICSR  = ", icsr);
    uart.write_reg("  RTC_PRER  = ", prer);
    uart.write_reg("  RTC_CR    = ", cr);
    uart.write_reg("  TAMP_BKP0R= ", unsafe { read32(TAMP_BKP0R) });
    write_sources(uart);
    uart.write_reg("  RTC_TR    = ", tr);
    uart.write_reg("  RTC_DR    = ", dr);
    uart.write_reg("  RTC_SSR   = ", ssr);
    uart.write_str("  calendar ");
    if icsr & ICSR_INITS == 0 {
        uart.write_line(
            "never initialised (INITS = 0): no time is kept; `time set YYYY-MM-DD HH:MM:SS`",
        );
        return;
    }
    uart.write_str(if icsr & ICSR_RSF != 0 {
        "set; shadow registers synchronised: 20"
    } else {
        "set; shadow registers NOT yet synchronised, values may be stale: 20"
    });
    write_bcd(uart, dr >> 16);
    uart.write_byte(b'-');
    write_bcd(uart, (dr >> 8) & 0x1F);
    uart.write_byte(b'-');
    write_bcd(uart, dr & 0x3F);
    uart.write_byte(b' ');
    write_bcd(uart, (tr >> 16) & 0x3F);
    uart.write_byte(b':');
    write_bcd(uart, (tr >> 8) & 0x7F);
    uart.write_byte(b':');
    write_bcd(uart, tr & 0x7F);
    uart.write_str(if tr & TR_PM != 0 { " PM" } else { "" });
    uart.write_str("  (weekday ");
    uart.write_u32((dr >> 13) & 0b111);
    uart.write_line(", 1 = Monday)");
}

fn write_sources(uart: Uart) {
    uart.write_str("  zone      : ");
    match zone() {
        Some(z) => {
            write_zone(uart, z.offset);
            uart.write_line("");
            for c in z.next.iter().flatten() {
                uart.write_str("  change    : ");
                write_civil(uart, civil_of_seconds(i64::from(c.at)));
                uart.write_str(" UTC, to ");
                write_zone(uart, c.offset);
                uart.write_line("");
            }
        }
        None => uart.write_line("no record: daylight saving is not followed (tools/wrist-time.py)"),
    }
    uart.write_str("  last set  : ");
    match last_set() {
        Some((source, at)) => {
            uart.write_str("by ");
            uart.write_str(source.name());
            uart.write_str(", to ");
            write_civil(uart, at);
            uart.write_line(" local");
        }
        None => uart.write_line("no record (the backup domain was reset)"),
    }
    uart.write_str("  alarm     : ");
    crate::alarm::write_record(uart);
    uart.write_line("");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Host = 1,
    Gps = 2,
}

impl Source {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Host),
            2 => Some(Self::Gps),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Host => "the host (time set)",
            Self::Gps => "GPS",
        }
    }
}

pub fn note_set(source: Source, local: Civil) {
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(TAMP_BKP2R, SOURCE_TAG | source as u32);
        write32(TAMP_BKP31R, u32::try_from(seconds_of(local)).unwrap_or(0));
    }
}

/// Who last set the calendar, and to what.
#[must_use]
pub fn last_set() -> Option<(Source, Civil)> {
    clock_bus();
    let w = unsafe { read32(TAMP_BKP2R) };
    if w & 0xFFFF_0000 != SOURCE_TAG {
        return None;
    }
    let source = Source::from_u8(u8::try_from(w & 0xFF).unwrap_or(0))?;
    let at = i64::from(unsafe { read32(TAMP_BKP31R) });
    Some((source, civil_of_seconds(at)))
}

fn zone_words() -> [u32; 5] {
    clock_bus();
    let at = |i: u32| unsafe { read32(TAMP_BKP11R + 4 * i) };
    [unsafe { read32(TAMP_BKP1R) }, at(0), at(1), at(2), at(3)]
}

/// The zone record, if `time zone` left one since the backup domain was last reset.
#[must_use]
pub fn zone() -> Option<Zone> {
    Zone::decode(zone_words())
}

pub fn set_zone(z: &Zone) {
    let w = z.encode();
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(TAMP_BKP1R, w[0]);
        for (i, &v) in (0u32..).zip(&w[1..]) {
            write32(TAMP_BKP11R + 4 * i, v);
        }
    }
}

/// `BKP4R` as it stands.
#[must_use]
pub fn alarm_word() -> u32 {
    clock_bus();
    unsafe { read32(TAMP_BKP4R) }
}

/// The alarm, if `alarm` set one since the backup domain was last reset.
#[must_use]
pub fn alarm() -> Option<Alarm> {
    Alarm::decode(alarm_word())
}

pub fn set_alarm(a: Alarm) {
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(TAMP_BKP4R, a.encode());
    }
}

/// `BKP16R`: tag `BL` and 1 while the radio should be on, so it comes back after a reset.
#[cfg(feature = "ble")]
const TAMP_BKP16R: u32 = 0x4600_7C00 + 0x140;
#[cfg(feature = "ble")]
const RADIO_ON: u32 = 0x424C_0001;

/// `ble on` was the last word, since the backup domain was last reset.
#[cfg(feature = "ble")]
#[must_use]
pub fn radio_wanted() -> bool {
    clock_bus();
    unsafe { read32(TAMP_BKP16R) == RADIO_ON }
}

#[cfg(feature = "ble")]
pub fn set_radio_wanted(on: bool) {
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(TAMP_BKP16R, if on { RADIO_ON } else { 0 });
    }
}

pub fn clear_zone() {
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(TAMP_BKP1R, 0);
        for i in 0..4 {
            write32(TAMP_BKP11R + 4 * i, 0);
        }
    }
}

/// Moves the calendar by the zone's first change once it is due; the minutes moved.
///
/// # Errors
/// The calendar's own, as for `time set`; the record is then kept for the next try.
pub fn apply_zone_change() -> Result<Option<i32>, &'static str> {
    let Some(z) = zone() else { return Ok(None) };
    let Some(local) = now_civil() else { return Ok(None) };
    let Some((shift, after)) = z.due(seconds_of(local)) else { return Ok(None) };
    // The calendar runs, so this only notes its clock.
    start()?;
    set(civil_of_seconds(seconds_of(local) + i64::from(shift) * 60))?;
    set_zone(&after);
    Ok(Some(shift))
}

pub fn write_zone(uart: Uart, minutes: i32) {
    if minutes == 0 {
        uart.write_str("UTC");
        return;
    }
    uart.write_str(if minutes < 0 { "-" } else { "+" });
    let a = minutes.unsigned_abs();
    for (sep, v) in [("", a / 60), (":", a % 60)] {
        uart.write_str(sep);
        if v < 10 {
            uart.write_str("0");
        }
        uart.write_u32(v);
    }
}

/// Arms alarm A for `hh:mm:ss` every day, with its interrupt, the RTC's wake from Stop. A
/// `time set` (`set`) clears it with the rest of `RTC_CR`.
pub fn arm_alarm_a(hour: u8, minute: u8, second: u8) {
    clock_bus();
    unsafe {
        modify32(RCC_APB3SMENR, 0, RTCAPB_IN_STOP);
        modify32(RCC_SRDAMR, 0, RTCAPB_IN_STOP);
        write32(PWR_DBPR, DBPR_DBP);
        write32(RTC_WPR, WPR_KEY1);
        write32(RTC_WPR, WPR_KEY2);
        // ALRMAR is written only with ALRAE clear (RM0456, RTC alarm).
        modify32(RTC_CR, CR_ALRAE | CR_ALRAIE, 0);
        write32(
            RTC_ALRMAR,
            ALRMAR_MSK4 | to_bcd(hour.min(23)) << 16 | to_bcd(minute.min(59)) << 8 | to_bcd(second.min(59)),
        );
        // MASKSS 0: no sub-second compare, so it matches as the second turns.
        write32(RTC_ALRMASSR, 0);
        write32(RTC_SCR, SR_ALRAF);
        modify32(RTC_CR, 0, CR_ALRAE | CR_ALRAIE);
        write32(RTC_WPR, WPR_LOCK);
    }
}

/// The `hh:mm:ss` alarm A is armed for, or `None` while it is off.
#[must_use]
pub fn alarm_a() -> Option<(u8, u8, u8)> {
    clock_bus();
    let (cr, alrmar) = unsafe { (read32(RTC_CR), read32(RTC_ALRMAR)) };
    (cr & (CR_ALRAE | CR_ALRAIE) == CR_ALRAE | CR_ALRAIE)
        .then(|| (from_bcd((alrmar >> 16) & 0x3F), from_bcd((alrmar >> 8) & 0x7F), from_bcd(alrmar & 0x7F)))
}

/// Whether alarm A's flag is up, clearing it.
pub fn take_alarm_a() -> bool {
    clock_bus();
    let up = unsafe { read32(RTC_SR) } & SR_ALRAF != 0;
    if up {
        unsafe { write32(RTC_SCR, SR_ALRAF) };
    }
    up
}

pub fn disarm_alarm_a() {
    clock_bus();
    unsafe {
        write32(PWR_DBPR, DBPR_DBP);
        write32(RTC_WPR, WPR_KEY1);
        write32(RTC_WPR, WPR_KEY2);
        modify32(RTC_CR, CR_ALRAE | CR_ALRAIE, 0);
        write32(RTC_SCR, SR_ALRAF);
        write32(RTC_WPR, WPR_LOCK);
    }
}

/// `None` while the RTC is stopped or was never set.
#[must_use]
pub fn now_civil() -> Option<Civil> {
    if !calendar_ready() {
        return None;
    }
    let _ssr = unsafe { read32(RTC_SSR) };
    let tr = unsafe { read32(RTC_TR) };
    let dr = unsafe { read32(RTC_DR) };
    let mut hour = from_bcd((tr >> 16) & 0x3F);
    if tr & TR_PM != 0 {
        hour = (hour + 12) % 24;
    }
    Some(Civil {
        year: 2000 + u16::from(from_bcd((dr >> 16) & 0xFF)),
        month: from_bcd((dr >> 8) & 0x1F),
        day: from_bcd(dr & 0x3F),
        hour,
        minute: from_bcd((tr >> 8) & 0x7F),
        second: from_bcd(tr & 0x7F),
    })
}

/// Milliseconds since the calendar's minute began, while it runs.
pub fn ms_into_minute() -> Option<u32> {
    if !calendar_ready() {
        return None;
    }
    // Reading `SSR` holds `TR` and `DR` until `DR` is read (RM0456 63.3.10).
    let ssr = unsafe { read32(RTC_SSR) } & 0xFFFF;
    let tr = unsafe { read32(RTC_TR) };
    let _dr = unsafe { read32(RTC_DR) };
    let prediv_s = unsafe { read32(RTC_PRER) } & 0x7FFF;
    let fraction_ms = (prediv_s - ssr.min(prediv_s)) * 1000 / (prediv_s + 1);
    Some(u32::from(from_bcd(tr & 0x7F)) * 1000 + fraction_ms)
}

pub fn write_civil(uart: Uart, c: Civil) {
    uart.write_u32(u32::from(c.year));
    for (sep, v) in [("-", c.month), ("-", c.day), (" ", c.hour), (":", c.minute), (":", c.second)] {
        uart.write_str(sep);
        if v < 10 {
            uart.write_str("0");
        }
        uart.write_u32(u32::from(v));
    }
}
