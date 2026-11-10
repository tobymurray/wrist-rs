//! The core on the fake chip: the phone's side, end to end.

use std::panic::{catch_unwind, AssertUnwindSafe};

use wrist_ble::{Millis, Op};
use wrist_ble_core::engine::Input;
use wrist_ble_core::goals::{Goals, Power};
use wrist_ble_core::phone::{Phone as PhoneState, ASK_TO_PAIR_AFTER};
use wrist_ble_sim::peers::{Keys, Os, Phone};
use wrist_ble_sim::sim::{on, Sim};

#[test]
fn the_watch_advertises_and_a_bonded_phone_connects() {
    let mut s = Sim::started(1, on(Power::Battery));
    assert!(s.line().contains("phone=discoverable(995ms)"), "{}", s.line());
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    s.run_for(Millis::secs(5));
    assert!(s.line().contains("phone=connected"), "{}\n{}", s.line(), s.report());
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(1));
    assert!(s.line().contains("phone=discoverable(995ms)"), "{}", s.line());
    s.power(Power::Cable);
    s.run_for(Millis::secs(1));
    assert!(s.line().contains("phone=discoverable(100ms)"), "{}", s.line());
    s.run_for(Millis::mins(2));
}


#[test]
fn on_cable_power_the_watch_advertises_fast() {
    let s = Sim::started(1, on(Power::Cable));
    assert!(s.line().contains("phone=discoverable(100ms)"), "{}", s.line());
}

#[test]
fn an_input_reaches_the_core() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.input(Input::Goals(Goals::OFF));
    s.run_for(Millis::secs(1));
    assert!(s.line().contains("phone=off"), "{}", s.line());
}

/// Runs in steps of 10 ms until the phone's link is up, failing if a minute of steps does not
/// bring it up.
fn linked(s: &mut Sim) {
    for _ in 0..6_000 {
        if s.fake.phone_link().is_some() {
            return;
        }
        s.run_for(Millis(10));
    }
    panic!("the phone never connected\n{}", s.report());
}

/// At one instant the radio's events go in before the core's own timer, so a timer never acts on
/// a state the radio has already moved on from.
#[test]
fn at_one_instant_the_radio_goes_before_the_core_timer() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.phone_arrives(Phone { keys: Keys::None, ..Phone::new(Os::Linux, 1) });
    linked(&mut s);
    let PhoneState::Up(l) = s.core.state.phone else { panic!("the core has no phone link") };
    let ask = l.since + ASK_TO_PAIR_AFTER;
    s.run_until(ask - Millis(1));
    s.fake.sync(ask);
    s.fake.unknown(0x0C99);
    s.run_until(ask);
    let report = s.report();
    let lines: Vec<&str> = report.lines().collect();
    let radio = lines.iter().position(|l| l.starts_with(&format!("{:>9} Radio(Unknown", ask.0)));
    let tick = lines.iter().position(|l| l.starts_with(&format!("{:>9} Tick", ask.0)));
    assert!(matches!((radio, tick), (Some(r), Some(t)) if r < t), "{report}");
}

#[test]
#[should_panic(expected = "the core spins")]
fn a_core_stepped_without_end_at_one_instant_fails_the_run() {
    let mut s = Sim::started(1, on(Power::Battery));
    for _ in 0..10_000 {
        s.input(Input::Tick);
    }
}

#[test]
fn a_failing_run_names_the_check_and_shows_the_recent_trace() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.submit(Op::Advertise(None)).unwrap();
    let e = catch_unwind(AssertUnwindSafe(|| s.run_for(Millis::secs(5)))).unwrap_err();
    let e = e.downcast_ref::<String>().unwrap();
    assert!(e.starts_with("seed 1: reachable: a gap"), "{e}");
    assert!(e.lines().any(|l| l.starts_with("        0 Goals(") && l.ends_with("-> Start(StartId(1)) ")), "{e}");
    assert!(e.lines().any(|l| l.contains("Started(StartId(1), Ok(") && l.contains("-> Advertise(Some(")), "{e}");
}

#[test]
fn the_trace_keeps_the_recent_steps_and_lets_the_oldest_go() {
    let mut s = Sim::started(1, on(Power::Battery));
    let first = s.report().lines().next().unwrap().to_owned();
    for _ in 0..1_000 {
        s.run_for(Millis(1));
        s.input(Input::Tick);
    }
    let report = s.report();
    let last = format!("{:>9} Tick -> ", s.now.0);
    assert_eq!(report.lines().last(), Some(last.as_str()), "{report}");
    assert!(report.lines().count() > 1, "{report}");
    assert!(!report.contains(&first), "{report}");
}

/// A snapshot carries no advertising interval, and needs none while a new interval is a stop then a
/// start (`a_new_advertising_interval_is_a_stop_then_a_start`).
#[test]
fn a_new_interval_whose_events_are_lost_leaves_the_core_at_the_chips() {
    // The stop's outcome lost, then the start's too.
    for lost in 1..=2 {
        for (from, to) in [(Power::Battery, Power::Cable), (Power::Cable, Power::Battery)] {
            let mut s = Sim::started(1, on(from));
            s.fake.lose_events(lost);
            s.power(to);
            s.run_for(Millis::mins(1));
            assert_eq!(s.core.state.radio.advertising, s.fake.advertising(), "{lost} lost, {from:?} to {to:?}\n{}", s.report());
            assert_eq!(s.core.counters.divergences, 0, "{lost} lost, {from:?} to {to:?}\n{}", s.report());
            assert_eq!(s.core.counters.resets, 0, "{lost} lost, {from:?} to {to:?}\n{}", s.report());
        }
    }
}
