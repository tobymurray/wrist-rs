use wrist_time::zone::{Change, Zone};
use wrist_time::{civil_of_seconds, parse_civil, seconds_of};

fn at(text: &str) -> i64 {
    seconds_of(parse_civil(text).unwrap())
}

fn utc(text: &str) -> u32 {
    u32::try_from(at(text)).unwrap()
}

/// The UK from summer 2026: back to GMT on 25 October, forward on 28 March.
fn london() -> Zone {
    Zone::parse(&format!(
        "60 {} 0 {} 60",
        utc("2026-10-25 01:00:00"),
        utc("2027-03-28 01:00:00")
    ))
    .unwrap()
}

/// What the display task does on a wake: one change at most.
fn wake(zone: &mut Zone, local: &mut i64) -> bool {
    match zone.due(*local) {
        Some((shift, after)) => {
            *local += i64::from(shift) * 60;
            *zone = after;
            true
        }
        None => false,
    }
}

#[test]
fn unset_registers_are_no_record() {
    assert_eq!(Zone::decode([0; 5]), None);
    assert_eq!(Zone::decode([0xFFFF_FFFF; 5]), None);
    assert_eq!(Zone::decode([0x5447_003C, 0, 0, 0, 0]), None);
    let zone = Zone::decode([0x545A_003C, 0, 0x1234, 0, 0]).unwrap();
    assert_eq!(zone, Zone { offset: 60, next: [None, None] });
    assert_eq!(zone.due(at("2099-01-01 00:00:00")), None);
}

#[test]
fn a_change_left_alone_in_the_second_slot_still_comes_due() {
    let spring = utc("2027-03-28 01:00:00");
    let zone = Zone::decode([0x545A_0000, 0, 0, 0x5432_003C, spring]).unwrap();
    assert_eq!(zone.next, [Some(Change { at: spring, offset: 60 }), None]);
    assert_eq!(zone.due(at("2027-03-28 01:00:00")).map(|(shift, _)| shift), Some(60));
}

#[test]
fn records_round_trip() {
    for zone in [
        london(),
        Zone::parse("0").unwrap(),
        Zone::parse("-300 1793516400 -240").unwrap(),
        Zone::parse("330").unwrap(),
        Zone::parse("-840 1 840 2 0").unwrap(),
    ] {
        assert_eq!(Zone::decode(zone.encode()), Some(zone));
    }
    assert_eq!(london().encode()[0], 0x545A_003C);
    assert_eq!(Zone::parse("-300").unwrap().encode(), [0x545A_FED4, 0, 0, 0, 0]);
}

#[test]
fn bad_commands_are_refused() {
    for text in ["", "x", "841", "-841", "60 100", "60 100 0 50 60", "60 1 0 2 60 3 0", "60 1 900"] {
        assert_eq!(Zone::parse(text), None, "{text}");
    }
}

#[test]
fn a_change_in_the_future_waits() {
    let zone = london();
    assert_eq!(zone.due(at("2026-10-21 12:00:00")), None);
    assert_eq!(zone.due(at("2026-10-25 01:59:59")), None);
}

#[test]
fn a_change_just_due_moves_the_calendar() {
    let (shift, after) = london().due(at("2026-10-25 02:00:00")).unwrap();
    assert_eq!(shift, -60);
    assert_eq!(after.offset, 0);
    assert_eq!(after.next, [Some(Change { at: utc("2027-03-28 01:00:00"), offset: 60 }), None]);
}

#[test]
fn the_repeated_hour_is_applied_once() {
    let mut zone = london();
    let mut local = at("2026-10-25 02:00:00");
    assert!(wake(&mut zone, &mut local));
    assert_eq!(civil_of_seconds(local), parse_civil("2026-10-25 01:00:00").unwrap());
    for minute in 0..=60 {
        assert!(!wake(&mut zone, &mut (local + minute * 60)), "{minute}");
    }
}

#[test]
fn two_changes_apply_in_order_across_two_wakes() {
    let mut zone = london();
    let mut local = at("2027-04-01 12:00:00");
    assert!(wake(&mut zone, &mut local));
    assert_eq!(zone.offset, 0);
    assert!(wake(&mut zone, &mut local));
    assert_eq!(zone, Zone { offset: 60, next: [None, None] });
    assert_eq!(civil_of_seconds(local), parse_civil("2027-04-01 12:00:00").unwrap());
    assert!(!wake(&mut zone, &mut local));
}

#[test]
fn offsets_of_zero_and_west_of_utc() {
    // Toronto's 2026 fall-back, 06:00 UTC, from -4:00 to -5:00.
    let mut zone = Zone::parse(&format!("-240 {} -300", utc("2026-11-01 06:00:00"))).unwrap();
    assert_eq!(Zone::decode(zone.encode()), Some(zone));
    let mut local = at("2026-11-01 01:59:00");
    assert!(!wake(&mut zone, &mut local));
    local = at("2026-11-01 02:00:00");
    assert!(wake(&mut zone, &mut local));
    assert_eq!(civil_of_seconds(local), parse_civil("2026-11-01 01:00:00").unwrap());
    assert_eq!(zone, Zone { offset: -300, next: [None, None] });
    // UTC with a change into +1:00.
    let mut zone = Zone::parse(&format!("0 {} 60", utc("2027-03-28 01:00:00"))).unwrap();
    let mut local = at("2027-03-28 00:59:00");
    assert!(!wake(&mut zone, &mut local));
    local = at("2027-03-28 01:00:00");
    assert!(wake(&mut zone, &mut local));
    assert_eq!(civil_of_seconds(local), parse_civil("2027-03-28 02:00:00").unwrap());
}
