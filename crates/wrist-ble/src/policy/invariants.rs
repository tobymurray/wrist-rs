//! The properties the core keeps, checked on the host and alarmed on the device.

use crate::{AdvParams, Instant, Millis};

/// I1: the longest planned gap in connectable advertising with no phone link.
pub const REACHABLE_GAP: Millis = Millis::secs(3);

/// I1: planned gaps may add up to this share of any [`REACHABLE_WINDOW`].
pub const REACHABLE_SHARE_PERCENT: u32 = 5;
pub const REACHABLE_WINDOW: Millis = Millis::secs(60);

/// I1: a gap this long raises the alarm and recovers to the safe baseline.
pub const REACHABLE_ALARM: Millis = Millis::secs(10);

/// I2: chip commands a minute, outside GATT traffic, transfers and each phone link's own asks:
/// its first parameter request and the advertising after it ends, so a phone reconnecting
/// quickly is not taken for the core retrying.
pub const COMMANDS_PER_MINUTE: u32 = 60;

/// The invariants watched on the device, each with an alarm of its own. I3 is not: after the
/// reconciler, the radio is away from the plan only while an operation is asked, in flight or
/// waiting to be retried, which its deadline, the busy budget and the refused baseline end.
pub const WATCHED: [Invariant; 2] = [Invariant::BoundedEffort, Invariant::Reachable];

/// I2's alarm ends once the minute's operations are this far under the limit.
pub const ALARM_HYSTERESIS: u32 = 10;

/// I3: with goals and power still this long, the radio has reached the plan or says why not.
pub const CONVERGE_AFTER: Millis = Millis::secs(15);

/// I4: the status reflects a change of state within this.
pub const STATUS_WITHIN: Millis = Millis::secs(1);

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
    /// Operations asked in the minute to now, past each phone link's own.
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

    /// Whether `broken` is broken at `now`; `told`: its alarm was told and has not ended, so too
    /// many operations ends only [`ALARM_HYSTERESIS`] under the limit.
    #[must_use]
    pub fn broken(&self, broken: Invariant, now: Instant, told: bool) -> bool {
        match broken {
            Invariant::BoundedEffort => self.commands > COMMANDS_PER_MINUTE || (told && self.commands > COMMANDS_PER_MINUTE - ALARM_HYSTERESIS),
            Invariant::Reachable => self.unreachable_from().is_some_and(|t| now >= t),
            _ => false,
        }
    }


}
