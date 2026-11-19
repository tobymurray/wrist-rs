//! The chip's initialised event as the driver judges it (`wrist_bluenrg2::wake::ble::restart`).

use bluenrg2::BootReason;
use wrist_bluenrg2::wake::ble::restart::{boot, Boot};

#[test]
fn the_boot_a_reset_asked_for_is_up_unless_in_the_updater() {
    for reason in (0..=u8::MAX).map(BootReason::new) {
        let want = if reason.is_updater() { Boot::InUpdater } else { Boot::Up };
        assert_eq!(boot(true, reason), want, "reason {reason:?}");
    }
}

/// Linux check 8 (install 252): `04FF03 0100 02`, the updater by command, under a live link.
#[test]
fn any_boot_nobody_asked_for_is_a_restart() {
    for reason in [BootReason::UPDATER_BY_COMMAND, BootReason::STARTED, BootReason::WATCHDOG, BootReason::BROWNOUT, BootReason::new(0x42)] {
        assert_eq!(boot(false, reason), Boot::Restarted, "reason {reason:?}");
    }
}
