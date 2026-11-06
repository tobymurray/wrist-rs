//! The chip's initialised event as the driver judges it (`wrist_wake::ble::restart`).

use bluenrg2::boot_reason;
use wrist_wake::ble::restart::{Boot, Restarts, RESTARTS_PER_HOUR};

const HZ: u64 = 32_768;

fn secs(s: u64) -> u64 {
    s * HZ
}

#[test]
fn the_boot_a_reset_asked_for_is_up_unless_in_the_updater() {
    let mut r = Restarts::default();
    for reason in 0..=u8::MAX {
        let want = if boot_reason::updater(reason) { Boot::InUpdater } else { Boot::Up };
        assert_eq!(r.initialized(true, reason, secs(1), HZ), want, "reason {reason}");
    }
    assert_eq!(r, Restarts::default(), "a start's own boot spends no budget");
}

/// Linux check 8 (install 252): `04FF03 0100 02`, the updater by command, under a live link.
#[test]
fn any_boot_nobody_asked_for_is_a_restart() {
    for reason in [boot_reason::UPDATER_BY_COMMAND, boot_reason::STARTED, boot_reason::WATCHDOG, boot_reason::BROWNOUT, 0x42] {
        assert_eq!(Restarts::default().initialized(false, reason, secs(10), HZ), Boot::Restarted, "reason {reason}");
    }
}

#[test]
fn a_chip_that_keeps_restarting_is_held_then_given_its_budget_back_by_the_hour() {
    let mut r = Restarts::default();
    for i in 0..RESTARTS_PER_HOUR as u64 {
        assert_eq!(r.initialized(false, boot_reason::WATCHDOG, secs(60 * i), HZ), Boot::Restarted);
    }
    assert_eq!(r.initialized(false, boot_reason::WATCHDOG, secs(3_599), HZ), Boot::KeptRestarting);
    assert_eq!(r.initialized(true, boot_reason::STARTED, secs(3_599), HZ), Boot::Up, "a start's boot is never held");
    assert_eq!(r.initialized(false, boot_reason::WATCHDOG, secs(3_600), HZ), Boot::Restarted);
    assert_eq!(r.initialized(false, boot_reason::WATCHDOG, secs(3_601), HZ), Boot::KeptRestarting);
}
