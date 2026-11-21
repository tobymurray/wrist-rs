//! The properties the core keeps, checked on the host and alarmed on the device.

use crate::{AdvParams, Instant, Millis};

/// I1: the longest planned gap in connectable advertising with no phone link.
pub const REACHABLE_GAP: Millis = Millis::secs(3);

/// I1: planned gaps may add up to this share of any [`REACHABLE_WINDOW`].
pub const REACHABLE_SHARE_PERCENT: u32 = 5;
pub const REACHABLE_WINDOW: Millis = Millis::secs(60);

/// I1: a gap this long raises the alarm and recovers to the safe baseline.
pub const REACHABLE_ALARM: Millis = Millis::secs(10);

/// I2: chip commands a minute, outside GATT traffic and transfers.
pub const COMMANDS_PER_MINUTE: u32 = 60;

/// I3: with goals and power still this long, the radio has reached the plan or says why not.
pub const CONVERGE_AFTER: Millis = Millis::secs(15);

/// I4: the status reflects a change of state within this.
pub const STATUS_WITHIN: Millis = Millis::secs(1);

/// An alarm not cleared by a resync this long after it was raised restarts the chip.
pub const ALARM_RESTART_AFTER: Millis = CONVERGE_AFTER;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Invariant {
    Reachable,
    BoundedEffort,
    Converges,
    TruthfulStatus,
    SecurityOrder,
    PhoneFirst,
    NothingSilent,
    NoExecutorHolds,
    PeripheralsEnd,
}

/// What the core's own record says, for judging I1–I3 on the device, where there is no other.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Record {
    /// The chip is up and the plan feasible.
    pub ready: bool,
    pub radio_on: bool,
    pub phone_linked: bool,
    /// The advertising the plan wants and the advertising the core's record has.
    pub planned: Option<AdvParams>,
    pub observed: Option<AdvParams>,
    /// When connectable advertising last stopped, and when goals or power last changed.
    pub silent_since: Instant,
    pub settled_since: Instant,
    /// No operation asked, in flight or waiting to be retried.
    pub idle: bool,
    /// Operations asked in the minute to now.
    pub commands: u32,
}

impl Record {
    /// When I1's alarm is due: the plan wants connectable advertising, the record has none,
    /// and no phone is linked, counted from when advertising stopped or the goals last changed,
    /// whichever came later, so time hidden on purpose is not counted.
    #[must_use]
    pub fn unreachable_from(&self) -> Option<Instant> {
        let connectable = |a: Option<AdvParams>| a.is_some_and(|a| a.connectable);
        let owed = self.ready && self.radio_on && !self.phone_linked && connectable(self.planned) && !connectable(self.observed);
        owed.then(|| self.silent_since.max(self.settled_since) + REACHABLE_ALARM)
    }

    /// When I3's alarm is due: with nothing in flight, the record's advertising is not the plan's.
    #[must_use]
    pub fn unconverged_from(&self) -> Option<Instant> {
        (self.ready && self.idle && self.planned != self.observed).then(|| self.settled_since + CONVERGE_AFTER)
    }

    /// The invariant broken at `now`, if any.
    #[must_use]
    pub fn breach(&self, now: Instant) -> Option<Invariant> {
        if self.commands > COMMANDS_PER_MINUTE {
            return Some(Invariant::BoundedEffort);
        }
        if self.unreachable_from().is_some_and(|t| now >= t) {
            return Some(Invariant::Reachable);
        }
        self.unconverged_from().filter(|&t| now >= t).map(|_| Invariant::Converges)
    }
}
