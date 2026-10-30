//! The planner's interface: goals in, the radio's target out, chosen only from configurations the
//! driver's [`Schedule`] admits.

use core::hash::{Hash, Hasher};

use wrist_ble::{Activities, AdvParams, Capabilities, Change, ConnParams, Instant, Op, OpId, Schedule};

use crate::goals::{Goals, Power, MAX_PERIPHERALS};
use crate::State;

/// What the planner decided for one goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    NotWanted,
    Fits,
    Busy(Conflict),
}

/// What the radio is busy with instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conflict {
    Phone,
    Peripheral,
    Scan,
    Transfer,
    /// Every option the model admits has been refused by the chip.
    Refused,
    Capacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanHealth {
    Ok,
    Infeasible(Conflict),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub activities: Activities,
    pub phone: Verdict,
    pub peripherals: [Verdict; MAX_PERIPHERALS],
}

/// One option on the driver's menu, with the conditions it is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice<P> {
    pub params: P,
    pub when: When,
    /// Estimated average current above the floor, in µA.
    pub cost_ua: u16,
}

/// `None` matches either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct When {
    pub power: Option<Power>,
    pub beside_central: Option<bool>,
    pub idle: Option<bool>,
}

impl When {
    #[must_use]
    pub fn matches(&self, power: Power, beside_central: bool, idle: bool) -> bool {
        self.power.is_none_or(|p| p == power)
            && self.beside_central.is_none_or(|b| b == beside_central)
            && self.idle.is_none_or(|i| i == idle)
    }
}

/// The driver's options for each activity, most preferred first.
#[derive(Clone, Copy, Debug)]
pub struct Menu {
    pub advertising: &'static [Choice<AdvParams>],
    /// A central link while it connects and discovers.
    pub connecting: &'static [Choice<ConnParams>],
    /// A central link once it delivers readings.
    pub live: &'static [Choice<ConnParams>],
}

pub struct PlanInput<'a> {
    pub goals: &'a Goals,
    pub state: &'a State,
    pub caps: &'a Capabilities,
    pub schedule: &'a dyn Schedule,
    pub menu: &'a Menu,
    pub refused: &'a Refused,
    pub now: Instant,
}

/// A pure function from goals and what is running to the target.
pub trait Planner {
    fn plan(&self, input: &PlanInput<'_>) -> Target;
}

/// An operation in flight, until its terminal event or its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub id: OpId,
    pub op: Op,
    pub deadline: Instant,
}

/// Issues at most one operation at a time towards the target.
pub trait Reconciler {
    fn next(&mut self, target: &Target, state: &State, pending: Option<&Pending>, now: Instant) -> Option<Op>;
}

/// A refused configuration: the activities and the change the chip would not take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub u64);

#[must_use]
pub fn fingerprint(current: &Activities, change: &Change) -> Fingerprint {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    current.hash(&mut h);
    change.hash(&mut h);
    Fingerprint(h.finish())
}

struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Refused configurations the planner may not choose again until the chip resets.
pub const REFUSED_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    set: [Option<Fingerprint>; REFUSED_CAPACITY],
}

impl Refused {
    pub const EMPTY: Self = Self { set: [None; REFUSED_CAPACITY] };

    /// False when full: the caller escalates rather than forget a refusal.
    pub fn insert(&mut self, f: Fingerprint) -> bool {
        if self.contains(f) {
            return true;
        }
        match self.set.iter_mut().find(|s| s.is_none()) {
            Some(slot) => {
                *slot = Some(f);
                true
            }
            None => false,
        }
    }

    #[must_use]
    pub fn contains(&self, f: Fingerprint) -> bool {
        self.set.contains(&Some(f))
    }

    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }
}
