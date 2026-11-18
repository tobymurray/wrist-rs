//! What the chip's `ACI_BLUE_INITIALIZED` means to the driver, and what it does about it.

use bluenrg2::BootReason;

/// Restarts the chip may make by itself in any hour before the driver holds it in reset, as the
/// core's `RESETS_PER_HOUR`.
pub const RESTARTS_PER_HOUR: usize = 3;

/// One `ACI_BLUE_INITIALIZED`, judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    /// The boot a reset asked for: the stack runs; configure it.
    Up,
    /// The boot a reset asked for came up in the updater: the start fails rather than talk to
    /// it, and `ble on` tries again.
    InUpdater,
    /// Unasked: the link, its encryption and subscription, a transfer and the console's queue are
    /// gone. Reset the chip through its pin, which also leaves the updater, and bring it up again.
    Restarted,
    /// Unasked, more than [`RESTARTS_PER_HOUR`] times in an hour: hold the chip in reset.
    KeptRestarting,
}

/// The chip's restarts, for the budget and the `ble` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restarts {
    at: [Option<u64>; RESTARTS_PER_HOUR],
}

impl Restarts {
    /// The event, with its `reason`, at `now` on a clock of `ticks_per_second`. `booting`: a
    /// reset through the pin is waiting for this event; it is the only boot that is not a restart.
    pub fn initialized(&mut self, booting: bool, reason: BootReason, now: u64, ticks_per_second: u64) -> Boot {
        if booting {
            return if reason.is_updater() { Boot::InUpdater } else { Boot::Up };
        }
        let hour = 3_600 * ticks_per_second;
        match self.at.iter_mut().find(|at| at.is_none_or(|t| now.saturating_sub(t) >= hour)) {
            Some(slot) => {
                *slot = Some(now);
                Boot::Restarted
            }
            None => Boot::KeptRestarting,
        }
    }
}
