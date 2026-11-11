use wrist_power::{
    end_of_charge, fstat_flags, full_detected, full_update_slope, hibernating, learn_stage, mixing_rate, Unit,
};

#[test]
fn units_in_hundredths() {
    assert_eq!(Unit::Percent.hundredths(0x5F05), 9501);
    assert_eq!(Unit::Percent.hundredths(0x6400), 10_000);
    assert_eq!(Unit::Capacity.hundredths(0x0190), 20_000);
    assert_eq!(Unit::Capacity.hundredths(0xFFFF), 3_276_750);
    assert_eq!(Unit::Voltage.hundredths(0xD200), 420_000);
    assert_eq!(Unit::Voltage.hundredths(0xFFFF), 511_992);
    assert_eq!(Unit::Current.hundredths(0x0059), 1390);
    assert_eq!(Unit::Current.hundredths(0xFFC4), -937);
    assert_eq!(Unit::Current.hundredths(0x8000), -512_000);
    assert_eq!(Unit::Cycles.hundredths(0x0064), 100);
}

#[test]
fn status2_full_det_beside_hib() {
    assert!(full_detected(0x0020));
    assert!(!full_detected(0x3002));
    assert!(hibernating(0x3022) && full_detected(0x3022));
}

#[test]
fn learning_and_mixing_fields() {
    assert_eq!(learn_stage(0x4486), 0);
    assert_eq!(learn_stage(0x44F6), 7);
    assert_eq!(full_update_slope(0x3870), 3);
    assert_eq!(full_update_slope(0xF870), 15);
    assert_eq!(mixing_rate(0x3870), 3);
    assert_eq!(mixing_rate(0x0BE0), 31);
}

#[test]
fn fstat_names_each_set_flag() {
    assert!(fstat_flags(0).next().is_none());
    assert_eq!(fstat_flags(0x0281).collect::<Vec<_>>(), ["DNR", "RelDt", "FQ"]);
    assert_eq!(fstat_flags(0xFF3E).collect::<Vec<_>>(), ["RelDt", "EDet"]);
    assert_eq!(fstat_flags(0x0040).collect::<Vec<_>>(), ["RelDt2"]);
}

#[test]
fn end_of_charge_band_is_open_at_both_ends() {
    // 13.9 mA: the band is 11.125 to 111.25 raw steps.
    let term = 0x0059;
    assert_eq!(end_of_charge(0x6000, 0x5F05, term, 38, 38), [true, true, true]);
    assert_eq!(end_of_charge(0x5F05, 0x5F05, term, 11, 12), [false, false, true]);
    assert_eq!(end_of_charge(0x6400, 0x5F05, term, 111, 112), [true, true, false]);
    assert_eq!(end_of_charge(0x6400, 0x5F05, term, (-19i16).cast_unsigned(), 0), [true, false, false]);
    assert_eq!(end_of_charge(0x6400, 0x5F05, 0x0050, 10, 100), [true, false, false]);
}
