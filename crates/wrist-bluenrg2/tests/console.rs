//! The chip's values as the console words them.

use bluenrg2::BootReason;
use wrist_bluenrg2::console::boot_reason;

#[test]
fn every_boot_reason_is_described_apart() {
    assert_eq!(boot_reason(BootReason::UPDATER_BY_COMMAND), "the updater, by command");
    assert_eq!(boot_reason(BootReason::new(0x0A)), "a reason unknown");
    let known: Vec<_> = (0x01..=0x09).map(|r| boot_reason(BootReason::new(r))).collect();
    for (i, d) in known.iter().enumerate() {
        assert_ne!(*d, boot_reason(BootReason::new(0x0A)), "{:#04x}", i + 1);
        assert!(!known[..i].contains(d), "{d}");
    }
}
