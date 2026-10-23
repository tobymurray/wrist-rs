use wrist_power::{hibernating, tte_seconds, BatteryRun};

#[test]
fn a_run_averages_the_readings_it_had() {
    let mut run = BatteryRun::new(1_000);
    assert_eq!(run.mean_ua(), None);
    run.reading(61_000, Some(-150), Some(921), Some(3_600_000), Some(true));
    run.reading(121_000, None, None, None, None);
    run.reading(181_000, Some(-90), Some(919), Some(3_500_000), Some(false));
    assert_eq!(run.mean_ua(), Some(-120));
    assert_eq!(run.samples(), 2);
    assert_eq!(run.seconds(), 180);
    assert_eq!((run.first_soc_x10, run.last_soc_x10), (Some(921), Some(919)));
    assert_eq!(run.last_tte_s, Some(3_500_000));
    assert_eq!((run.hibernating, run.hib_known), (1, 2));
}

#[test]
fn a_reading_out_of_order_does_not_shorten_the_run() {
    let mut run = BatteryRun::new(0);
    run.reading(10_000, Some(-1), None, None, None);
    run.reading(5_000, Some(-1), None, None, None);
    assert_eq!(run.seconds(), 10);
}

#[test]
fn time_to_empty_is_5_625_s_a_count_and_unknown_at_ffff() {
    assert_eq!(tte_seconds(0), Some(0));
    assert_eq!(tte_seconds(8), Some(45));
    assert_eq!(tte_seconds(0xFFFE), Some(368_628));
    assert_eq!(tte_seconds(0xFFFF), None);
}

#[test]
fn hib_is_status2_bit_1() {
    assert!(hibernating(0b10));
    assert!(!hibernating(0b01));
    assert!(hibernating(0xFFFF));
}
