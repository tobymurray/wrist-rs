mod common;

use core::mem::{discriminant, Discriminant};

use common::*;
use wrist_ble::Millis;
use wrist_ble_core::peripheral::{Ctx, Event, Peripheral, Stage, GIVE_UP};
use wrist_ble_core::plan::Conflict;
use wrist_ble_core::status::{peer_state, PeerState};

fn slow_searching() -> (Ctx<'static>, Peripheral, wrist_ble::Instant) {
    let c = Ctx { workout: true, ..ctx(&HR) };
    let at = t(u64::from(GIVE_UP.0) + 120_000);
    let p = run(Peripheral::default().step(&c, t(0), Event::Want), &c, t(0), at);
    assert!(p.slow_until.is_some() && matches!(p.stage, Stage::Seeking { .. }), "{p:?}");
    (c, p, at)
}

#[test]
fn a_strap_the_slow_search_finds_is_not_given_up_as_it_connects() {
    let (c, p, at) = slow_searching();
    let p = p.step(&c, at + Millis(500), Event::LinkUp(sensor())).step(&c, at + Millis(600), Event::Tick);
    assert!(matches!(p.stage, Stage::Linked { .. }), "{p:?}");
    assert_eq!(p.deadline(&c, at + Millis(600)), p.slow_until);
    let p = [Event::Discovered(true), Event::Subscribed(true), Event::Tick].into_iter().fold(p, |p, e| p.step(&c, at + Millis(700), e));
    assert!(matches!(p.stage, Stage::Live { .. }), "{p:?}");
    assert_eq!(p.step(&c, at + Millis(1_000), bpm(70)).slow_until, None);
}

#[test]
fn a_peripheral_let_go_during_the_slow_search_has_no_deadline_left_behind() {
    let (c, p, at) = slow_searching();
    let end = p.slow_until.unwrap() + Millis::mins(60);
    for e in [Event::Unwant, Event::Busy(Conflict::Transfer)] {
        run(p.step(&c, at, e), &c, at, end);
    }
    let freed = p.step(&c, at, Event::Busy(Conflict::Transfer)).step(&c, p.slow_until.unwrap() + Millis::mins(5), Event::Free);
    run(freed, &c, p.slow_until.unwrap() + Millis::mins(5), end);
}

fn class(s: PeerState) -> (Discriminant<PeerState>, Option<bool>) {
    let current = match s {
        PeerState::Live { current, .. } | PeerState::Unstable { current, .. } => Some(current),
        _ => None,
    };
    (discriminant(&s), current)
}

/// A display that renders at every deadline the machine gives is never more than a second behind.
#[test]
fn the_deadline_announces_every_change_of_status() {
    let c = ctx(&HR);
    let mut p = live(&c, t(0));
    for s in 1..=40u64 {
        p = p.step(&c, secs(s), bpm(70));
    }
    let mut now = secs(40);
    let mut shown = class(peer_state(&p, &HR, now));
    let mut due = p.deadline(&c, now);
    let mut behind_since = None;
    while now < secs(75) {
        now = now + Millis(100);
        if due.is_some_and(|d| d <= now) {
            p = p.step(&c, now, Event::Tick);
            shown = class(peer_state(&p, &HR, now));
            due = p.deadline(&c, now);
        }
        if class(peer_state(&p, &HR, now)) == shown {
            behind_since = None;
        } else {
            let since = *behind_since.get_or_insert(now);
            assert!(now.since(since) <= Millis(1_000), "{:?} at {now:?}, shown {shown:?} until {due:?}", peer_state(&p, &HR, now));
        }
    }
}

#[test]
fn an_idle_sensor_found_by_the_slow_search_has_no_deadline_left_behind() {
    let c = Ctx { workout: true, ..ctx(&CSC) };
    let at = t(u64::from(GIVE_UP.0) + 120_000);
    let p = run(Peripheral::default().step(&c, t(0), Event::Want), &c, t(0), at);
    let p = [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)].into_iter().fold(p, |p, e| p.step(&c, at, e));
    run(p, &c, at, at + Millis::mins(60));
}

#[test]
fn a_reading_ends_the_loss_a_later_reconnect_reports() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(70)).step(&c, t(10_000), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TIMEOUT));
    let p = [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true), bpm(70)].into_iter().fold(p, |p, e| p.step(&c, t(12_000), e));
    assert_eq!(p.lost_since, None);
    let p = p.step(&c, t(100_000), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TIMEOUT));
    assert_eq!(peer_state(&p, &HR, t(101_000)), PeerState::Reconnecting { since: Millis(1_000), attempts: p.attempts });
}

#[test]
fn going_live_restarts_the_backoff_and_the_rediscoveries() {
    let c = ctx(&HR);
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    for _ in 0..3 {
        p = p.step(&c, t(0), Event::LinkUp(sensor())).step(&c, t(0), Event::Discovered(false)).step(&c, t(0), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TERMINATED_BY_LOCAL_HOST));
        let Stage::Waiting { until } = p.stage else { panic!("{p:?}") };
        p = p.step(&c, until, Event::Tick).step(&c, until, Event::ConnectFailed(wrist_ble::FailureKind::Timeout));
        let Stage::Backoff { until } = p.stage else { panic!("{p:?}") };
        p = p.step(&c, until, Event::Tick);
    }
    p = [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)].into_iter().fold(p, |p, e| p.step(&c, t(60_000), e));
    p = p.step(&c, t(61_000), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TIMEOUT)).step(&c, t(61_000), Event::ConnectFailed(wrist_ble::FailureKind::Timeout));
    let Stage::Backoff { until } = p.stage else { panic!("{p:?}") };
    assert!(until.since(t(61_000)) < Millis(2_000), "{p:?}");
    let p = p.step(&c, until, Event::Tick).step(&c, until, Event::LinkUp(sensor())).step(&c, until, Event::Discovered(false));
    assert!(matches!(p.stage, Stage::Dropping { then: wrist_ble_core::peripheral::Then::Rediscover, .. }), "{p:?}");
}

static UNPACED: wrist_ble_core::profile::Profile = wrist_ble_core::profile::Profile { cadence: wrist_ble_core::profile::Cadence::Events, ..HR };

#[test]
fn a_stale_drop_counts_towards_unstable() {
    let c = ctx(&UNPACED);
    let p = run(live(&c, t(0)).step(&c, t(1_000), bpm(70)), &c, t(1_000), t(31_000));
    let p = p.step(&c, t(31_000), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TERMINATED_BY_LOCAL_HOST));
    let p = [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true), bpm(70)].into_iter().fold(p, |p, e| p.step(&c, t(33_000), e));
    let p = p.step(&c, t(40_000), Event::LinkDown(sensor(), wrist_ble::Status::CONN_TIMEOUT));
    assert!(p.unstable(&UNPACED, t(40_000), false), "{p:?}");
}

#[test]
fn a_reading_without_contact_is_not_shown_once_quiet() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), Event::Reading(wrist_ble_core::profile::Reading { value: 70, contact: wrist_ble_core::profile::Contact::Lost }));
    assert_eq!(peer_state(&p, &HR, t(7_000)), PeerState::Quiet { value: None, age: Millis(6_000) });
}

#[test]
fn a_search_resumed_after_the_radio_was_busy_is_a_full_one() {
    let (c, p, at) = slow_searching();
    let c = Ctx { workout: false, ..c };
    let p = p.step(&c, at, Event::Busy(Conflict::Transfer)).step(&c, at + Millis(1_000), Event::Free);
    assert_eq!(p.slow_until, None);
    assert_eq!(peer_state(&p, &HR, at + Millis(1_000)), PeerState::Finding { slow: false, since: (at + Millis(1_000)).since(t(0)) });
}

#[test]
fn a_slow_search_find_is_let_go_when_the_workout_ends() {
    let (c, p, at) = slow_searching();
    let p = p.step(&c, at + Millis(500), Event::LinkUp(sensor()));
    let done = Ctx { workout: false, ..c };
    let due = p.deadline(&done, at + Millis(600)).unwrap();
    assert!(due <= at + Millis(600), "{due:?}");
    assert!(matches!(p.step(&done, at + Millis(600), Event::Tick).stage, Stage::Dropping { .. }));
}

#[test]
fn an_idle_sensor_is_not_woken_for_coverage() {
    let c = ctx(&CSC);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(80));
    let p = run(p, &c, t(1_000), t(40_000));
    assert_eq!(p.deadline(&c, t(40_000)), None);
}
