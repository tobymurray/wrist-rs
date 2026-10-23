//! The local zone and its next changes, kept in five backup registers.
//!
//! `BKP1R` holds `TZ` and the offset in force, minutes east of UTC, as the low
//! half; `BKP11R` and `BKP13R` hold `T1` and `T2` with each change's new
//! offset, `BKP12R` and `BKP14R` the UTC instant it takes effect, seconds
//! since 1970. A register without its tag is no record.

pub const ZONE_TAG: u32 = 0x545A_0000;
pub const CHANGE_TAGS: [u32; 2] = [0x5431_0000, 0x5432_0000];
const TAG_MASK: u32 = 0xFFFF_0000;

/// An offset change: from `at`, UTC seconds since 1970, the zone is `offset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub at: u32,
    pub offset: i32,
}

/// The offset in force, minutes east of UTC, and up to two changes in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub offset: i32,
    pub next: [Option<Change>; 2],
}

/// Whether `minutes` east of UTC is a zone at all: -14:00 to +14:00.
#[must_use]
pub fn in_zone_range(minutes: i32) -> bool {
    (-14 * 60..=14 * 60).contains(&minutes)
}

fn tagged(tag: u32, minutes: i32) -> u32 {
    tag | u32::from(i16::try_from(minutes).unwrap_or(0).cast_unsigned())
}

fn untag(word: u32, tag: u32) -> Option<i32> {
    if word & TAG_MASK != tag {
        return None;
    }
    let minutes = i32::from(u16::try_from(word & 0xFFFF).unwrap_or(0).cast_signed());
    in_zone_range(minutes).then_some(minutes)
}

impl Zone {
    /// `BKP1R`, `BKP11R` to `BKP14R`.
    #[must_use]
    pub fn encode(&self) -> [u32; 5] {
        let mut w = [tagged(ZONE_TAG, self.offset), 0, 0, 0, 0];
        for (i, c) in self.next.iter().enumerate() {
            if let Some(c) = c {
                w[1 + 2 * i] = tagged(CHANGE_TAGS[i], c.offset);
                w[2 + 2 * i] = c.at;
            }
        }
        w
    }

    /// `None` without the `TZ` tag; a change slot without its tag is empty.
    #[must_use]
    pub fn decode(w: [u32; 5]) -> Option<Self> {
        let offset = untag(w[0], ZONE_TAG)?;
        let mut next = [None; 2];
        let mut n = 0;
        for i in 0..2 {
            if let Some(o) = untag(w[1 + 2 * i], CHANGE_TAGS[i]) {
                next[n] = Some(Change { at: w[2 + 2 * i], offset: o });
                n += 1;
            }
        }
        Some(Self { offset, next })
    }

    /// `<offset> [<at> <offset>]...` in minutes and UTC seconds since 1970,
    /// at most two changes, in order.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        let offset: i32 = parts.next()?.parse().ok()?;
        if !in_zone_range(offset) {
            return None;
        }
        let mut next = [None; 2];
        let mut last = None;
        for slot in &mut next {
            let Some(at) = parts.next() else { break };
            let at: u32 = at.parse().ok()?;
            let o: i32 = parts.next()?.parse().ok()?;
            if !in_zone_range(o) || last.is_some_and(|l| at <= l) {
                return None;
            }
            last = Some(at);
            *slot = Some(Change { at, offset: o });
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Self { offset, next })
    }

    /// The first change due at local time `local` (seconds since 1970 under
    /// the offset in force): the minutes to move the calendar by and the
    /// record after it, which no longer holds that change.
    #[must_use]
    pub fn due(&self, local: i64) -> Option<(i32, Self)> {
        let first = self.next[0]?;
        if local - i64::from(self.offset) * 60 < i64::from(first.at) {
            return None;
        }
        let after = Self { offset: first.offset, next: [self.next[1], None] };
        Some((first.offset - self.offset, after))
    }
}
