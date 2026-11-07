//! The chip's life as the core sees it: started, reset, given up on.

use wrist_ble::{Capabilities, ChipInfo, Instant, Millis, ResetCause};

/// Resets in any hour, the core's and the chip's own alike, before the core declares a fault.
pub const RESETS_PER_HOUR: usize = 3;

/// A chip in fault is started once this often, or when the user asks.
pub const FAULT_RETRY: Millis = Millis::mins(60);

/// A start out of a fault that lasts this long has worked, and the budget is whole again.
pub const RETRY_PROVEN: Millis = Millis::mins(5);

const HOUR: Millis = Millis::mins(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Chip {
    /// Held in reset: the radio is off.
    #[default]
    Off,
    Starting {
        since: Instant,
    },
    Ready(ChipInfo),
    Fault {
        cause: FaultCause,
        retry_at: Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultCause {
    StartFailed,
    /// The chip refused even the safe baseline.
    BaselineRefused,
    /// The chip reset itself more often than the budget allows.
    KeptResetting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    RadioOn,
    RadioOff,
    Ready(ChipInfo),
    Reset(ResetCause),
    StartFailed,
    BaselineRefused,
    /// The user asked for the radio again.
    Retry,
    Tick,
}

/// One start, which its answer names: an answer to an earlier start is not this one's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StartId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Reset the chip and bring it up, then answer with this start's id.
    Start(StartId),
    /// Keep the chip in reset.
    Hold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChipMachine {
    pub state: Chip,
    resets: [Option<Instant>; RESETS_PER_HOUR],
    /// The latest start asked for.
    start: StartId,
}

impl ChipMachine {
    pub const OFF: Self = Self { state: Chip::Off, resets: [None; RESETS_PER_HOUR], start: StartId(0) };

    /// The start in flight, which an answer must name to count.
    #[must_use]
    pub fn starting(&self) -> Option<StartId> {
        matches!(self.state, Chip::Starting { .. }).then_some(self.start)
    }

    #[must_use]
    pub fn step(mut self, caps: &Capabilities, now: Instant, event: Event) -> (Self, Option<Action>) {
        let action = match (self.state, event) {
            (_, Event::RadioOff) => {
                self.state = Chip::Off;
                Some(Action::Hold)
            }
            (Chip::Off, Event::RadioOn) | (Chip::Fault { .. }, Event::Retry) => Some(self.begin(now)),
            (Chip::Fault { retry_at, .. }, Event::Tick) if now >= retry_at => {
                self.resets = [Some(now + RETRY_PROVEN - HOUR); RESETS_PER_HOUR];
                Some(self.begin(now))
            }
            (Chip::Starting { .. }, Event::Ready(info)) => {
                self.state = Chip::Ready(info);
                None
            }
            (Chip::Starting { .. }, Event::StartFailed) => Some(self.escalate(now, FaultCause::StartFailed)),
            (Chip::Starting { since }, Event::Tick) if now >= since + caps.start_within => Some(self.escalate(now, FaultCause::StartFailed)),
            (Chip::Ready(_), Event::BaselineRefused) => Some(self.escalate(now, FaultCause::BaselineRefused)),
            (Chip::Ready(_), Event::Reset(_)) => Some(self.escalate(now, FaultCause::KeptResetting)),
            _ => None,
        };
        (self, action)
    }

    #[must_use]
    pub fn deadline(&self, caps: &Capabilities) -> Option<Instant> {
        match self.state {
            Chip::Fault { retry_at, .. } => Some(retry_at),
            Chip::Starting { since } => Some(since + caps.start_within),
            _ => None,
        }
    }

    fn escalate(&mut self, now: Instant, cause: FaultCause) -> Action {
        let recent = |at: &Option<Instant>| at.is_some_and(|t| now.since(t) < HOUR);
        if let Some(slot) = self.resets.iter_mut().find(|at| !recent(at)) {
            *slot = Some(now);
            self.begin(now)
        } else {
            self.state = Chip::Fault { cause, retry_at: now + FAULT_RETRY };
            Action::Hold
        }
    }

    fn begin(&mut self, now: Instant) -> Action {
        self.state = Chip::Starting { since: now };
        self.start = StartId(self.start.0.wrapping_add(1));
        Action::Start(self.start)
    }
}
