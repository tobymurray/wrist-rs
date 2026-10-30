use wrist_steps::{day_of, Ledger, WORDS};

const MON: Option<u16> = Some(20_390);
const TUE: Option<u16> = Some(20_391);
const THU: Option<u16> = Some(20_393);

#[test]
fn today_is_the_count_since_the_day_began() {
    let mut l = Ledger::new(MON, 500);
    assert_eq!(l.today(), 0);
    assert_eq!(l.observe(MON, 500), 0);
    assert_eq!(l.observe(MON, 620), 120);
    assert_eq!(l.observe(MON, 1_000), 500);
    assert_eq!(l.yesterday(), None);
}

#[test]
fn midnight_starts_a_new_day_and_keeps_yesterday() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 4_000);
    // The first reading of the new day: the last minute's steps went to the day that ended.
    assert_eq!(l.observe(TUE, 4_010), 0);
    assert_eq!(l.yesterday(), Some(4_010));
    assert_eq!(l.observe(TUE, 4_100), 90);
}

#[test]
fn a_day_skipped_has_no_yesterday() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 300);
    assert_eq!(l.observe(THU, 310), 0);
    assert_eq!(l.yesterday(), None);
}

#[test]
fn the_clock_set_back_a_day_starts_the_day_again() {
    let mut l = Ledger::new(TUE, 0);
    l.observe(TUE, 50);
    assert_eq!(l.observe(MON, 60), 0);
    assert_eq!(l.yesterday(), None);
}

#[test]
fn a_chip_starting_again_banks_what_it_had_counted() {
    let mut l = Ledger::new(MON, 1_000);
    l.observe(MON, 1_400);
    assert_eq!(l.observe(MON, 30), 430);
    assert_eq!(l.observe(MON, 100), 500);
    // And again.
    assert_eq!(l.observe(MON, 5), 505);
}

#[test]
fn a_chip_starting_again_at_midnight_banks_into_the_day_that_ended() {
    let mut l = Ledger::new(MON, 1_000);
    l.observe(MON, 1_400);
    assert_eq!(l.observe(TUE, 20), 0);
    assert_eq!(l.yesterday(), Some(420));
    assert_eq!(l.observe(TUE, 70), 50);
}

#[test]
fn steps_before_the_clock_is_set_go_to_the_first_set_day() {
    let mut l = Ledger::new(None, 0);
    assert_eq!(l.observe(None, 80), 80);
    assert_eq!(l.observe(MON, 90), 90);
    assert_eq!(l.yesterday(), None);
    assert_eq!(l.observe(TUE, 95), 0);
    assert_eq!(l.yesterday(), Some(95));
}

#[test]
fn the_clock_lost_keeps_the_day() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 40);
    assert_eq!(l.observe(None, 60), 60);
    assert_eq!(l.observe(MON, 70), 70);
}

#[test]
fn saved_words_restore_the_same_ledger_when_the_count_carried_on() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 3_000);
    l.observe(TUE, 3_200);
    l.observe(TUE, 3_500);
    let words = l.save();
    let mut back = Ledger::restore(&words, true).unwrap();
    assert_eq!(back, l);
    assert_eq!(back.observe(TUE, 3_600), 400);
    assert_eq!(back.yesterday(), Some(3_200));
}

#[test]
fn a_reloaded_chip_keeps_the_day_so_far() {
    let mut l = Ledger::new(MON, 100);
    l.observe(MON, 700);
    let mut back = Ledger::restore(&l.save(), false).unwrap();
    assert_eq!(back.today(), 600);
    assert_eq!(back.observe(MON, 0), 600);
    assert_eq!(back.observe(MON, 25), 625);
}

#[test]
fn a_restored_ledger_still_sees_the_chip_start_again() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 900);
    // The words say the count carried on, but it reads lower: the chip started again after all.
    let mut back = Ledger::restore(&l.save(), true).unwrap();
    assert_eq!(back.observe(MON, 10), 910);
}

#[test]
fn unset_days_and_totals_survive_saving() {
    let l = Ledger::new(None, 7);
    assert_eq!(Ledger::restore(&l.save(), true), Some(l));
}

#[test]
fn anything_but_saved_words_is_refused() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, 1_234);
    let words = l.save();
    assert_eq!(Ledger::restore(&[0; WORDS], true), None);
    assert_eq!(Ledger::restore(&[u32::MAX; WORDS], true), None);
    for i in 0..WORDS {
        for bit in [0, 7, 16, 31] {
            let mut bad = words;
            bad[i] ^= 1 << bit;
            assert_eq!(Ledger::restore(&bad, true), None, "word {i} bit {bit}");
        }
    }
}

#[test]
fn today_saturates_rather_than_wrapping() {
    let mut l = Ledger::new(MON, 0);
    l.observe(MON, u32::MAX);
    assert_eq!(l.observe(MON, 10), u32::MAX);
}

#[test]
fn days_are_local_day_numbers() {
    assert_eq!(day_of(0), Some(0));
    assert_eq!(day_of(86_399), Some(0));
    assert_eq!(day_of(86_400), Some(1));
    // 2025-10-29 00:00 and 23:59:59.
    assert_eq!(day_of(1_761_696_000), Some(20_390));
    assert_eq!(day_of(1_761_782_399), Some(20_390));
    assert_eq!(day_of(-1), None);
    assert_eq!(day_of(i64::from(u16::MAX) * 86_400), None);
    assert_eq!(day_of(i64::MAX), None);
}
