//! The status on the console: when it changes by time alone, and which changes print a line.

mod common;

use common::*;
use wrist_ble::policy::phone;
use wrist_ble::policy::plan::{Conflict, Verdict};
use wrist_ble::policy::status::{next_change, status, PhoneStatus, Status, StatusLog, LOG_GAP};
use wrist_ble::policy::State;
use wrist_ble::{Millis, PairingStep};

/// Every millisecond in `0..to` at which the shape changes is the change `next_change` named a
/// millisecond before, and it names none that doesn't come.
fn changes_only_when_named(state: &State, to: u64) -> usize {
    let mut seen = 0;
    for ms in 1..to {
        let (before, now) = (t(ms - 1), t(ms));
        let changed = status(state, before).shape() != status(state, now).shape();
        let named = next_change(state, before);
        assert!(named.is_none_or(|at| at > before), "named the past at {ms}");
        if changed {
            seen += 1;
            assert_eq!(named, Some(now), "a change at {ms} ms not named");
        } else if named == Some(now) {
            panic!("named a change at {ms} ms that didn't come");
        }
    }
    seen
}

#[test]
fn a_link_left_unencrypted_turns_unsecured_when_named() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(1_000));
    assert_eq!(changes_only_when_named(&state, 12_000), 1);
    assert_eq!(next_change(&state, t(2_000)), Some(t(1_000) + phone::ASK_TO_PAIR_AFTER + Millis(1)));
}

#[test]
fn a_planned_gap_turns_unreachable_when_named() {
    let mut state = ready(t(0));
    state.phone_verdict = Verdict::Busy(Conflict::Scan);
    assert_eq!(changes_only_when_named(&state, 10_000), 1);
}

#[test]
fn nothing_is_named_while_nothing_changes_by_time() {
    let mut state = ready(t(0));
    state.radio.advertising = Some(ADV_995);
    assert_eq!(next_change(&state, t(10_000)), None);
    assert_eq!(changes_only_when_named(&state, 10_000), 0);
}

#[test]
fn a_peripheral_is_looked_at_each_log_gap() {
    let ctx = ctx(&HR);
    let state = with_peer(ready(t(0)), &HR, live(&ctx, t(0)));
    assert_eq!(next_change(&state, t(5_000)), Some(t(5_000) + LOG_GAP));
}

#[test]
fn the_shape_keeps_what_changes_and_drops_ages() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    assert_eq!(status(&state, t(6_000)).shape(), status(&state, t(60_000)).shape());
    assert_ne!(status(&state, t(4_000)).shape(), status(&state, t(6_000)).shape());
    state.phone = state.phone.step(t(1_000), phone::Event::Pairing(phone(), PairingStep::Numeric(123_456))).0;
    assert_eq!(status(&state, t(2_000)).shape(), status(&state, t(20_000)).shape());
    let mut other = state;
    other.phone = ready(t(0)).phone;
    phone_up(&mut other, t(0));
    other.phone = other.phone.step(t(1_000), phone::Event::Pairing(phone(), PairingStep::Numeric(654_321))).0;
    assert_ne!(status(&state, t(2_000)).shape(), status(&other, t(2_000)).shape());
}

fn securing() -> Status {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    status(&state, t(0))
}

fn advertising() -> Status {
    let mut state = ready(t(0));
    state.radio.advertising = Some(ADV_995);
    status(&state, t(0))
}

#[test]
fn the_log_prints_the_first_status_then_only_changes() {
    let mut log = StatusLog::NEW;
    assert!(log.due(t(0), &advertising()));
    assert!(!log.due(t(5_000), &advertising()));
    assert!(log.due(t(6_000), &securing()));
    assert_eq!(log.next(), None);
}

#[test]
fn a_change_within_the_gap_is_held_until_it_ends() {
    let mut log = StatusLog::NEW;
    assert!(log.due(t(0), &advertising()));
    assert!(!log.due(t(400), &securing()));
    assert_eq!(log.next(), Some(t(0) + LOG_GAP));
    assert!(log.due(t(1_000), &securing()));
    assert_eq!(log.next(), None);
}

/// A flicker that is back where it was by the end of the gap prints nothing.
#[test]
fn a_change_undone_within_the_gap_prints_nothing() {
    let mut log = StatusLog::NEW;
    assert!(log.due(t(0), &advertising()));
    assert!(!log.due(t(300), &securing()));
    assert!(!log.due(t(600), &advertising()));
    assert_eq!(log.next(), None);
    assert!(!log.due(t(1_000), &advertising()));
}

/// The status as the radio goes is printed even within the gap, and holds nothing back.
#[test]
fn the_last_status_is_printed_whatever_the_gap() {
    let mut log = StatusLog::NEW;
    assert!(log.due(t(0), &advertising()));
    assert!(log.last(t(200), &securing()));
    assert_eq!(log.next(), None);
    assert!(!log.last(t(300), &securing()));
}

#[test]
fn an_encrypted_link_changes_by_no_time() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    state.phone = state.phone.step(t(800), phone::Event::Encryption(phone(), true)).0;
    assert_eq!(next_change(&state, t(1_000)), None);
    assert_eq!(status(&state, t(1_000)).phone, PhoneStatus::Connected);
}

#[test]
fn the_shape_drops_a_peripherals_ages_and_the_drift() {
    let ctx = ctx(&HR);
    let machine = live(&ctx, t(0)).step(&ctx, t(100), bpm(72));
    let mut state = with_peer(ready(t(0)), &HR, machine);
    let (early, late) = (status(&state, t(500)), status(&state, t(900)));
    assert_ne!(early.peers, late.peers);
    assert_eq!(early.shape(), late.shape());
    state.drift_since = Some(t(0));
    let (early, late) = (status(&state, t(500)), status(&state, t(900)));
    assert_ne!(early.drift, late.drift);
    assert_eq!(early.shape(), late.shape());
}

/// Only a live peripheral's readings age; a search moves on its own deadlines.
#[test]
fn a_peripheral_that_cannot_change_by_time_wakes_nothing() {
    use wrist_ble::policy::peripheral::{Event, Peripheral};
    let ctx = ctx(&HR);
    for machine in [Peripheral::default(), Peripheral::default().step(&ctx, t(0), Event::Want)] {
        let state = with_peer(ready(t(0)), &HR, machine);
        assert_eq!(next_change(&state, t(5_000)), None);
        assert_eq!(status(&state, t(5_000)).shape(), status(&state, t(600_000)).shape());
    }
}
