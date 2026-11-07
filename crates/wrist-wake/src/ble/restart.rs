//! What the chip's `ACI_BLUE_INITIALIZED` means to the driver, and what it does about it.
//!
//! The BlueNRG-2 sends that event after every reset: the one the driver's start asked for through
//! the chip's reset pin, and any it was not asked for (a watchdog, lockup or brownout reset, a
//! crash, or its updater's command, `0xFC20` on firmware 2353). Unasked, the chip has lost its
//! links and its configuration, and in its updater it answers no normal command. Linux check 8
//! (install 252) left the driver `connected` to a chip in its updater, silent until the image
//! was reinstalled.

use bluenrg2::boot_reason;

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
    pub fn initialized(&mut self, booting: bool, reason: u8, now: u64, ticks_per_second: u64) -> Boot {
        if booting {
            return if boot_reason::updater(reason) { Boot::InUpdater } else { Boot::Up };
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
