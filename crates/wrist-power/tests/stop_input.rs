use wrist_power::{StopInput, UNANSWERED_MS, UNANSWERED_READS};

const MINUTE: u64 = 60_000;

#[test]
fn an_answer_passes_through_and_clears_the_count() {
    let mut s = StopInput::new();
    assert_eq!(s.sample(0, None, false, false), None);
    assert_eq!(s.sample(MINUTE, Some(true), false, false), Some(true));
    assert_eq!(s.misses(), 0);
    assert_eq!(s.sample(2 * MINUTE, Some(false), false, false), Some(false));
    assert!(!s.assumed_battery());
}

#[test]
fn unanswered_reads_count_as_battery_only_after_both_the_count_and_the_time() {
    let mut s = StopInput::new();
    for i in 0..u64::from(UNANSWERED_READS) {
        assert_eq!(s.sample(i * MINUTE, None, false, false), None);
    }
    // Five reads span four minutes: not yet.
    assert!(!s.assumed_battery());
    assert_eq!(s.sample(UNANSWERED_MS, None, false, false), Some(false));
    assert!(s.assumed_battery());
}

#[test]
fn quick_reads_do_not_shorten_the_wait() {
    let mut s = StopInput::new();
    for i in 0..600 {
        let now = i * 1000;
        let expected = (now >= UNANSWERED_MS).then_some(false);
        assert_eq!(s.sample(now, None, false, false), expected, "at {now} ms");
    }
}

#[test]
fn a_host_on_the_link_holds_the_input_unknown() {
    let mut s = StopInput::new();
    for i in 0..10 {
        assert_eq!(s.sample(i * MINUTE, None, false, true), None);
    }
    assert_eq!(s.sample(10 * MINUTE, None, false, false), Some(false));
    assert_eq!(s.sample(11 * MINUTE, None, false, true), None);
}

#[test]
fn an_answer_or_an_input_change_ends_the_assumption() {
    let mut s = StopInput::new();
    for i in 0..=5 {
        s.sample(i * MINUTE, None, false, false);
    }
    assert!(s.assumed_battery());
    assert_eq!(s.sample(6 * MINUTE, Some(true), false, false), Some(true));
    assert!(!s.assumed_battery());

    for i in 7..=12 {
        s.sample(i * MINUTE, None, false, false);
    }
    assert!(s.assumed_battery());
    assert_eq!(s.sample(13 * MINUTE, None, true, false), None);
    assert_eq!(s.misses(), 1);
    for i in 14..18 {
        assert_eq!(s.sample(i * MINUTE, None, false, false), None);
    }
    assert_eq!(s.sample(18 * MINUTE, None, false, false), Some(false));
}
