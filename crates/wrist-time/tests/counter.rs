use wrist_time::counter::extend;
#[test]
fn rollover_remains_monotonic_before_and_after_the_interrupt() {
    let before = extend((7, 7), (false, false), (65535, 65535)).unwrap();
    let pending = extend((7, 7), (true, true), (0, 0)).unwrap();
    let serviced = extend((8, 8), (false, false), (0, 0)).unwrap();
    assert_eq!(pending, before + 1);
    assert_eq!(pending, serviced);
}
#[test]
fn racing_snapshots_require_retry() {
    assert_eq!(extend((7, 8), (true, false), (1, 1)), None);
    assert_eq!(extend((7, 7), (false, true), (0, 0)), None);
    assert_eq!(extend((7, 7), (false, false), (65535, 0)), None);
}
