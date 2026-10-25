use wrist_power::{i2c_mode, Rails};

#[test]
fn the_reset_values_decode_to_the_datasheets_defaults() {
    // Mode 0's reset values, datasheet Table 8.
    let r = Rails::decode([0b0001_0100, 0b0000_1100, 0b0100_1111, 0b0011_1001]);
    assert_eq!((r.sw1_mv, r.sw2_mv, r.ldo1_mv, r.ldo2_mv), (1000, 1800, 1800, 3300));
    assert_eq!(r.enabled, [true; 4]);
    assert!(!r.ship && !r.by_pin);
    assert_eq!(r.watchdog_s, None);
}

#[test]
fn this_watchs_modes() {
    // HARDWARE.md, the PCA9420's registers.
    let r = Rails::decode([0x1C, 0x0C, 0x4F, 0x34]);
    assert_eq!((r.sw1_mv, r.sw2_mv, r.ldo1_mv, r.ldo2_mv), (1200, 1800, 1800, 3200));
    assert_eq!(r.enabled, [true; 4]);
    assert!(!r.ship && !r.by_pin);
    assert_eq!(r.watchdog_s, None);
    assert_eq!(Rails::decode([0x1C, 0x4C, 0x4F, 0x34]), r);
    let r = Rails::decode([0x5C, 0x4C, 0x42, 0x34]);
    assert_eq!(r.enabled, [false, false, true, false]);
    assert!(r.by_pin && !r.ship);
    assert_eq!(r.ldo1_mv, 1800);
}

#[test]
fn the_ends_of_each_table() {
    let r = Rails::decode([0x3F, 0x3F, 0xF0, 0xC0]);
    assert_eq!((r.sw1_mv, r.sw2_mv, r.ldo1_mv, r.ldo2_mv), (1800, 3300, 1900, 1500));
    assert_eq!(r.enabled, [false; 4]);
    assert_eq!(r.watchdog_s, Some(64));
    assert_eq!(Rails::decode([0x29, 0, 0, 0x40]).sw1_mv, 1500);
    assert_eq!(Rails::decode([0x28, 0, 0, 0x40]).watchdog_s, Some(16));
    assert_eq!(Rails::decode([0xC0, 0, 0, 0x80]), Rails {
        ship: true,
        by_pin: true,
        sw1_mv: 500,
        sw2_mv: 1500,
        ldo1_mv: 1700,
        ldo2_mv: 1500,
        enabled: [false; 4],
        watchdog_s: Some(32),
    });
}

#[test]
fn the_mode_bits() {
    assert_eq!(i2c_mode(0b0001_1000), 3);
    assert_eq!(i2c_mode(0b0000_1001), 1);
    assert_eq!(i2c_mode(0b1110_0111), 0);
}
