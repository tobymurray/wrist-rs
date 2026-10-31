//! The daily alarm, kept in one backup register, and when it rings.
//!
//! `BKP4R` holds `AL` in the high half; below it, bit 12 while today's ring is spent, bit 11
//! while the alarm is on, and bits 0 to 10 the minute of the local day it rings at, 0 to 1439.
//! Bits 13 to 15 are zero. A register without the tag, or with anything else, is no alarm.
//!
//! The calendar keeps local time and a zone's change moves it ([`crate::zone`]), so the alarm
//! is a minute of the calendar's day, checked as each minute turns. It rings at the first check
//! that finds the calendar up to [`LATE`] minutes past it with its ring not spent: a minute the
//! calendar skips, as in daylight saving's spring hour, still rings it. The ring then stays
//! spent until the calendar is [`LATE`] plus [`REPEAT`] minutes past it, and again from
//! [`REPEAT`] minutes short of it, so neither the hour an autumn change repeats nor a reset in
//! the minute it rang rings it twice. Being in the register, the ring's spent state survives
//! a reset as the time does.

use crate::Civil;

pub const ALARM_TAG: u32 = 0x414C_0000;
const TAG_MASK: u32 = 0xFFFF_0000;
const SPENT: u32 = 1 << 12;
const ON: u32 = 1 << 11;
const MINUTE: u32 = 0x7FF;

/// Minutes in the calendar's day.
pub const DAY: u16 = 1440;
/// How late a check may find the alarm's minute and still ring it: more than any forward
/// change of zone moves the calendar.
pub const LATE: u16 = 120;
/// The longest stretch a backward change repeats.
pub const REPEAT: u16 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alarm {
    /// The minute of the local day, 0 to 1439.
    pub at: u16,
    pub on: bool,
    /// Rung, or set inside its ringing window, since the calendar was last well clear of it.
    pub spent: bool,
}

/// The calendar's minute of the day.
#[must_use]
pub fn minute_of_day(c: Civil) -> u16 {
    u16::from(c.hour.min(23)) * 60 + u16::from(c.minute.min(59))
}

/// `H:MM` or `HH:MM` on the 24-hour clock, with surrounding whitespace ignored.
#[must_use]
pub fn parse_hhmm(text: &str) -> Option<(u8, u8)> {
    let (h, m) = text.trim().split_once(':')?;
    let digits = |s: &str, n: core::ops::RangeInclusive<usize>| n.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(h, 1..=2) || !digits(m, 2..=2) {
        return None;
    }
    let (h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

impl Alarm {
    /// On at `hour:minute`, with its ring not yet spent; `None` for a time that is not one.
    #[must_use]
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { at: u16::from(hour) * 60 + u16::from(minute), on: true, spent: false })
    }

    #[must_use]
    pub fn hour(self) -> u8 {
        u8::try_from(self.at / 60).unwrap_or(0)
    }

    #[must_use]
    pub fn minute(self) -> u8 {
        u8::try_from(self.at % 60).unwrap_or(0)
    }

    #[must_use]
    pub fn encode(self) -> u32 {
        ALARM_TAG | if self.spent { SPENT } else { 0 } | if self.on { ON } else { 0 } | u32::from(self.at)
    }

    /// `None` without the `AL` tag, with a reserved bit set, or with no minute of the day.
    #[must_use]
    pub fn decode(w: u32) -> Option<Self> {
        if w & TAG_MASK != ALARM_TAG || w & !(TAG_MASK | SPENT | ON | MINUTE) != 0 {
            return None;
        }
        let at = u16::try_from(w & MINUTE).ok().filter(|&m| m < DAY)?;
        Some(Self { at, on: w & ON != 0, spent: w & SPENT != 0 })
    }

    /// Minutes the calendar's minute `now` is past the alarm's, 0 to 1439.
    fn past(self, now: u16) -> u16 {
        (now % DAY + DAY - self.at) % DAY
    }

    /// The record as set at the calendar's minute `now`, by the console or by setting the clock:
    /// spent while `now` is in its ringing window or the stretch after it, so setting it does
    /// not ring it at once.
    #[must_use]
    pub fn settled(self, now: u16) -> Self {
        Self { spent: self.past(now) < LATE + REPEAT, ..self }
    }

    /// The check as the calendar shows minute `now`: whether to ring, and the record after it,
    /// which the caller keeps before it rings.
    #[must_use]
    pub fn check(self, now: u16) -> (bool, Self) {
        if !self.on {
            return (false, self);
        }
        let past = self.past(now);
        if past < LATE {
            return (!self.spent, Self { spent: true, ..self });
        }
        if (LATE + REPEAT..DAY - REPEAT).contains(&past) {
            return (false, Self { spent: false, ..self });
        }
        (false, self)
    }

    /// Minutes of the calendar from `now` to the check that rings it, with no change to the
    /// calendar in between; 0 rings at the check of `now`. `None` while it is off.
    #[must_use]
    pub fn minutes_to_ring(self, now: u16) -> Option<u16> {
        if !self.on {
            return None;
        }
        let past = self.past(now);
        if !self.spent && past < LATE {
            return Some(0);
        }
        let until = (DAY - past) % DAY;
        // Spent, and not re-armed before its minute comes: the day after.
        if self.spent && (until == 0 || past >= DAY - REPEAT) {
            return Some(until + DAY);
        }
        Some(until)
    }
}
