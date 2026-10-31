//! One test per row of the status tables: the phone's, then each peripheral's.

mod common;

use common::*;
use wrist_ble::{Status, Millis, PairingStep};
use wrist_ble_core::chip;
use wrist_ble_core::peripheral::{Event, Peripheral, StopReason, GIVE_UP};
use wrist_ble_core::phone;
use wrist_ble_core::plan::{Conflict, Verdict};
use wrist_ble_core::profile::{Contact, Reading};
use wrist_ble_core::status::{status, PeerState, PhoneStatus, RadioStatus, Unreachable};
use wrist_ble_core::State;

fn phone_token(state: &State, now: wrist_ble::Instant) -> String {
    let l = line(state, now);
    l.split(' ').find_map(|w| w.strip_prefix("phone=")).unwrap().to_string()
}

#[test]
fn phone_off_when_the_radio_is_off() {
    let s = status(&State::OFF, t(0));
    assert_eq!((s.phone, s.radio), (PhoneStatus::Off, RadioStatus::Off));
    assert_eq!(line(&State::OFF, t(0)), "[ble] status phone=off peers=- radio=off plan=ok drift=0s");
}

#[test]
fn phone_fault_once_the_reset_budget_is_spent() {
    let mut state = ready(t(0));
    let mut c = state.chip.step(t(0), chip::Event::RadioOff).0;
    c = c.step(t(0), chip::Event::RadioOn).0;
    for _ in 0..=chip::RESETS_PER_HOUR {
        c = c.step(t(1_000), chip::Event::StartFailed).0;
    }
    state.chip = c;
    assert!(matches!(status(&state, t(2_000)).phone, PhoneStatus::Fault { cause: chip::FaultCause::StartFailed, .. }));
    assert_eq!(phone_token(&state, t(2_000)), "fault(start-failed)");
}

#[test]
fn phone_pairing_shows_the_value_and_the_time_left() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    state.phone = state.phone.step(t(1_000), phone::Event::Pairing(phone(), PairingStep::Numeric(123_456))).0;
    let now = t(19_000);
    assert_eq!(status(&state, now).phone, PhoneStatus::Pairing { value: 123_456, left: Millis::secs(42) });
    assert_eq!(phone_token(&state, now), "pairing(123456,42s)");
}

#[test]
fn phone_connected_only_when_encrypted() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    state.phone = state.phone.step(t(800), phone::Event::Encryption(phone(), true)).0;
    assert_eq!(status(&state, t(900)).phone, PhoneStatus::Connected);
    assert_eq!(phone_token(&state, t(900)), "connected");
}

#[test]
fn phone_securing_within_the_first_seconds() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    assert_eq!(status(&state, t(4_000)).phone, PhoneStatus::Securing);
    assert_eq!(phone_token(&state, t(4_000)), "securing");
}

#[test]
fn phone_unsecured_after_five_seconds() {
    let mut state = ready(t(0));
    phone_up(&mut state, t(0));
    assert_eq!(status(&state, t(12_000)).phone, PhoneStatus::Unsecured { since: Millis::secs(12) });
    assert_eq!(phone_token(&state, t(12_000)), "unsecured(12s)");
}

#[test]
fn phone_discoverable_with_the_interval() {
    let mut state = ready(t(0));
    state.radio.advertising = Some(ADV_995);
    assert_eq!(status(&state, t(60_000)).phone, PhoneStatus::Discoverable { interval_ms: Some(995) });
    assert_eq!(phone_token(&state, t(60_000)), "discoverable(995ms)");
}

#[test]
fn phone_discoverable_through_a_planned_gap() {
    let state = ready(t(0));
    assert_eq!(status(&state, t(3_000)).phone, PhoneStatus::Discoverable { interval_ms: None });
    assert_eq!(phone_token(&state, t(3_000)), "discoverable");
}

#[test]
fn phone_unreachable_past_the_gap_with_the_reason() {
    let mut state = ready(t(0));
    state.phone_verdict = Verdict::Busy(Conflict::Peripheral);
    assert_eq!(
        status(&state, t(4_000)).phone,
        PhoneStatus::Unreachable { reason: Unreachable::Busy(Conflict::Peripheral), since: Millis::secs(4) }
    );
    assert_eq!(phone_token(&state, t(4_000)), "unreachable(peripheral,4s)");
    state.phone_verdict = Verdict::Fits;
    assert_eq!(phone_token(&state, t(4_000)), "unreachable(not-converged,4s)");
    state.phone_verdict = Verdict::NotWanted;
    assert_eq!(phone_token(&state, t(4_000)), "unreachable(hidden,4s)");
}

#[test]
fn phone_unreachable_while_the_chip_starts() {
    let mut state = ready(t(0));
    state.chip = chip::ChipMachine::OFF.step(t(0), chip::Event::RadioOn).0;
    assert_eq!(status(&state, t(1_000)).phone, PhoneStatus::Unreachable { reason: Unreachable::Starting, since: Millis::secs(1) });
    assert_eq!(status(&state, t(1_000)).radio, RadioStatus::Starting);
}

fn hr(machine: Peripheral) -> State {
    with_peer(ready(t(0)), &HR, machine)
}

fn peer_token(state: &State, now: wrist_ble::Instant) -> String {
    let l = line(state, now);
    let rest = l.split(" peers=").nth(1).unwrap();
    rest.split(" radio=").next().unwrap().to_string()
}

#[test]
fn peer_off_when_not_wanted() {
    let state = hr(Peripheral::default());
    assert_eq!(peer_state(&state, t(0)), PeerState::Off);
    assert_eq!(peer_token(&state, t(0)), "hr:off");
}

#[test]
fn peer_finding_with_time_since_chosen() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want);
    let state = hr(run(p, &c, t(0), t(4_000)));
    assert_eq!(peer_state(&state, t(4_000)), PeerState::Finding { slow: false, since: Millis::secs(4) });
    assert_eq!(peer_token(&state, t(4_000)), "hr:finding(4s)");
}

#[test]
fn peer_connecting_once_linked() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(900), Event::LinkUp(sensor()));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(1_000)), PeerState::Connecting);
    assert_eq!(peer_token(&state, t(1_000)), "hr:connecting");
}

#[test]
fn peer_live_with_value_and_age() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(72));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(1_800)), PeerState::Live { value: 72, age: Millis(800), current: true });
    assert_eq!(peer_token(&state, t(1_800)), "hr:live(72bpm,0.8s)");
}

#[test]
fn peer_unstable_after_two_drops() {
    let c = ctx(&HR);
    let mut p = live(&c, t(0)).step(&c, t(1_000), bpm(70));
    for at in [10_000, 20_000] {
        p = p.step(&c, t(at), Event::LinkDown(sensor(), Status::REMOTE_USER_TERMINATED_CONN));
        for e in [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)] {
            p = p.step(&c, t(at + 1_000), e);
        }
    }
    let p = p.step(&c, t(22_000), bpm(72));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(22_500)), PeerState::Unstable { value: Some(72), age: Millis(500), current: true });
    assert_eq!(peer_token(&state, t(22_500)), "hr:unstable(72bpm,0.5s)");
}

#[test]
fn peer_idle_when_an_idle_profile_goes_silent() {
    let c = ctx(&CSC);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(80));
    let p = run(p, &c, t(1_000), t(60_000));
    let state = with_peer(ready(t(0)), &CSC, p);
    assert_eq!(peer_state(&state, t(60_000)), PeerState::Idle);
    assert_eq!(peer_token(&state, t(60_000)), "csc:idle");
}

#[test]
fn peer_no_contact_shows_no_value() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), Event::Reading(Reading { value: 0, contact: Contact::Lost }));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(1_500)), PeerState::NoContact);
    assert_eq!(peer_token(&state, t(1_500)), "hr:no-contact");
}

#[test]
fn peer_quiet_shows_the_value_with_its_age() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(72));
    let state = hr(run(p, &c, t(1_000), t(13_000)));
    assert_eq!(peer_state(&state, t(13_000)), PeerState::Quiet { value: Some(72), age: Millis::secs(12) });
    assert_eq!(peer_token(&state, t(13_000)), "hr:quiet(72bpm,12s)");
}

#[test]
fn peer_reconnecting_since_the_link_was_lost() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(72)).step(&c, t(2_000), Event::LinkDown(sensor(), Status::CONN_TIMEOUT));
    let state = hr(run(p, &c, t(2_000), t(14_000)));
    assert!(matches!(peer_state(&state, t(14_000)), PeerState::Reconnecting { since: Millis(12_000), .. }));
    assert_eq!(peer_token(&state, t(14_000)), "hr:reconnecting(12s)");
}

#[test]
fn peer_rechecking_while_it_waits_to_look_for_the_service_again() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(100), Event::LinkUp(sensor())).step(&c, t(200), Event::Discovered(false));
    assert_eq!(peer_token(&hr(p), t(250)), "hr:rechecking");
    let p = p.step(&c, t(300), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(2_300)), PeerState::Rechecking { left: Some(Millis::secs(8)) });
    assert_eq!(peer_token(&state, t(2_300)), "hr:rechecking(8s)");
    let state = hr(run(p, &c, t(300), t(10_400)));
    assert!(matches!(peer_state(&state, t(10_400)), PeerState::Reconnecting { since: Millis(10_200), .. }), "{:?}", peer_state(&state, t(10_400)));
}

#[test]
fn peer_reconnecting_after_a_procedure_failed_on_its_link() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(100), Event::LinkUp(sensor())).step(&c, t(5_000), Event::ProcedureFailed);
    assert_eq!(peer_token(&hr(p), t(5_000)), "hr:reconnecting(0s)");
    let p = p.step(&c, t(5_100), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    assert_eq!(peer_token(&hr(p), t(7_100)), "hr:reconnecting(2s)");
}

#[test]
fn peer_stopped_kept_dropping() {
    let c = ctx(&HR);
    let mut p = live(&c, t(0)).step(&c, t(1_000), bpm(72));
    let mut now = 2_000;
    while now < 1_000 + u64::from(GIVE_UP.0) {
        p = p.step(&c, t(now), Event::LinkDown(sensor(), Status::REMOTE_USER_TERMINATED_CONN));
        p = p.step(&c, t(now + 500), Event::LinkUp(sensor()));
        now += 10_000;
    }
    let state = hr(run(p, &c, t(now), t(now + 1_000)));
    assert_eq!(peer_state(&state, t(now + 1_000)), PeerState::Stopped(StopReason::KeptDropping));
    assert_eq!(peer_token(&state, t(now + 1_000)), "hr:stopped(kept-dropping)");
}

#[test]
fn peer_stopped_not_found() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want);
    let end = t(u64::from(GIVE_UP.0) + 1_000);
    let state = hr(run(p, &c, t(0), end));
    assert_eq!(peer_state(&state, end), PeerState::Stopped(StopReason::NotFound));
    assert_eq!(peer_token(&state, end), "hr:stopped(not-found)");
}

#[test]
fn peer_stopped_while_the_radio_is_busy() {
    let c = ctx(&HR);
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(100), Event::Busy(Conflict::Transfer));
    let state = hr(p);
    assert_eq!(peer_state(&state, t(200)), PeerState::Stopped(StopReason::RadioBusy(Conflict::Transfer)));
    assert_eq!(peer_token(&state, t(200)), "hr:stopped(busy-transfer)");
}

#[test]
fn peer_incompatible_after_every_rediscovery() {
    let c = ctx(&HR);
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    let mut now = 0;
    for _ in 0..4 {
        p = p.step(&c, t(now + 100), Event::LinkUp(sensor()));
        p = p.step(&c, t(now + 200), Event::Discovered(false));
        p = p.step(&c, t(now + 300), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
        p = run(p, &c, t(now + 300), t(now + 11_000));
        now += 11_000;
    }
    let state = hr(p);
    assert_eq!(peer_state(&state, t(now)), PeerState::Incompatible);
    assert_eq!(peer_token(&state, t(now)), "hr:incompatible");
}

#[test]
fn peer_slow_search_in_a_workout() {
    let c = wrist_ble_core::peripheral::Ctx { workout: true, ..ctx(&HR) };
    let p = Peripheral::default().step(&c, t(0), Event::Want);
    let end = t(u64::from(GIVE_UP.0) + 120_000);
    let state = hr(run(p, &c, t(0), end));
    assert!(matches!(peer_state(&state, end), PeerState::Finding { slow: true, .. }));
    assert_eq!(peer_token(&state, end), "hr:finding(slow)");
}

#[test]
fn the_whole_line() {
    let c = ctx(&HR);
    let mut state = hr(live(&c, t(0)).step(&c, t(1_000), bpm(72)));
    phone_up(&mut state, t(0));
    state.phone = state.phone.step(t(500), phone::Event::Encryption(phone(), true)).0;
    let rower = wrist_ble_core::profile::Profile { kind: wrist_ble_core::profile::Kind::Ftms(wrist_ble_core::profile::Machine::Rower), ..HR };
    let rower: &'static _ = Box::leak(Box::new(rower));
    let lost = live(&c, t(0)).step(&c, t(500), Event::LinkDown(sensor(), Status::CONN_TIMEOUT));
    state.peers[1] = Some(wrist_ble_core::Peer { want: wrist_ble_core::goals::Want { profile: rower, peer: peer(), priority: 1 }, machine: lost });
    assert_eq!(line(&state, t(1_800)), "[ble] status phone=connected peers=hr:live(72bpm,0.8s) rower:reconnecting(1s) radio=ok plan=ok drift=0s");
}
