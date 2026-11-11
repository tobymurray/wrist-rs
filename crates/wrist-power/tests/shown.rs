use wrist_power::{charge_done, ShownCharge};

const BATTERY: Option<bool> = Some(false);
const EXTERNAL: Option<bool> = Some(true);

#[test]
fn on_battery_it_only_falls() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(Some(94), BATTERY, None), Some(94));
    // The night's climb toward the voltage model, and back.
    assert_eq!(s.reading(Some(95), BATTERY, None), Some(94));
    assert_eq!(s.reading(Some(94), BATTERY, None), Some(94));
    assert_eq!(s.reading(Some(93), BATTERY, None), Some(93));
    assert_eq!(s.reading(Some(95), BATTERY, Some(false)), Some(93));
}

#[test]
fn an_unknown_input_counts_as_battery() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(Some(80), None, None), Some(80));
    assert_eq!(s.reading(Some(85), None, Some(true)), Some(80));
}

#[test]
fn charging_it_follows_the_gauge_below_100() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(Some(66), BATTERY, None), Some(66));
    assert_eq!(s.reading(Some(70), EXTERNAL, Some(false)), Some(70));
    // The gauge's own 100 % before the charger stops.
    assert_eq!(s.reading(Some(100), EXTERNAL, Some(false)), Some(99));
    assert_eq!(s.reading(Some(100), EXTERNAL, None), Some(99));
    assert_eq!(s.reading(Some(100), EXTERNAL, Some(true)), Some(100));
}

#[test]
fn done_reads_100_and_unplugging_keeps_what_the_gauge_says() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(Some(99), EXTERNAL, Some(true)), Some(100));
    assert_eq!(s.reading(Some(100), BATTERY, None), Some(100));
    assert_eq!(s.reading(Some(99), BATTERY, None), Some(99));
}

#[test]
fn a_new_charge_after_done_starts_below_100() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(Some(100), EXTERNAL, Some(true)), Some(100));
    // Recharging after the cell fell below the restart threshold.
    assert_eq!(s.reading(Some(97), EXTERNAL, Some(false)), Some(97));
}

#[test]
fn an_unanswered_gauge_keeps_the_last() {
    let mut s = ShownCharge::new();
    assert_eq!(s.reading(None, BATTERY, None), None);
    assert_eq!(s.reading(Some(50), BATTERY, None), Some(50));
    assert_eq!(s.reading(None, EXTERNAL, Some(true)), Some(50));
}

#[test]
fn done_is_bat_chg_status_110() {
    assert!(charge_done(0b110));
    assert!(charge_done(0b0111_0110));
    assert!(!charge_done(0b101));
    assert!(!charge_done(0b011));
    assert!(!charge_done(0b111));
    assert!(!charge_done(0));
}
