mod common;

use common::*;
use wrist_ble::{Activities, Admitted, Change, ConnParams, Duration, Exclusion, FailureKind, PairingStep, SchedKind, Schedule, Status};
use wrist_ble_core::chip::{self, Chip, ChipMachine, FaultCause};
use wrist_ble_core::goals::Power;
use wrist_ble_core::peripheral::{self as per, Event, Peripheral, Stage, StopReason, Then, GIVE_UP};
use wrist_ble_core::phone::{self, Action, Phone, Security};
use wrist_ble_core::plan::{fingerprint, Conflict, Refused, REFUSED_CAPACITY};
use wrist_ble_core::status::PeerState;

const NONE: Vec<Action> = Vec::new();

fn up(at: u64) -> Phone {
    Phone::Discoverable.step(t(at), phone::Event::LinkUp(phone())).0
}

fn actions(p: Phone, at: u64, e: phone::Event) -> (Phone, Vec<Action>) {
    let (p, a) = p.step(t(at), e);
    (p, a.iter().collect())
}

#[test]
fn phone_asks_an_unencrypted_link_to_pair_once() {
    let p = up(0);
    assert_eq!(p.deadline(), Some(t(1_500)));
    let (p, a) = actions(p, 1_500, phone::Event::Tick);
    assert_eq!(a, NONE);
    assert!(p.may_ask_params(t(1_500)));
    let (p, a) = actions(p, 5_000, phone::Event::Tick);
    assert_eq!(a, [Action::RequestSecurity(phone())]);
    assert_eq!(p.deadline(), None);
    assert_eq!(actions(p, 9_000, phone::Event::Tick).1, NONE);
}

#[test]
fn phone_encrypted_in_time_is_never_asked() {
    let (p, _) = actions(up(0), 800, phone::Event::Encryption(phone(), true));
    assert!(p.may_ask_params(t(800)));
    assert_eq!(actions(p, 6_000, phone::Event::Tick).1, NONE);
}

#[test]
fn phone_parameters_wait_out_a_pairing() {
    let (p, _) = actions(up(0), 500, phone::Event::Pairing(phone(), PairingStep::Numeric(1)));
    assert!(!p.may_ask_params(t(2_000)));
    let (p, _) = actions(p, 3_000, phone::Event::Pairing(phone(), PairingStep::Failed));
    assert!(p.may_ask_params(t(3_000)));
}

#[test]
fn phone_numeric_comparison_is_rejected_at_its_deadline() {
    let (p, _) = actions(up(0), 1_000, phone::Event::Pairing(phone(), PairingStep::Numeric(42)));
    assert_eq!(p.deadline(), Some(t(61_000)));
    let (p, a) = actions(p, 61_000, phone::Event::Tick);
    assert_eq!(a, [Action::ConfirmNumeric(phone(), false)]);
    assert_eq!(actions(p, 61_500, phone::Event::Answer(true)).1, NONE);
}

#[test]
fn phone_answer_is_passed_on_once() {
    let (p, _) = actions(up(0), 1_000, phone::Event::Pairing(phone(), PairingStep::Numeric(42)));
    let (p, a) = actions(p, 2_000, phone::Event::Answer(true));
    assert_eq!(a, [Action::ConfirmNumeric(phone(), true)]);
    let (p, a) = actions(p, 2_100, phone::Event::Answer(true));
    assert_eq!(a, NONE);
    let (p, _) = actions(p, 2_500, phone::Event::Encryption(phone(), true));
    assert!(matches!(p, Phone::Up(l) if l.security == Security::Secure));
}

#[test]
fn phone_bond_lost_is_told_in_either_order() {
    let (p, a) = actions(up(0), 100, phone::Event::BondLost(phone()));
    assert_eq!(a, [Action::AllowRebond(phone())]);
    let (_, a) = actions(p, 30_000, phone::Event::Pairing(phone(), PairingStep::Failed));
    assert_eq!(a, [Action::TellBondLost(phone())]);

    let (p, a) = actions(up(0), 100, phone::Event::Pairing(phone(), PairingStep::Failed));
    assert_eq!(a, NONE);
    let (p, a) = actions(p, 30_000, phone::Event::BondLost(phone()));
    assert_eq!(a, [Action::AllowRebond(phone()), Action::TellBondLost(phone())]);
    let (_, a) = actions(p, 31_000, phone::Event::Pairing(phone(), PairingStep::Failed));
    assert_eq!(a, NONE);
}

#[test]
fn phone_settles_after_connecting_and_each_update() {
    let p = up(0);
    assert_eq!(p.settled_at(), Some(t(3_000)));
    let (p, _) = actions(p, 1_900, phone::Event::ParamsChanged(phone()));
    assert_eq!(p.settled_at(), Some(t(4_900)));
}

#[test]
fn phone_ignores_another_link() {
    let p = up(0);
    let (q, _) = actions(p, 100, phone::Event::LinkDown(sensor()));
    assert_eq!(p, q);
    let (q, _) = actions(p, 100, phone::Event::Encryption(sensor(), true));
    assert_eq!(p, q);
    assert_eq!(p.step(t(200), phone::Event::LinkDown(phone())).0, Phone::Discoverable);
}

#[test]
fn chip_resets_within_its_budget_then_faults_and_retries_hourly() {
    let mut c = ChipMachine::OFF.step(t(0), chip::Event::RadioOn).0;
    for i in 0..chip::RESETS_PER_HOUR {
        let (next, a) = c.step(t(i as u64), chip::Event::StartFailed);
        assert_eq!(a, Some(chip::Action::Start));
        c = next;
    }
    let (c, a) = c.step(t(10), chip::Event::StartFailed);
    assert_eq!(a, Some(chip::Action::Hold));
    assert_eq!(c.state, Chip::Fault { cause: FaultCause::StartFailed, retry_at: t(10) + chip::FAULT_RETRY });
    assert_eq!(c.step(t(20), chip::Event::Tick).1, None);
    let (c, a) = c.step(t(10) + chip::FAULT_RETRY, chip::Event::Tick);
    assert_eq!(a, Some(chip::Action::Start));
    let c = c.step(t(3_700_000), chip::Event::Ready(INFO)).0;
    assert_eq!(c.state, Chip::Ready(INFO));
    assert_eq!(c.step(t(3_800_000), chip::Event::BaselineRefused).1, Some(chip::Action::Start));
}

#[test]
fn chip_reset_by_itself_is_started_again() {
    let c = ready_chip(t(0));
    let (c, a) = c.step(t(5), chip::Event::Reset(wrist_ble::ResetCause::Watchdog));
    assert_eq!((c.state, a), (Chip::Starting { since: t(5) }, Some(chip::Action::Start)));
}

#[test]
fn quick_retries_after_a_failed_establishment() {
    let c = ctx(&HR);
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    for n in 1..=per::QUICK_RETRIES {
        p = p.step(&c, t(100), Event::ConnectFailed(FailureKind::NotEstablished));
        assert_eq!(p.stage, Stage::Seeking { since: t(100), quick: n });
    }
    p = p.step(&c, t(200), Event::ConnectFailed(FailureKind::NotEstablished));
    assert!(matches!(p.stage, Stage::Backoff { .. }));
}

#[test]
fn a_link_that_drops_as_it_is_set_up_earns_no_give_up_credit() {
    let c = ctx(&HR);
    let p = Peripheral::default()
        .step(&c, t(0), Event::Want)
        .step(&c, t(100), Event::LinkUp(sensor()))
        .step(&c, t(150), Event::LinkDown(sensor(), Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT));
    assert_eq!(p.stage, Stage::Seeking { since: t(150), quick: 1 });
    let end = t(u64::from(GIVE_UP.0) + 1_000);
    assert_eq!(run(p, &c, t(150), end).stage, Stage::Stopped(StopReason::NotFound));
}

#[test]
fn backoff_climbs_and_is_capped_on_battery() {
    let c = ctx(&HR);
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    let mut now = 0;
    let mut waits = Vec::new();
    for _ in 0..9 {
        p = p.step(&c, t(now), Event::ConnectFailed(FailureKind::Timeout));
        let Stage::Backoff { until } = p.stage else { panic!("{p:?}") };
        waits.push(until.since(t(now)));
        now = until.0;
        p = p.step(&c, t(now), Event::Tick);
    }
    for (w, base) in waits.iter().zip(per::BACKOFF) {
        assert!(*w >= base && w.0 <= base.0 + base.0 / 4, "{w:?} for {base:?}");
    }
    assert!(waits[6..].iter().all(|&w| w == per::BATTERY_BACKOFF_CAP));
}

#[test]
fn backoff_repeats_its_last_step_on_a_cable() {
    let c = wrist_ble_core::peripheral::Ctx { power: Power::Cable, ..ctx(&HR) };
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    let last = per::BACKOFF[per::BACKOFF.len() - 1];
    let mut now = t(0);
    for i in 0..9 {
        p = p.step(&c, now, Event::ConnectFailed(FailureKind::Timeout));
        let Stage::Backoff { until } = p.stage else { panic!("{p:?}") };
        let wait = until.since(now);
        assert!(i < 6 || (wait >= last && wait.0 <= last.0 * 5 / 4), "{wait:?}");
        now = until;
        p = p.step(&c, now, Event::Tick);
    }
}

#[test]
fn a_flapping_strap_delivering_most_readings_is_never_stopped() {
    let c = ctx(&HR);
    let mut p = live(&c, t(0));
    let mut now = 0u64;
    let mut unstable = false;
    while now < 15 * 60_000 {
        now += 1_000;
        let second = now / 1_000;
        if second.is_multiple_of(40) {
            p = p.step(&c, t(now), Event::LinkDown(sensor(), Status::CONN_TIMEOUT));
            for e in [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)] {
                p = p.step(&c, t(now + 7_000), e);
            }
            now += 7_000;
        } else if !second.is_multiple_of(4) {
            p = run(p, &c, t(now - 1_000), t(now));
            p = p.step(&c, t(now), bpm(70));
        }
        assert!(!matches!(p.stage, Stage::Stopped(_)), "stopped at {now}");
        unstable |= matches!(peer_state(&with_peer(ready(t(0)), &HR, p), t(now)), PeerState::Unstable { .. });
    }
    assert!(unstable);
}

#[test]
fn coverage_below_ninety_percent_is_unstable() {
    let c = ctx(&HR);
    let mut p = live(&c, t(0));
    for s in 1..=60u64 {
        if s % 10 >= 2 {
            p = p.step(&c, secs(s), bpm(70));
        }
    }
    assert!(matches!(peer_state(&with_peer(ready(t(0)), &HR, p), t(60_200)), PeerState::Unstable { current: true, .. }));
    let mut p = live(&c, t(0));
    for s in 1..=60u64 {
        p = p.step(&c, secs(s), bpm(70));
    }
    assert!(matches!(peer_state(&with_peer(ready(t(0)), &HR, p), t(60_200)), PeerState::Live { current: true, .. }));
}

#[test]
fn a_service_that_appears_late_goes_live() {
    let c = ctx(&HR);
    let mut p = Peripheral::default().step(&c, t(0), Event::Want);
    p = p.step(&c, t(100), Event::LinkUp(sensor())).step(&c, t(200), Event::Discovered(false));
    assert_eq!(p.stage, Stage::Dropping { link: sensor(), then: Then::Rediscover });
    p = p.step(&c, t(300), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    assert_eq!(p.stage, Stage::Waiting { until: t(300) + per::REDISCOVER_AFTER });
    p = run(p, &c, t(300), t(10_300));
    assert!(matches!(p.stage, Stage::Seeking { .. }));
    for e in [Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)] {
        p = p.step(&c, t(11_000), e);
    }
    assert!(matches!(p.stage, Stage::Live { .. }));
}

#[test]
fn a_silent_strap_is_dropped_and_looked_for_again() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(72));
    let p = run(p, &c, t(1_000), t(31_000));
    assert_eq!(p.stage, Stage::Dropping { link: sensor(), then: Then::Reconnect });
    let p = p.step(&c, t(31_100), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    assert!(matches!(p.stage, Stage::Seeking { .. }));
    assert_eq!(p.lost_since, Some(t(31_000)));
}

#[test]
fn an_idle_sensor_is_kept_while_subscribed() {
    let c = ctx(&CSC);
    let p = live(&c, t(0)).step(&c, t(1_000), bpm(80));
    let p = run(p, &c, t(1_000), t(30 * 60_000));
    assert!(matches!(p.stage, Stage::Live { .. }));
}

#[test]
fn pairing_failure_stops_with_its_reason() {
    let c = wrist_ble_core::peripheral::Ctx { profile: &ENCRYPTED, ..ctx(&HR) };
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(100), Event::LinkUp(sensor()));
    assert!(matches!(p.stage, Stage::Linked { phase: per::Phase::Securing, .. }));
    let p = p.step(&c, t(200), Event::Secured(false));
    assert_eq!(p.stage, Stage::Dropping { link: sensor(), then: Then::Stop(StopReason::PairingFailed) });
    let p = p.step(&c, t(300), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    assert_eq!(p.stage, Stage::Stopped(StopReason::PairingFailed));
    assert!(matches!(p.step(&c, t(400), Event::Retry).stage, Stage::Seeking { .. }));
}

static ENCRYPTED: wrist_ble_core::profile::Profile =
    wrist_ble_core::profile::Profile { security: wrist_ble_core::profile::SecurityNeed::Encrypted, ..HR };

#[test]
fn radio_busy_resumes_when_the_conflict_ends() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(100), Event::Busy(Conflict::Transfer));
    assert_eq!(p.stage, Stage::Dropping { link: sensor(), then: Then::Stop(StopReason::RadioBusy(Conflict::Transfer)) });
    let p = p.step(&c, t(200), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST));
    let p = run(p, &c, t(200), t(20 * 60_000));
    assert_eq!(p.stage, Stage::Stopped(StopReason::RadioBusy(Conflict::Transfer)));
    let p = p.step(&c, t(20 * 60_000), Event::Free);
    assert_eq!(p.stage, Stage::Seeking { since: t(20 * 60_000), quick: 0 });
    assert_eq!(p.heard_at, t(20 * 60_000));
}

#[test]
fn unwanted_drops_its_link_first() {
    let c = ctx(&HR);
    let p = live(&c, t(0)).step(&c, t(100), Event::Unwant);
    assert_eq!(p.stage, Stage::Dropping { link: sensor(), then: Then::Off });
    assert_eq!(p.step(&c, t(200), Event::LinkDown(sensor(), Status::CONN_TERMINATED_BY_LOCAL_HOST)).stage, Stage::Off);
    let p = Peripheral::default().step(&c, t(0), Event::Want).step(&c, t(10), Event::Unwant);
    assert_eq!(p.stage, Stage::Off);
}

#[test]
fn slow_search_ends_with_the_workout() {
    let c = wrist_ble_core::peripheral::Ctx { workout: true, ..ctx(&HR) };
    let p = Peripheral::default().step(&c, t(0), Event::Want);
    let at = t(u64::from(GIVE_UP.0) + 60_000);
    let p = run(p, &c, t(0), at);
    assert!(p.slow_until.is_some());
    let done = wrist_ble_core::peripheral::Ctx { workout: false, ..c };
    assert_eq!(p.step(&done, at, Event::Tick).stage, Stage::Stopped(StopReason::NotFound));
    let end = p.slow_until.unwrap();
    assert_eq!(run(p, &c, at, end).stage, Stage::Stopped(StopReason::NotFound));
}

/// Refuses advertising while initiating, and nothing else.
struct NoAdvertiseWhileInitiating;

impl Schedule for NoAdvertiseWhileInitiating {
    fn admit(&self, current: &Activities, change: &Change) -> Result<Admitted, SchedKind> {
        match change {
            Change::Advertise(Some(_)) if current.initiating.is_some() => Err(SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating)),
            _ => Ok(Admitted::AS_ASKED),
        }
    }
}

const CONNECT: ConnParams = ConnParams { interval_min: Duration::from_u16(40), interval_max: Duration::from_u16(40), latency: 0, timeout: Duration::from_u16(400), ce_len: Duration::from_u16(2) };

#[test]
fn a_transition_is_checked_at_every_step() {
    let start = Activities { advertising: Some(ADV_995), ..Activities::IDLE };
    let s = &NoAdvertiseWhileInitiating;
    let good = [Change::Advertise(None), Change::Initiate(Some(CONNECT))];
    assert!(start.walk(s, &good).is_ok());
    let bad = [Change::Advertise(None), Change::Initiate(Some(CONNECT)), Change::Advertise(Some(ADV_995))];
    assert_eq!(start.walk(s, &bad), Err((2, SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating))));
}

#[test]
fn a_connection_takes_its_admitted_slot_and_rescales_the_others() {
    let slot = wrist_ble::CentralSlot { interval: Duration::from_u16(400), latency: 0, ce_len: Duration::from_u16(2) };
    let a = Activities::IDLE.with(&Change::Initiate(Some(CONNECT)), &Admitted::AS_ASKED);
    let a = a.with(&Change::Connected, &Admitted { slot: Some(slot), latency_scale: 1 });
    assert_eq!((a.initiating, a.centrals()), (None, 1));
    let fast = wrist_ble::CentralSlot { interval: Duration::from_u16(200), latency: 0, ce_len: Duration::from_u16(2) };
    let a = a.with(&Change::Connected, &Admitted { slot: Some(fast), latency_scale: 2 });
    assert_eq!(a.central[0], Some(slot));
    let busy = wrist_ble::CentralSlot { latency: 1, ..slot };
    let a = Activities { central: [Some(busy), None, None, None, None, None, None, None], ..Activities::IDLE };
    let a = a.with(&Change::Connected, &Admitted { slot: Some(fast), latency_scale: 2 });
    assert_eq!(a.central[..2], [Some(wrist_ble::CentralSlot { latency: 2, ..slot }), Some(fast)]);
}

#[test]
fn a_refused_configuration_is_remembered_until_cleared() {
    let start = Activities::IDLE;
    let a = fingerprint(&start, &Change::Advertise(Some(ADV_995)));
    let b = fingerprint(&start, &Change::Advertise(None));
    assert_ne!(a, b);
    let mut r = Refused::EMPTY;
    assert!(r.insert(a));
    assert!(r.contains(a) && !r.contains(b));
    for i in 1..REFUSED_CAPACITY {
        assert!(r.insert(wrist_ble_core::plan::Fingerprint(i as u64)));
    }
    assert!(r.insert(a));
    assert!(!r.insert(b));
    r.clear();
    assert!(!r.contains(a));
}
