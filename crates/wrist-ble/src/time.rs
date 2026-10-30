use core::ops::{Add, Sub};

/// Milliseconds since boot on the core's monotonic clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Millis(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Micros(pub u32);

impl Millis {
    #[must_use]
    pub const fn secs(s: u32) -> Self {
        Self(s * 1_000)
    }

    #[must_use]
    pub const fn mins(m: u32) -> Self {
        Self(m * 60_000)
    }
}

impl Instant {
    /// Zero if `earlier` is later; saturates at `u32::MAX` ms.
    #[must_use]
    pub fn since(self, earlier: Instant) -> Millis {
        Millis(u32::try_from(self.0.saturating_sub(earlier.0)).unwrap_or(u32::MAX))
    }
}

impl Add<Millis> for Instant {
    type Output = Instant;

    fn add(self, d: Millis) -> Instant {
        Instant(self.0 + u64::from(d.0))
    }
}

impl Sub<Millis> for Instant {
    type Output = Instant;

    fn sub(self, d: Millis) -> Instant {
        Instant(self.0.saturating_sub(u64::from(d.0)))
    }
}

impl Add for Millis {
    type Output = Millis;

    fn add(self, d: Millis) -> Millis {
        Millis(self.0.saturating_add(d.0))
    }
}
