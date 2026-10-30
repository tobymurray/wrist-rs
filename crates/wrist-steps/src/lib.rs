#![no_std]

//! Today's steps from the IMU's running count.
//!
//! The chip counts from 0 when it is loaded and never resets by itself. The ledger keeps where
//! today began on that count, and banks what an earlier count reached when the chip starts
//! again from 0. A day is a local day number; `None` while the clock is unset, when steps go to
//! whatever day is current and the first set day adopts them.

/// Words a [`Ledger`] keeps across the MCU's resets, the last a check of the others.
pub const WORDS: usize = 6;

const TAG: u32 = 0x5354_0000;
const NO_DAY: u32 = 0xFFFF;
const NO_TOTAL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ledger {
    day: Option<u16>,
    /// The count when today began, or when the chip last started again.
    base: u32,
    /// Today's steps on counts before the current one.
    banked: u32,
    /// The count at the last reading.
    last: u32,
    yesterday: Option<u32>,
}

impl Ledger {
    /// A ledger beginning with `count` on `day`.
    #[must_use]
    pub const fn new(day: Option<u16>, count: u32) -> Self {
        Self { day, base: count, banked: 0, last: count, yesterday: None }
    }

    /// Takes a reading of the chip's count on `day`, and returns today's steps.
    pub fn observe(&mut self, day: Option<u16>, count: u32) -> u32 {
        if count < self.last {
            // The chip started again; what it counted after the last reading is lost.
            self.banked = self.today();
            self.base = 0;
        }
        self.last = count;
        match (self.day, day) {
            (Some(was), Some(now)) if was != now => {
                let total = self.today();
                self.yesterday = (u32::from(was) + 1 == u32::from(now)).then_some(total);
                self.banked = 0;
                self.base = count;
                self.day = day;
            }
            (None, Some(_)) => self.day = day,
            _ => {}
        }
        self.today()
    }

    #[must_use]
    pub const fn today(&self) -> u32 {
        self.banked.saturating_add(self.last.saturating_sub(self.base))
    }

    #[must_use]
    pub const fn yesterday(&self) -> Option<u32> {
        self.yesterday
    }

    #[must_use]
    pub fn save(&self) -> [u32; WORDS] {
        let day = self.day.map_or(NO_DAY, u32::from);
        let mut w = [TAG | day, self.base, self.banked, self.last, self.yesterday.unwrap_or(NO_TOTAL), 0];
        w[WORDS - 1] = check(&w);
        w
    }

    /// The ledger `words` kept. `count_carried_on` says the chip kept counting across the
    /// reset; if it did not, it starts again from 0 and today's steps so far are banked. `None`
    /// when the words are not a ledger's.
    #[must_use]
    pub fn restore(words: &[u32; WORDS], count_carried_on: bool) -> Option<Self> {
        if words[0] & 0xFFFF_0000 != TAG || check(words) != words[WORDS - 1] {
            return None;
        }
        let day = match words[0] & 0xFFFF {
            NO_DAY => None,
            d => Some(u16::try_from(d).ok()?),
        };
        let yesterday = (words[4] != NO_TOTAL).then_some(words[4]);
        let mut ledger = Self { day, base: words[1], banked: words[2], last: words[3], yesterday };
        if !count_carried_on {
            ledger.banked = ledger.today();
            ledger.base = 0;
            ledger.last = 0;
        }
        Some(ledger)
    }
}

fn check(w: &[u32; WORDS]) -> u32 {
    w[..WORDS - 1].iter().fold(0xA5A5_5A5A, |acc, &x| acc.rotate_left(5) ^ x)
}

/// The local day number of `seconds` since 1970 in local time, or `None` outside what a
/// [`Ledger`] keeps.
#[must_use]
pub fn day_of(local_seconds: i64) -> Option<u16> {
    u16::try_from(local_seconds.div_euclid(86_400)).ok().filter(|&d| u32::from(d) != NO_DAY)
}
