//! Where the core and the driver acted differently, judged as their actions come: each waits
//! [`NEAR`] for the other side's, then is a disagreement if none came.

use core::fmt;

use wrist_ble::{Op, OpKind};

use crate::DriverOp;

/// How far apart, in milliseconds, the two sides may act and still have done the same thing.
pub const NEAR: u64 = 300;

/// How long after the operator's `ble disconnect` the driver's own disconnect may come.
pub const OPERATOR: u64 = 1_000;

/// Actions within [`NEAR`] of each other that each side can hold; past that, the oldest is
/// judged early.
const WINDOW: usize = 16;

/// What one side did that the other must have done too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// At whatever interval: the core takes its own from its menu.
    Advertise,
    /// Interval min and max in 1.25 ms, and latency.
    Ask(u16, u16, u16),
    /// The answer to a numeric comparison.
    Confirm(bool),
    RequestSecurity,
    AllowRebond,
    Disconnect,
}

impl Act {
    /// The core's operation as the comparison reads it, for those it reads.
    #[must_use]
    pub fn of_core(op: &Op) -> Option<Self> {
        Some(match op {
            Op::Advertise(Some(_)) => Self::Advertise,
            Op::UpdateParams(_, p) => Self::Ask(p.interval_min.as_u16(), p.interval_max.as_u16(), p.latency),
            Op::ConfirmNumeric(_, yes) => Self::Confirm(*yes),
            Op::RequestSecurity(_) => Self::RequestSecurity,
            Op::AllowRebond(_) => Self::AllowRebond,
            Op::Disconnect(..) => Self::Disconnect,
            _ => return None,
        })
    }

    /// The driver's command as the comparison reads it, for those it reads.
    #[must_use]
    pub fn of_driver(d: DriverOp) -> Option<Self> {
        Some(match d {
            DriverOp::Advertise(..) => Self::Advertise,
            DriverOp::AskParams(min, max, latency) => Self::Ask(min, max, latency),
            DriverOp::Confirm(yes) => Self::Confirm(yes),
            DriverOp::Other(OpKind::RequestSecurity) => Self::RequestSecurity,
            DriverOp::Other(OpKind::AllowRebond) => Self::AllowRebond,
            DriverOp::Other(OpKind::Disconnect) => Self::Disconnect,
            DriverOp::StopAdvertising | DriverOp::Other(_) => return None,
        })
    }
}

impl fmt::Display for Act {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Advertise => f.write_str("advertised"),
            Self::Ask(min, max, latency) => write!(f, "asked for {min}-{max} units, latency {latency}"),
            Self::Confirm(true) => f.write_str("accepted the comparison"),
            Self::Confirm(false) => f.write_str("refused the comparison"),
            Self::RequestSecurity => f.write_str("asked the central to pair"),
            Self::AllowRebond => f.write_str("allowed a rebond"),
            Self::Disconnect => f.write_str("disconnected"),
        }
    }
}

/// One side acted and the other did nothing like it within [`NEAR`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disagreement {
    pub at: u64,
    /// The core acted alone, else the driver did.
    pub core: bool,
    pub act: Act,
}

impl fmt::Display for Disagreement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (one, other) = if self.core { ("core", "driver") } else { ("driver", "core") };
        write!(f, "{}: the {one} {} and the {other} did not", self.at, self.act)
    }
}

#[derive(Clone, Copy)]
struct Seen {
    at: u64,
    act: Act,
    /// Still owed the other side's.
    owed: bool,
}

/// One side's actions of the last [`NEAR`], oldest first.
struct Window {
    seen: [Seen; WINDOW],
    len: usize,
    core: bool,
}

impl Window {
    const fn new(core: bool) -> Self {
        Self { seen: [Seen { at: 0, act: Act::Advertise, owed: false }; WINDOW], len: 0, core }
    }

    /// Lets go of what is older than [`NEAR`] before `now`.
    fn expire(&mut self, now: u64, found: &mut impl FnMut(Disagreement)) {
        let gone = self.seen[..self.len].iter().take_while(|s| s.at.saturating_add(NEAR) < now).count();
        self.drop_oldest(gone, found);
    }

    /// Each dropped action still owed the other side's is a disagreement.
    fn drop_oldest(&mut self, n: usize, found: &mut impl FnMut(Disagreement)) {
        for s in self.seen[..n].iter().filter(|s| s.owed) {
            found(Disagreement { at: s.at, core: self.core, act: s.act });
        }
        self.seen.copy_within(n..self.len, 0);
        self.len -= n;
    }

    fn push(&mut self, s: Seen, found: &mut impl FnMut(Disagreement)) {
        if self.len == WINDOW {
            self.drop_oldest(1, found);
        }
        self.seen[self.len] = s;
        self.len += 1;
    }

    /// Whether this side did `act` within [`NEAR`], each such action now answered.
    fn answer(&mut self, act: Act) -> bool {
        let mut any = false;
        for s in self.seen[..self.len].iter_mut().filter(|s| s.act == act) {
            s.owed = false;
            any = true;
        }
        any
    }
}

/// The two sides set beside each other, told each action as it comes, in time order.
pub struct Verdict {
    core: Window,
    driver: Window,
    /// The operator's last `ble disconnect`.
    operator: Option<u64>,
}

impl Default for Verdict {
    fn default() -> Self {
        Self::new()
    }
}

impl Verdict {
    #[must_use]
    pub const fn new() -> Self {
        Self { core: Window::new(true), driver: Window::new(false), operator: None }
    }

    /// When the first action still owed the other side's can be judged.
    pub(crate) fn next(&self) -> Option<u64> {
        [&self.core, &self.driver].into_iter().filter_map(|w| w.seen[..w.len].iter().find(|s| s.owed)).map(|s| s.at.saturating_add(NEAR + 1)).min()
    }

    /// Judges every action older than [`NEAR`] before `now`.
    pub fn advance(&mut self, now: u64, found: &mut impl FnMut(Disagreement)) {
        self.core.expire(now, found);
        self.driver.expire(now, found);
    }

    /// Judges every action held, as at the end of a record.
    pub fn finish(&mut self, found: &mut impl FnMut(Disagreement)) {
        self.core.drop_oldest(self.core.len, found);
        self.driver.drop_oldest(self.driver.len, found);
    }

    pub fn core(&mut self, at: u64, act: Act, found: &mut impl FnMut(Disagreement)) {
        self.advance(at, found);
        let owed = !self.driver.answer(act);
        self.core.push(Seen { at, act, owed }, found);
    }

    /// The driver asks a central for parameters at every connection, so its ask is owed
    /// nothing; nor is a disconnect within [`OPERATOR`] after the operator's.
    pub fn driver(&mut self, at: u64, act: Act, found: &mut impl FnMut(Disagreement)) {
        self.advance(at, found);
        let answered = self.core.answer(act);
        let operator = act == Act::Disconnect && self.operator.is_some_and(|t| t <= at && at - t <= OPERATOR);
        let owed = !answered && !matches!(act, Act::Ask(..)) && !operator;
        self.driver.push(Seen { at, act, owed }, found);
    }

    /// The operator's `ble disconnect`, which the core has no goal for; the driver's own can
    /// come in the same millisecond, ahead of it.
    pub fn operator(&mut self, at: u64, found: &mut impl FnMut(Disagreement)) {
        self.advance(at, found);
        self.operator = Some(at);
        for s in self.driver.seen[..self.driver.len].iter_mut().filter(|s| s.act == Act::Disconnect && s.at == at) {
            s.owed = false;
        }
    }
}
