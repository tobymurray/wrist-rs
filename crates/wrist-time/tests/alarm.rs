use wrist_time::alarm::{minute_of_day, parse_hhmm, Alarm, Found, DAY, GRACE, LATE, REPEAT, WINDOW};
use wrist_time::zone::Zone;
use wrist_time::{civil_of_seconds, parse_civil, seconds_of, Civil};

fn at(text: &str) -> i64 {
    seconds_of(parse_civil(text).unwrap())
}

fn utc(text: &str) -> u32 {
    u32::try_from(at(text)).unwrap()
}

fn hm(h: u8, m: u8) -> Alarm {
    Alarm::new(h, m).unwrap()
}

/// The UK from summer 2026: back to GMT on 25 October, forward on 28 March.
fn london() -> Zone {
    Zone::parse(&format!("60 {} 0 {} 60", utc("2026-10-25 01:00:00"), utc("2027-03-28 01:00:00"))).unwrap()
}

/// A ring: the calendar's local time and the offset in force when it rang.
#[derive(Debug, PartialEq, Eq)]
struct Rang {
    local: Civil,
    offset: i32,
}

/// The forward minutes a change of `shift` moved the calendar, for [`Alarm::check`].
fn jumped(shift: i32) -> u16 {
    u16::try_from(shift.max(0)).unwrap()
}

/// Checks every `step` seconds for `seconds` from the local time `from`, as extra checks (the
/// console's, a boot's) may come: the zone's change first, then the alarm, whose record is
/// kept in a register across each check as the image keeps it.
fn run(alarm: Alarm, zone: Option<Zone>, from: i64, seconds: i64, step: i64) -> (Vec<Rang>, Alarm) {
    let (mut word, mut zone, mut clock) = (alarm.encode(), zone, from);
    let mut rings = Vec::new();
    let end = from + seconds;
    while clock < end {
        let mut jump = 0;
        if let Some((shift, after)) = zone.and_then(|z| z.due(clock)) {
            clock += i64::from(shift) * 60;
            zone = Some(after);
            jump = jumped(shift);
        }
        let (found, after) = Alarm::decode(word).unwrap().check(minute_of_day(civil_of_seconds(clock)), jump);
        word = after.encode();
        if found == Found::Ring {
            rings.push(Rang { local: civil_of_seconds(clock), offset: zone.map_or(0, |z| z.offset) });
        }
        clock += step;
    }
    (rings, Alarm::decode(word).unwrap())
}

/// The image's own checks only, for `seconds` from the local time `from`: one at boot, one
/// when alarm A matches the minute [`Alarm::next_check`] named, and one after each change of
/// zone, which the display task applies within `lag` seconds of its instant. The checks woken.
fn run_woken(alarm: Alarm, zone: Option<Zone>, from: i64, seconds: i64, lag: i64) -> (Vec<Rang>, Alarm, u32) {
    let (mut a, mut zone, mut clock) = (alarm, zone, from);
    let (mut rings, mut wakes) = (Vec::new(), 0);
    let end = from + seconds;
    let mut jump = 0;
    loop {
        let (found, after) = a.check(minute_of_day(civil_of_seconds(clock)), jump);
        a = Alarm::decode(after.encode()).unwrap();
        if found == Found::Ring {
            rings.push(Rang { local: civil_of_seconds(clock), offset: zone.map_or(0, |z| z.offset) });
        }
        let now = civil_of_seconds(clock);
        // Armed for the start of the minute named; `None`, nothing to wake for.
        let wake = a.next_check(minute_of_day(now)).map(|m| {
            assert!(m > 0 || found != Found::Nothing, "a check that changed nothing named its own minute");
            clock - i64::from(now.second) + i64::from(m) * 60
        });
        // Alarm A, unless the zone's change comes first and moves the calendar.
        let change = zone.and_then(|z| z.next[0].map(|c| i64::from(c.at) + i64::from(z.offset) * 60 + lag));
        jump = 0;
        match (wake, change) {
            (_, Some(t)) if wake.is_none_or(|w| t <= w) => {
                // One already due applies at once.
                clock = t.max(clock);
                let (shift, after) = zone.unwrap().due(clock).unwrap();
                clock += i64::from(shift) * 60;
                zone = Some(after);
                jump = jumped(shift);
            }
            (Some(w), _) if w == clock => {}
            (Some(w), _) => {
                clock = w;
                wakes += 1;
            }
            (None, None) => break,
            (None, Some(_)) => unreachable!(),
        }
        if clock >= end {
            break;
        }
    }
    (rings, a, wakes)
}

fn civil(text: &str) -> Civil {
    parse_civil(text).unwrap()
}

#[test]
fn records_round_trip_and_read_as_documented() {
    for at in 0..DAY {
        for on in [false, true] {
            for spent in [false, true] {
                for missed in [false, true] {
                    let a = Alarm { at, on, spent, missed };
                    assert_eq!(Alarm::decode(a.encode()), Some(a));
                }
            }
        }
    }
    assert_eq!(hm(7, 0).encode(), 0x414C_09A4);
    assert_eq!(Alarm { spent: true, ..hm(23, 59) }.encode(), 0x414C_1D9F);
    assert_eq!(Alarm { on: false, ..hm(0, 0) }.encode(), 0x414C_0000);
    assert_eq!(Alarm { spent: true, missed: true, ..hm(7, 0) }.encode(), 0x414C_39A4);
    assert_eq!((hm(7, 5).hour(), hm(7, 5).minute()), (7, 5));
}

#[test]
fn anything_else_in_the_register_is_no_alarm() {
    for w in [0, 0xFFFF_FFFF, 0x545A_09A4, 0x414D_09A4, 0x414C_05A0, 0x414C_07FF, 0x414C_49A4, 0x414C_89A4] {
        assert_eq!(Alarm::decode(w), None, "{w:#010x}");
    }
    assert_eq!(Alarm::new(24, 0), None);
    assert_eq!(Alarm::new(0, 60), None);
}

#[test]
fn times_parse_exactly() {
    assert_eq!(parse_hhmm("07:00"), Some((7, 0)));
    assert_eq!(parse_hhmm("7:00"), Some((7, 0)));
    assert_eq!(parse_hhmm(" 23:59\r\n"), Some((23, 59)));
    assert_eq!(parse_hhmm("0:00"), Some((0, 0)));
    for text in ["", "24:00", "7:60", "0700", "7:0", "07:000", "07:00:00", "+7:00", "7:+0", "a7:00", "07 00", "007:00"] {
        assert_eq!(parse_hhmm(text), None, "{text:?}");
    }
}

#[test]
fn an_alarm_rings_once_in_its_minute_however_often_it_is_checked() {
    let (rang, after) = run(hm(7, 0), None, at("2026-11-02 06:58:00"), 5 * 60, 1);
    assert_eq!(rang, [Rang { local: civil("2026-11-02 07:00:00"), offset: 0 }]);
    assert!(after.spent);
}

#[test]
fn a_reset_in_the_minute_it_rang_does_not_ring_it_again() {
    let (rang, kept) = run(hm(7, 0), None, at("2026-11-02 06:59:00"), 61, 60);
    assert_eq!(rang.len(), 1);
    // The new image reads the register in the same minute and checks again.
    let booted = Alarm::decode(kept.encode()).unwrap();
    assert_eq!(booted.check(7 * 60, 0), (Found::Nothing, booted));
    let (rang, _) = run(booted, None, at("2026-11-02 07:00:40"), 3600, 10);
    assert_eq!(rang, []);
}

#[test]
fn an_alarm_left_on_rings_at_its_minute_once_a_day() {
    for a in 0..DAY {
        // Set at noon on the 1st, for four days.
        let alarm = Alarm { at: a, on: true, spent: false, missed: false }.settled(12 * 60);
        let (rang, _) = run(alarm, None, at("2026-11-01 12:00:00"), 3 * 86_400 + 12 * 3600, 60);
        let minutes: Vec<_> = rang.iter().map(|r| (r.local.day, minute_of_day(r.local))).collect();
        let first = u8::from(a <= 12 * 60) + 1;
        assert_eq!(minutes, (first..=4).map(|d| (d, a)).collect::<Vec<_>>(), "{a}");
    }
}

#[test]
fn an_alarm_turned_off_never_rings_and_leaves_its_record_alone() {
    let off = Alarm { on: false, ..hm(7, 0) };
    for now in 0..DAY {
        assert_eq!(off.check(now, 0), (Found::Nothing, off));
        assert_eq!(off.next_check(now), None);
        assert_eq!(off.minutes_to_ring(now), None);
    }
}

#[test]
fn the_record_is_written_twice_a_day_at_most() {
    let mut a = hm(7, 0);
    let mut writes = 0;
    for now in (0..3 * DAY).map(|m| m % DAY) {
        let (_, after) = a.check(now, 0);
        writes += u32::from(after != a);
        a = after;
    }
    // Spent as it rings, re-armed thirteen hours later.
    assert_eq!(writes, 2 * 3);
}

#[test]
fn setting_it_in_or_just_after_its_minute_waits_for_tomorrow() {
    let a = hm(7, 0);
    assert!(!a.settled(6 * 60 + 59).spent);
    assert!(a.settled(7 * 60).spent);
    assert!(a.settled(7 * 60 + WINDOW + REPEAT - 1).spent);
    assert!(!a.settled(7 * 60 + WINDOW + REPEAT).spent);
    // Nor is it missed: set after its minute, it waits.
    let (found, after) = a.settled(7 * 60 + 30).check(7 * 60 + 31, 0);
    assert_eq!((found, after.missed), (Found::Nothing, false));
    let (rang, _) = run(a.settled(7 * 60), None, at("2026-11-02 07:00:30"), 86_400 + 60, 60);
    assert_eq!(rang, [Rang { local: civil("2026-11-03 07:00:30"), offset: 0 }]);
}

#[test]
fn the_time_to_ring_is_what_the_checks_find() {
    for a in [0, 1, 59, 420, 719, 1379, 1380, 1439] {
        for now in 0..DAY {
            for spent in [false, true] {
                let alarm = Alarm { at: a, on: true, spent, missed: false };
                let mut state = alarm;
                let found = (0..3 * DAY).find(|t| {
                    let (found, after) = state.check((now + t) % DAY, 0);
                    state = after;
                    found == Found::Ring
                });
                assert_eq!(alarm.minutes_to_ring(now), found, "alarm {a} at {now}, spent {spent}");
            }
        }
    }
}

/// Every minute of the day as an alarm, set at noon the day before `day` and run to its end,
/// on USB's 10 s passes or battery's minute: the rings on `day`.
fn every_alarm_across(zone: Zone, day: &str, step: i64) -> Vec<(Alarm, Vec<Rang>)> {
    let from = at(&format!("{day} 00:00:00")) - 12 * 3600;
    (0..DAY)
        .map(|a| {
            let alarm = Alarm { at: a, on: true, spent: false, missed: false }.settled(12 * 60);
            let (rang, _) = run(alarm, Some(zone), from, 36 * 3600, step);
            let on_day = civil(&format!("{day} 00:00:00")).day;
            (alarm, rang.into_iter().filter(|r| r.local.day == on_day).collect())
        })
        .collect()
}

#[test]
fn a_skipped_spring_hour_rings_its_alarms_at_the_change() {
    for step in [10, 60] {
        for (alarm, rang) in every_alarm_across(london(), "2027-03-28", step) {
            assert_eq!(rang.len(), 1, "{alarm:?} every {step} s: {rang:?}");
            let r = &rang[0];
            assert_eq!(r.local.day, 28, "{alarm:?}");
            if (60..120).contains(&alarm.at) {
                // 01:00 to 01:59 GMT never shows: the calendar goes from 00:59 to 02:00.
                assert_eq!((r.local, r.offset), (civil("2027-03-28 02:00:00"), 60), "{alarm:?}");
            } else {
                assert_eq!(minute_of_day(r.local), alarm.at, "{alarm:?}");
                assert!(r.local.second < 10 || step == 60 && r.local.second == 0, "{alarm:?}: {r:?}");
            }
        }
    }
}

#[test]
fn a_repeated_autumn_hour_rings_its_alarms_once_the_first_time_round() {
    for step in [10, 60] {
        for (alarm, rang) in every_alarm_across(london(), "2026-10-25", step) {
            assert_eq!(rang.len(), 1, "{alarm:?} every {step} s: {rang:?}");
            let r = &rang[0];
            assert_eq!((r.local.day, minute_of_day(r.local)), (25, alarm.at), "{alarm:?}");
            if (60..120).contains(&alarm.at) {
                assert_eq!(r.offset, 60, "{alarm:?} rang in the second 01:xx, GMT");
            }
        }
    }
}

#[test]
fn the_day_after_each_change_rings_as_usual() {
    for (zone, from) in [(london(), "2027-03-27 12:00:00"), (london(), "2026-10-24 12:00:00")] {
        for a in [0, 30, 60, 90, 119, 120, 150, 420, 1439] {
            let alarm = Alarm { at: a, on: true, spent: false, missed: false };
            let (rang, _) = run(alarm, Some(zone), at(from), 3 * 86_400, 60);
            assert_eq!(rang.len(), 3, "{a}: {rang:?}");
        }
    }
}

/// `release/GATE.md` step 5's forced change: `time zone <offset> T <offset±60>`.
fn forced(offset: i32, local: &str, shift: i32) -> Zone {
    let t = i64::from(utc(local)) - i64::from(offset) * 60;
    Zone::parse(&format!("{offset} {t} {}", offset + shift)).unwrap()
}

#[test]
fn a_forced_change_forward_over_the_alarm_rings_it_at_once() {
    let zone = forced(-300, "2026-11-02 14:00:15", 60);
    let alarm = hm(14, 30).settled(13 * 60 + 59);
    let (rang, _) = run(alarm, Some(zone), at("2026-11-02 13:59:55"), 600, 10);
    assert_eq!(rang, [Rang { local: civil("2026-11-02 15:00:15"), offset: -240 }]);
}

#[test]
fn a_forced_change_back_after_the_ring_does_not_ring_it_again() {
    for shift in [-60, -2] {
        let zone = forced(60, "2026-11-02 14:31:15", shift);
        let (rang, after) = run(hm(14, 30).settled(14 * 60 + 29), Some(zone), at("2026-11-02 14:29:55"), 3 * 3600, 10);
        assert_eq!(rang, [Rang { local: civil("2026-11-02 14:30:05"), offset: 60 }], "{shift}");
        assert!(after.spent);
        // As `alarm` says just after the change: tomorrow, not in an hour.
        let back = u16::try_from(14 * 60 + 31 + shift).unwrap();
        assert_eq!(hm(14, 30).check(14 * 60 + 30, 0).1.minutes_to_ring(back), Some(DAY + 870 - back), "{shift}");
    }
}

#[test]
fn a_jump_forward_of_more_than_late_misses_the_alarm() {
    let zone = forced(0, "2026-11-02 05:00:00", i32::from(LATE) + 60);
    let (rang, after) = run(hm(6, 0).settled(4 * 60), Some(zone), at("2026-11-02 04:59:00"), 3600, 60);
    assert_eq!(rang, []);
    assert!(after.missed && after.spent);
}

#[test]
fn travelling_across_zones_sets_the_clock_and_the_alarm_follows_it() {
    // `tools/wrist-time.py` in a new zone: `time set` moves the calendar, and the record is
    // settled again at the new local minute.
    let alarm = hm(7, 0);
    let landed = alarm.settled(minute_of_day(civil("2026-11-02 15:30:00")));
    let (rang, _) = run(landed, None, at("2026-11-02 15:30:00"), 86_400, 60);
    assert_eq!(rang, [Rang { local: civil("2026-11-03 07:00:00"), offset: 0 }]);
    let early = alarm.settled(minute_of_day(civil("2026-11-02 06:10:00")));
    let (rang, _) = run(early, None, at("2026-11-02 06:10:00"), 3600, 60);
    assert_eq!(rang, [Rang { local: civil("2026-11-02 07:00:00"), offset: 0 }]);
}

#[test]
fn a_check_within_the_grace_rings_it_and_a_later_one_marks_it_missed() {
    let a = hm(7, 0);
    for past in 0..WINDOW {
        let (found, after) = a.check(7 * 60 + past, 0);
        let expect = if past < GRACE { Found::Ring } else { Found::Missed };
        assert_eq!(found, expect, "{past}");
        assert!(after.spent);
        assert_eq!(after.missed, expect == Found::Missed, "{past}");
    }
    // Further than the window behind, the check leaves it for its next minute.
    assert_eq!(a.check(7 * 60 + WINDOW, 0), (Found::Nothing, a));
    assert_eq!(a.check(6 * 60 + 59, 0), (Found::Nothing, a));
}

#[test]
fn a_boot_after_the_watch_was_down_over_the_alarm() {
    // Down from 06:58 to 07:03: it rings at boot.
    let (rang, _, _) = run_woken(hm(7, 0), None, at("2026-11-02 07:03:20"), 600, 0);
    assert_eq!(rang, [Rang { local: civil("2026-11-02 07:03:20"), offset: 0 }]);
    // Down until 07:06: missed, said until a press, and tomorrow's rings as usual.
    let (rang, after, _) = run_woken(hm(7, 0), None, at("2026-11-02 07:06:00"), 86_400, 0);
    assert_eq!(rang, [Rang { local: civil("2026-11-03 07:00:00"), offset: 0 }]);
    assert!(!after.missed, "the next ring clears it");
    let (rang, after, _) = run_woken(hm(7, 0), None, at("2026-11-02 07:06:00"), 3600, 0);
    assert_eq!((rang, after.missed), (vec![], true));
}

#[test]
fn the_next_check_is_now_exactly_when_a_check_now_would_act() {
    for a in [0, 1, 59, 420, 719, 720, 1379, 1380, 1439] {
        for now in 0..DAY {
            for (spent, missed) in [(false, false), (true, false), (true, true)] {
                let alarm = Alarm { at: a, on: true, spent, missed };
                let acts = alarm.check(now, 0) != (Found::Nothing, alarm);
                let next = alarm.next_check(now).unwrap();
                assert_eq!(next == 0, acts, "alarm {a} at {now}, spent {spent}");
                assert!(next < DAY, "alarm {a} at {now}");
                // Nothing between now and the minute named would act.
                for t in 1..next {
                    assert_eq!(alarm.check((now + t) % DAY, 0), (Found::Nothing, alarm), "alarm {a} at {now}+{t}");
                }
            }
        }
    }
}

#[test]
fn waking_only_for_alarm_a_rings_as_checking_every_minute_does() {
    for a in 0..DAY {
        let alarm = Alarm { at: a, on: true, spent: false, missed: false }.settled(12 * 60);
        let from = at("2026-11-01 12:00:00");
        let (every, _) = run(alarm, None, from, 4 * 86_400, 60);
        let (woken, after, wakes) = run_woken(alarm, None, from, 4 * 86_400, 0);
        assert_eq!(woken, every, "{a}");
        assert!(!after.missed, "{a}");
        // The ring and the clearing: two wakes a day.
        assert!(wakes <= 2 * 4 + 1, "{a}: {wakes} wakes");
    }
}

#[test]
fn waking_only_for_alarm_a_rings_each_change_of_zone_as_checking_every_minute_does() {
    for (day, from) in [("2027-03-28", "2027-03-27 12:00:00"), ("2026-10-25", "2026-10-24 12:00:00")] {
        for a in 0..DAY {
            let alarm = Alarm { at: a, on: true, spent: false, missed: false }.settled(12 * 60);
            // USB applies the change within 10 s, battery within the minute.
            for lag in [0, 10, 59] {
                let (every, _) = run(alarm, Some(london()), at(from), 36 * 3600, 60);
                let (woken, after, _) = run_woken(alarm, Some(london()), at(from), 36 * 3600, lag);
                let minutes = |r: &[Rang]| r.iter().map(|r| (r.local.day, r.local.hour, r.local.minute, r.offset)).collect::<Vec<_>>();
                // Before a late change applies, the calendar still shows its minute: an alarm
                // on it rings there, once, instead of at the change.
                let at_its_minute = lag > 0
                    && minutes(&woken).len() == 1
                    && (woken[0].local.hour, woken[0].local.minute) == (alarm.hour(), alarm.minute());
                assert!(minutes(&woken) == minutes(&every) || at_its_minute, "{day} alarm {a}, lag {lag}: {woken:?} against {every:?}");
                assert!(!after.missed, "{day} alarm {a}, lag {lag}");
            }
        }
    }
}
