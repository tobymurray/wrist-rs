//! The daily alarm's record in one backup register, and [`Alarm::check`], which decides at each
//! check whether it rings.

use crate::Civil;

pub const ALARM_TAG: u32 = 0x414C_0000;
const TAG_MASK: u32 = 0xFFFF_0000;
const MISSED: u32 = 1 << 13;
const SPENT: u32 = 1 << 12;
const ON: u32 = 1 << 11;
const MINUTE: u32 = 0x7FF;

/// Minutes in the calendar's day.
pub const DAY: u16 = 1440;
/// How far behind its minute a check may find the alarm and still ring it, as after a restart
/// across it.
pub const GRACE: u16 = 5;
/// The longest forward change of zone over the alarm's minute that still rings it at the change.
pub const LATE: u16 = 120;
/// How far behind its minute a check may find the alarm and still act on it, ringing it or marking
/// it missed.
pub const WINDOW: u16 = 720;
/// The longest stretch a backward change repeats.
pub const REPEAT: u16 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alarm {
    /// The minute of the local day, 0 to 1439.
    pub at: u16,
    pub on: bool,
    /// Rung, missed, or set inside its window, since the calendar was last well clear of it.
    pub spent: bool,
    /// Found too late to ring, and not yet seen: the face says so until a press.
    pub missed: bool,
}

/// What a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Nothing,
    Ring,
    Missed,
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
        (hour < 24 && minute < 60).then_some(Self { at: u16::from(hour) * 60 + u16::from(minute), on: true, spent: false, missed: false })
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
        ALARM_TAG
            | if self.missed { MISSED } else { 0 }
            | if self.spent { SPENT } else { 0 }
            | if self.on { ON } else { 0 }
            | u32::from(self.at)
    }

    /// `None` without the `AL` tag, with a reserved bit set, or with no minute of the day.
    #[must_use]
    pub fn decode(w: u32) -> Option<Self> {
        if w & TAG_MASK != ALARM_TAG || w & !(TAG_MASK | MISSED | SPENT | ON | MINUTE) != 0 {
            return None;
        }
        let at = u16::try_from(w & MINUTE).ok().filter(|&m| m < DAY)?;
        Some(Self { at, on: w & ON != 0, spent: w & SPENT != 0, missed: w & MISSED != 0 })
    }

    /// Minutes the calendar's minute `now` is past the alarm's, 0 to 1439.
    fn past(self, now: u16) -> u16 {
        (now % DAY + DAY - self.at) % DAY
    }

    /// The record as set at minute `now`, spent through its window and the stretch after so that
    /// setting it neither rings it nor marks it missed.
    #[must_use]
    pub fn settled(self, now: u16) -> Self {
        Self { spent: self.past(now) < WINDOW + REPEAT, ..self }
    }

    /// The check as the calendar shows minute `now`, `jumped` minutes after a forward change of
    /// zone moved it there (0 for any other check): what it found, and the record after it,
    /// which the caller keeps before it rings.
    #[must_use]
    pub fn check(self, now: u16, jumped: u16) -> (Found, Self) {
        if !self.on {
            return (Found::Nothing, self);
        }
        let past = self.past(now);
        if past < WINDOW {
            if self.spent {
                return (Found::Nothing, self);
            }
            // The change left the calendar on minute `now - jumped`, then showed `now`.
            let skipped = past <= jumped && jumped <= LATE;
            let found = if past < GRACE || skipped { Found::Ring } else { Found::Missed };
            return (found, Self { spent: true, missed: found == Found::Missed, ..self });
        }
        if self.spent && (WINDOW + REPEAT..DAY - REPEAT).contains(&past) {
            return (Found::Nothing, Self { spent: false, ..self });
        }
        (Found::Nothing, self)
    }

    /// Minutes from `now` to the next check that could act, the calendar unmoved: 0 while a check
    /// now would, `None` while it is off.
    #[must_use]
    pub fn next_check(self, now: u16) -> Option<u16> {
        if !self.on {
            return None;
        }
        let past = self.past(now);
        let clears = (WINDOW + REPEAT..DAY - REPEAT).contains(&past);
        if (!self.spent && past < WINDOW) || (self.spent && clears) {
            return Some(0);
        }
        if self.spent && past < WINDOW + REPEAT {
            return Some(WINDOW + REPEAT - past);
        }
        // Not spent, or spent through an autumn hour: its own minute.
        Some(DAY - past)
    }

    /// Minutes from `now` to the check that rings it, the calendar unmoved: 0 for a check now,
    /// `None` while it is off.
    #[must_use]
    pub fn minutes_to_ring(self, now: u16) -> Option<u16> {
        if !self.on {
            return None;
        }
        let past = self.past(now);
        if !self.spent && past < GRACE {
            return Some(0);
        }
        let until = (DAY - past) % DAY;
        // Spent, and not cleared before its minute comes: the day after.
        if self.spent && (until == 0 || past >= DAY - REPEAT) {
            return Some(until + DAY);
        }
        // Missed at the next check, if it is in its window: tomorrow's.
        Some(until)
    }
}
