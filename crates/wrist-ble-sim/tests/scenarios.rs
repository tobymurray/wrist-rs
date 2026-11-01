//! The known failures of the Bluetooth subsystem, each replayed with the timings and peers that
//! showed it. Those needing the central role wait for it to be planned.

use wrist_ble::{BluetoothUuid, Change, Instant, Millis, Op, ParamRange, ResetCause, SchedKind, Duration};
use wrist_ble_core::engine::Tell;
use wrist_ble_core::goals::{Goals, Power, Want};
use wrist_ble_core::profile::{Cadence, Contact, Kind, Profile, Reading, SecurityNeed, Silence};
use wrist_ble_core::status::{PeerState, PhoneStatus};
use wrist_ble_sim::fake::Note;
use wrist_ble_sim::peers::{Keys, Os, Phone, Sensor};
use wrist_ble_sim::sim::{on, Sim};

fn hr(data: &[u8]) -> Option<Reading> {
    let contact = if data.first().is_some_and(|f| f & 0x06 == 0x04) { Contact::Lost } else { Contact::Detected };
    data.get(1).map(|&v| Reading { value: u16::from(v), contact })
}

static HR: Profile = Profile {
    kind: Kind::HeartRate,
    service: BluetoothUuid::from_u16(0x180D),
    subscribe: &[BluetoothUuid::from_u16(0x2A37)],
    security: SecurityNeed::None,
    cadence: Cadence::Every(Millis::secs(1)),
    silence: Silence::Fault,
    unit: "bpm",
    quiet: Millis::secs(5),
    stale: Millis::secs(30),
    parse: hr,
};

static HR_ENCRYPTED: Profile = Profile { security: SecurityNeed::Encrypted, ..HR };

fn with_strap(power: Power, profile: &'static Profile) -> Goals {
    Goals { peripherals: [Some(Want { profile, peer: Sensor::strap(1).addr, priority: 0 }), None, None], ..on(power) }
}

fn submitted(s: &Sim, pick: impl Fn(&Op) -> bool) -> Vec<Instant> {
    s.fake.notes.iter().filter_map(|&(at, n)| match n {
        Note::Submitted { op, .. } if pick(&op) => Some(at),
        _ => None,
    }).collect()
}

fn strap_state(s: &Sim) -> PeerState {
    s.status().peers[0].expect("a strap is wanted").state
}

fn live(s: &Sim) -> bool {
    matches!(strap_state(s), PeerState::Live { .. } | PeerState::Unstable { .. })
}

// --- The phone's side ---

#[test]
fn bond_lost_any_order() {
    for first in [true, false] {
        for seed in 0..10 {
            let mut s = Sim::started(seed, on(Power::Battery));
            s.fake.phone_arrives(Phone { keys: Keys::PhoneLost, bond_lost_first: first, ..Phone::new(Os::Android, 1) });
            s.run_for(Millis::mins(3));
            let told: Vec<_> = s.tells.iter().filter(|(_, t)| matches!(t, Tell::BondLost(_))).collect();
            assert_eq!(told.len(), 1, "seed {seed}, bond lost first: {first}");
            assert_eq!(submitted(&s, |op| matches!(op, Op::AllowRebond(_))).len(), 1);
        }
    }
}

/// A bonded Mac's link ended with a MIC failure when the interval was asked for at connect, beside
/// the Service Changed indication; asked once encrypted, never.
#[test]
fn macos_service_changed() {
    for seed in 0..30 {
        let mut s = Sim::started(seed, on(Power::Battery));
        for _ in 0..5 {
            s.fake.phone_arrives(Phone::new(Os::Mac, 1));
            s.run_for(Millis::secs(4));
            assert_eq!(s.status().phone, PhoneStatus::Connected, "seed {seed}");
            s.fake.phone_leaves(0);
            s.run_for(Millis::secs(2));
        }
        assert_eq!(s.core.counters.mic_failures, 0);
        assert_eq!(submitted(&s, |op| matches!(op, Op::UpdateParams(..))).len(), 5, "seed {seed}");
    }
}

#[test]
fn back_to_cable_after_battery() {
    let mut s = Sim::started(3, on(Power::Cable));
    let ms = |s: &Sim| s.fake.advertising().map(|a| a.interval_min.as_u16());
    assert_eq!(ms(&s), Some(160));
    for (i, power) in [Power::Battery, Power::Cable, Power::Battery, Power::Cable].into_iter().enumerate() {
        if i == 1 {
            s.fake.phone_arrives(Phone::new(Os::Linux, 1));
            s.run_for(Millis::secs(5));
        }
        s.power(power);
        if i == 1 {
            s.run_for(Millis::secs(30));
            s.fake.phone_leaves(0);
        }
        s.run_for(Millis::secs(15));
        assert_eq!(ms(&s), Some(if power == Power::Cable { 160 } else { 1592 }), "after flip {i}");
    }
}

#[test]
fn update_complete_with_error_on_the_phone_link() {
    let mut s = Sim::started(4, on(Power::Battery));
    s.fake.phone_arrives(Phone { update_fails: true, ..Phone::new(Os::Linux, 1) });
    s.run_for(Millis::mins(5));
    assert_eq!(s.core.counters.update_failures, 1);
    assert_eq!(submitted(&s, |op| matches!(op, Op::UpdateParams(..))).len(), 1);
    assert_eq!(s.status().phone, PhoneStatus::Connected);
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(1));
    assert!(s.fake.advertising().is_some());
}

/// A refusal the model did not predict is tried once in that configuration, and the next option
/// takes its place.
#[test]
fn unpredicted_refusal_is_quarantined() {
    let mut s = Sim::new(5);
    s.fake.diverge(Box::new(|_, c| matches!(c, Change::Advertise(Some(p)) if p.interval_min.as_u16() == 1592).then_some(SchedKind::LengthFailed)));
    s.goals(on(Power::Battery));
    s.run_for(Millis::mins(10));
    assert_eq!(s.fake.advertising().map(|a| a.interval_min.as_u16()), Some(3992));
    assert!(s.line().contains("phone=discoverable(2495ms)"), "{}", s.line());
    assert_eq!(s.core.counters.divergences, 1);
    assert!(s.tells.iter().any(|(_, t)| *t == Tell::Divergence { kind: wrist_ble::OpKind::Advertise, native: 0x86 }));
    assert_eq!(submitted(&s, |op| matches!(op, Op::Advertise(_))).len(), 2);
}

#[test]
fn everything_refused_ends_in_a_fault_started_once_an_hour() {
    let mut s = Sim::new(6);
    s.fake.diverge(Box::new(|_, c| matches!(c, Change::Advertise(Some(_))).then_some(SchedKind::LengthFailed)));
    s.goals(on(Power::Battery));
    s.run_for(Millis::mins(5));
    assert!(s.line().contains("phone=fault(baseline-refused)"), "{}", s.line());
    assert!(!s.fake.ready());
    let attempts = submitted(&s, |op| matches!(op, Op::Advertise(_))).len();
    assert_eq!(attempts, 2 * (wrist_ble_core::chip::RESETS_PER_HOUR + 1));
    let resets = s.core.counters.resets;
    for hour in 1..=3 {
        s.run_for(Millis::mins(60));
        assert!(s.line().contains("phone=fault(baseline-refused)"), "{}", s.line());
        assert_eq!(submitted(&s, |op| matches!(op, Op::Advertise(_))).len(), attempts + 2 * hour);
        assert_eq!(s.core.counters.resets, resets);
    }
}

#[test]
fn a_chip_that_keeps_resetting_ends_in_a_fault() {
    let mut s = Sim::started(7, on(Power::Battery));
    for _ in 0..=wrist_ble_core::chip::RESETS_PER_HOUR {
        s.fake.reset(ResetCause::Watchdog);
        s.run_for(Millis::mins(5));
    }
    assert!(s.line().contains("phone=fault(kept-resetting)"), "{}", s.line());
}

/// The ready event after a reset was lost: the chip is started again at once, not left silent
/// until the start's deadline.
#[test]
fn ready_lost_after_a_reset() {
    for seed in 0..20 {
        let mut s = Sim::started(seed, on(Power::Battery));
        s.fake.reset(ResetCause::Crash);
        s.run_for(Millis(0));
        s.fake.lose_events(1);
        s.run_for(Millis::secs(2));
        assert!(s.fake.advertising().is_some(), "seed {seed}: {}", s.line());
        s.run_for(Millis::mins(1));
    }
}

// --- With a strap: the central role ---

#[test]
#[ignore = "needs the central role on the planner"]
fn l2cap_answer_rejected_on_air() {
    let range = ParamRange { interval_min: Duration::from_u16(144), interval_max: Duration::from_u16(176), latency: 0, timeout: Duration::from_u16(600) };
    let mut s = Sim::new(11);
    s.fake.sensor_on(Sensor { asks: Some((Millis::secs(2), range)), ..Sensor::strap(1) });
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::secs(30));
    assert!(live(&s), "{}", s.line());
    assert_eq!(s.core.counters.refused_peer_requests, 1);
    assert_eq!(submitted(&s, |op| matches!(op, Op::AnswerParamRequest(_, false))).len(), 1);
}

#[test]
#[ignore = "needs the central role on the planner"]
fn phone_leaves_beside_live_strap() {
    for power in [Power::Cable, Power::Battery] {
        let mut s = Sim::new(12);
        s.fake.sensor_on(Sensor::strap(1));
        s.goals(with_strap(power, &HR));
        s.fake.phone_arrives(Phone::new(Os::Linux, 1));
        s.run_for(Millis::secs(30));
        assert!(live(&s), "{}", s.line());
        s.fake.phone_leaves(0);
        s.run_for(Millis::secs(30));
        assert!(s.fake.advertising().is_some());
        assert!(live(&s) || matches!(strap_state(&s), PeerState::Stopped(_)), "{}", s.line());
    }
}

#[test]
#[ignore = "needs the central role on the planner"]
fn strap_on_battery_discovery() {
    for seed in 0..10 {
        let mut s = Sim::new(seed);
        s.fake.sensor_on(Sensor::strap(1));
        s.goals(with_strap(Power::Battery, &HR));
        s.run_for(Millis::secs(5));
        assert!(live(&s), "seed {seed}: {}", s.line());
        assert_eq!(s.core.counters.timed_out, 0);
    }
}

#[test]
#[ignore = "needs the central role on the planner"]
fn hangup_loop_gives_up() {
    let mut s = Sim::new(14);
    s.fake.sensor_on(Sensor { hangs_up_after: Some(Millis(500)), ..Sensor::strap(1) });
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::mins(4));
    assert!(!matches!(strap_state(&s), PeerState::Stopped(_)), "{}", s.line());
    s.run_for(Millis::mins(2));
    assert_eq!(strap_state(&s), PeerState::Stopped(wrist_ble_core::peripheral::StopReason::KeptDropping));
}

#[test]
#[ignore = "needs the central role on the planner"]
fn service_appears_late() {
    let mut s = Sim::new(15);
    s.fake.sensor_on(Sensor { service_after: Millis::secs(25), ..Sensor::strap(1) });
    s.goals(with_strap(Power::Cable, &HR));
    s.run_for(Millis::secs(5));
    assert!(matches!(strap_state(&s), PeerState::Rechecking { .. }), "{}", s.line());
    s.run_for(Millis::secs(40));
    assert!(live(&s), "{}", s.line());
}

#[test]
#[ignore = "needs the central role on the planner"]
fn power_flip_with_live_strap() {
    let mut s = Sim::new(17);
    s.fake.sensor_on(Sensor::strap(1));
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::secs(30));
    for i in 0..10 {
        s.power(if i % 2 == 0 { Power::Cable } else { Power::Battery });
        s.run_for(Millis::secs(20));
        assert!(live(&s), "{}", s.line());
    }
    assert_eq!(s.core.counters.refused, 0);
}

#[test]
#[ignore = "needs the central role on the planner"]
fn advertise_while_initiating() {
    let mut s = Sim::new(18);
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::mins(3));
    assert_ne!(submitted(&s, |op| matches!(op, Op::Connect(..))).len(), 0);
    assert_eq!(s.core.counters.refused, 0);
}

#[test]
#[ignore = "needs the central role on the planner"]
fn flapping_strap_75pct() {
    let mut s = Sim::new(19);
    s.fake.sensor_on(Sensor { delivers_percent: 75, hangs_up_after: Some(Millis::secs(40)), ..Sensor::strap(1) });
    s.goals(with_strap(Power::Battery, &HR));
    let mut unstable = false;
    for _ in 0..60 {
        s.run_for(Millis::secs(10));
        assert!(!matches!(strap_state(&s), PeerState::Stopped(_)), "{}", s.line());
        unstable |= matches!(strap_state(&s), PeerState::Unstable { .. });
    }
    assert!(unstable);
}

#[test]
#[ignore = "needs the central role on the planner"]
fn first_connect_3e() {
    for seed in 0..20 {
        let mut s = Sim::new(seed);
        s.fake.sensor_on(Sensor { not_established_permille: 500, ..Sensor::strap(1) });
        s.goals(with_strap(Power::Battery, &HR));
        s.run_for(Millis::secs(8));
        assert!(live(&s), "seed {seed}: {}", s.line());
    }
}

#[test]
#[ignore = "needs the central role on the planner, and the chosen strap kept across resets"]
fn reset_with_strap_chosen() {
    let mut s = Sim::new(21);
    s.fake.sensor_on(Sensor::strap(1));
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::secs(30));
    s.fake.reset(ResetCause::Crash);
    s.run_for(Millis(500));
    assert!(matches!(strap_state(&s), PeerState::Reconnecting { .. } | PeerState::Finding { .. }), "{}", s.line());
    s.run_for(Millis::secs(30));
    assert!(live(&s), "{}", s.line());
}

#[test]
#[ignore = "needs the central role on the planner"]
fn central_start_in_phone_settle() {
    for seed in 0..20 {
        let mut s = Sim::new(seed);
        s.fake.sensor_on(Sensor::strap(1));
        s.fake.phone_arrives(Phone::new(Os::Android, 1));
        s.goals(with_strap(Power::Cable, &HR));
        s.run_for(Millis::secs(30));
        assert_eq!(s.core.counters.instant_passed, 0);
        assert!(live(&s), "seed {seed}: {}", s.line());
    }
}

#[test]
#[ignore = "needs the central role on the planner"]
fn mic_on_sensor_encrypt() {
    let range = ParamRange { interval_min: Duration::from_u16(400), interval_max: Duration::from_u16(400), latency: 0, timeout: Duration::from_u16(600) };
    let mut s = Sim::new(23);
    s.fake.sensor_on(Sensor { asks: Some((Millis(100), range)), ..Sensor::strap(1) });
    s.goals(with_strap(Power::Battery, &HR_ENCRYPTED));
    s.run_for(Millis::secs(30));
    assert_eq!(s.core.counters.mic_failures, 0);
    assert!(live(&s), "{}", s.line());
}

#[test]
#[ignore = "needs the central role on the planner"]
fn fresh_link_length_failed() {
    let mut s = Sim::new(24);
    s.fake.diverge(Box::new(|a, c| (a.centrals() > 0 && matches!(c, Change::Advertise(Some(_)))).then_some(SchedKind::LengthFailed)));
    s.fake.sensor_on(Sensor::strap(1));
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    s.goals(with_strap(Power::Cable, &HR));
    s.run_for(Millis::secs(30));
    assert!(live(&s), "{}", s.line());
    s.fake.phone_leaves(0);
    s.run_for(Millis::mins(5));
    assert!(s.core.counters.divergences <= 2, "{:?}", s.core.counters);
    assert!(s.fake.advertising().is_some() || matches!(strap_state(&s), PeerState::Stopped(_)), "{}", s.line());
}

#[test]
#[ignore = "needs the central role on the planner"]
fn update_complete_with_error() {
    let mut s = Sim::new(25);
    s.fake.sensor_on(Sensor { refuses_updates: true, ..Sensor::strap(1) });
    s.goals(with_strap(Power::Battery, &HR));
    s.run_for(Millis::mins(2));
    assert!(s.core.counters.update_failures >= 1);
    assert!(s.fake.advertising().is_some(), "{}", s.line());
}
