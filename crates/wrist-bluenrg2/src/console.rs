//! The chip's values as the watch's console words them.

use bluenrg2::BootReason;

/// What a boot reason says of the chip's last start, in a few words.
#[must_use]
pub const fn boot_reason(reason: BootReason) -> &'static str {
    match reason {
        BootReason::STARTED => "the stack started",
        BootReason::UPDATER_BY_COMMAND => "the updater, by command",
        BootReason::UPDATER_BAD_FLAG => "the updater, the stack not marked valid",
        BootReason::UPDATER_BY_PIN => "the updater, by the IRQ pin",
        BootReason::WATCHDOG => "a watchdog reset",
        BootReason::LOCKUP => "a lockup reset",
        BootReason::BROWNOUT => "a brownout reset",
        BootReason::CRASH => "a crash reset",
        BootReason::ECC_ERROR => "an ECC error reset",
        _ => "a reason unknown",
    }
}
