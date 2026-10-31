//! The chip's life as the core sees it: started, reset, given up on.

use wrist_ble::{ChipInfo, Instant, Millis, ResetCause};

/// Resets in any hour, the core's and the chip's own alike, before the core declares a fault.
pub const RESETS_PER_HOUR: usize = 3;

/// A chip in fault is tried again this often, or when the user asks.
pub const FAULT_RETRY: Millis = Millis::mins(60);

/// A chip not ready this long after its reset has failed to start (HARDWARE.md: ready within 3 s).
pub const START_WITHIN: Millis = Millis::secs(5);

const HOUR: Millis = Millis::mins(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Reset the chip and bring it up.
    Start,
    /// Keep the chip in reset.
    Hold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChipMachine {
    pub state: Chip,
    resets: [Option<Instant>; RESETS_PER_HOUR],
}

impl ChipMachine {
    pub const OFF: Self = Self { state: Chip::Off, resets: [None; RESETS_PER_HOUR] };

    #[must_use]
    pub fn step(mut self, now: Instant, event: Event) -> (Self, Option<Action>) {
        let action = match (self.state, event) {
            (_, Event::RadioOff) => {
                self.state = Chip::Off;
                Some(Action::Hold)
            }
            (Chip::Off, Event::RadioOn) | (Chip::Fault { .. }, Event::Retry) => {
                self.state = Chip::Starting { since: now };
                Some(Action::Start)
            }
            (Chip::Fault { retry_at, .. }, Event::Tick) if now >= retry_at => {
                self.state = Chip::Starting { since: now };
                Some(Action::Start)
            }
            (Chip::Starting { .. }, Event::Ready(info)) => {
                self.state = Chip::Ready(info);
                None
            }
            (Chip::Starting { .. }, Event::StartFailed) => Some(self.escalate(now, FaultCause::StartFailed)),
            (Chip::Starting { since }, Event::Tick) if now >= since + START_WITHIN => Some(self.escalate(now, FaultCause::StartFailed)),
            (Chip::Ready(_), Event::BaselineRefused) => Some(self.escalate(now, FaultCause::BaselineRefused)),
            (Chip::Ready(_) | Chip::Starting { .. }, Event::Reset(_)) => Some(self.escalate(now, FaultCause::KeptResetting)),
            _ => None,
        };
        (self, action)
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        match self.state {
            Chip::Fault { retry_at, .. } => Some(retry_at),
            Chip::Starting { since } => Some(since + START_WITHIN),
            _ => None,
        }
    }

    fn escalate(&mut self, now: Instant, cause: FaultCause) -> Action {
        let recent = |at: &Option<Instant>| at.is_some_and(|t| now.since(t) < HOUR);
        if let Some(slot) = self.resets.iter_mut().find(|at| !recent(at)) {
            *slot = Some(now);
            self.state = Chip::Starting { since: now };
            Action::Start
        } else {
            self.state = Chip::Fault { cause, retry_at: now + FAULT_RETRY };
            Action::Hold
        }
    }
}
