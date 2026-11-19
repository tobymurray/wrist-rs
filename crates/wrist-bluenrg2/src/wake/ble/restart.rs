//! What the chip's `ACI_BLUE_INITIALIZED` means to the driver; how many restarts are too many is
//! the core's `RESETS_PER_HOUR`.

use bluenrg2::BootReason;

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
}

/// The event, with its `reason`. `booting`: a reset through the pin is waiting for this event; it is
/// the only boot that is not a restart.
#[must_use]
pub fn boot(booting: bool, reason: BootReason) -> Boot {
    if !booting {
        Boot::Restarted
    } else if reason.is_updater() {
        Boot::InUpdater
    } else {
        Boot::Up
    }
}
