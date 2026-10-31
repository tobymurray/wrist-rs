//! The fake chip against the captures and the scheduler it shares.

use wrist_ble::{
    Activities, AdvParams, Cccd, Change, CharRef, ConnHandle, ConnParams, DiscoverWhat, Duration, FailureKind, Instant, LeConnRole, Millis, Op, Outcome,
    PairingStep, ParamRange, RadioEvent, Radio, ResetCause, ScanParams, SchedKind, ServerTable, Status,
};
use wrist_ble_sim::fake::{Fake, Note, CONSOLE};
use wrist_ble_sim::now;
use wrist_bluenrg2::Scheduler;
use wrist_ble_sim::peers::{Keys, Os, Phone, Sensor};

const ADV_100: AdvParams = AdvParams { interval_min: Duration::from_u16(160), interval_max: Duration::from_u16(160), connectable: true };
const ADV_995: AdvParams = AdvParams { interval_min: Duration::from_u16(1592), interval_max: Duration::from_u16(1592), connectable: true };

fn conn(units: u16) -> ConnParams {
    ConnParams { interval_min: Duration::from_u16(units), interval_max: Duration::from_u16(units), latency: 0, timeout: Duration::from_u16(400), ce_len: Duration::from_u16(4) }
}

fn ready(seed: u64) -> Fake {
    let mut f = Fake::new(seed);
    f.start();
    assert!(matches!(f.advance(Instant(5_000)), Some((_, RadioEvent::Ready(_)))));
    f
}

/// Every event up to `until`, with its time.
fn run(f: &mut Fake, until: u64) -> Vec<(Instant, RadioEvent)> {
    std::iter::from_fn(|| f.advance(Instant(until))).collect()
}

fn kinds(events: &[(Instant, RadioEvent)]) -> Vec<&'static str> {
    events
        .iter()
        .map(|(_, e)| match e {
            RadioEvent::LinkUp { .. } => "connected",
            RadioEvent::DataLength { .. } => "data-length",
            RadioEvent::Pairing(p) if p.step == PairingStep::Complete => "paired",
            RadioEvent::Pairing(_) => "pairing",
            RadioEvent::Encryption { on: true, .. } => "encrypted",
            RadioEvent::Encryption { on: false, .. } => "not-encrypted",
            RadioEvent::Mtu(..) => "mtu",
            RadioEvent::ServerSubscribe { .. } => "subscribed",
            RadioEvent::LinkDown { .. } => "disconnected",
            RadioEvent::Done(..) => "done",
            RadioEvent::Params { .. } => "params",
            RadioEvent::BondLost(_) => "bond-lost",
            _ => "other",
        })
        .collect()
}

/// Events up to and including the one `stop` picks, which it returns.
fn until<T>(f: &mut Fake, mut stop: impl FnMut(&RadioEvent) -> Option<T>) -> (Instant, T) {
    loop {
        let (at, e) = f.advance(Instant(u64::MAX)).expect("it happened");
        if let Some(t) = stop(&e) {
            return (at, t);
        }
    }
}

fn link_up(events: &[(Instant, RadioEvent)]) -> (Instant, ConnHandle) {
    events.iter().find_map(|(at, e)| if let RadioEvent::LinkUp { link, .. } = e { Some((*at, *link)) } else { None }).unwrap()
}

#[test]
fn a_bonded_phone_reconnects_as_btmon_saw_it_from_the_host() {
    for seed in 0..20 {
        let mut f = ready(seed);
        f.submit(Op::Advertise(Some(ADV_100))).unwrap();
        f.phone_arrives(Phone::new(Os::Linux, 1));
        let events = run(&mut f, 5_000);
        let shown: Vec<_> = kinds(&events).into_iter().filter(|k| *k != "done").collect();
        assert_eq!(shown, ["connected", "data-length", "paired", "encrypted", "mtu", "subscribed"], "seed {seed}");
        assert_eq!(f.advertising(), None);
        let (up, _) = link_up(&events);
        let encrypted = events.iter().find(|(_, e)| matches!(e, RadioEvent::Encryption { .. })).unwrap().0;
        let took = encrypted.since(up);
        let (lo, hi) = bluez_encrypts();
        assert!((lo..=hi).contains(&took), "{took:?}");
    }
}

/// The shortest and longest time to encryption the capture shows.
fn bluez_encrypts() -> (Millis, Millis) {
    let fixture = include_str!("../fixtures/bluez-reconnects.txt");
    let ms: Vec<u32> = fixture.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split(' ').nth(1)?.parse().ok()).collect();
    assert!(ms.len() > 40);
    (Millis(*ms.iter().min().unwrap()), Millis(*ms.iter().max().unwrap()))
}

#[test]
fn advertising_is_refused_while_initiating() {
    let mut f = ready(1);
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(40))).unwrap();
    let r = f.submit(Op::Advertise(Some(ADV_995)));
    assert_eq!(r.map_err(|e| (e.kind, e.native)), Err((FailureKind::ModeExclusion, 0x0C)));
}

#[test]
fn a_cancelled_connection_ends_the_initiation() {
    let mut f = ready(1);
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(40))).unwrap();
    f.submit(Op::CancelConnect).unwrap();
    assert!(f.submit(Op::Advertise(Some(ADV_995))).is_ok());
}

#[test]
fn a_refusal_carries_the_chips_code() {
    let mut f = ready(1);
    f.sensor_on(Sensor::strap(1));
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(28))).unwrap();
    run(&mut f, 2_000);
    let r = f.submit(Op::Advertise(Some(AdvParams { interval_min: Duration::from_u16(1636), interval_max: Duration::from_u16(1636), connectable: true })));
    assert_eq!(r.map_err(|e| (e.kind, e.native)), Err((FailureKind::Schedule(SchedKind::IntervalTooLarge), 0x85)));
}

const SCAN_30: ScanParams = ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(32), active: false, duration: Millis::secs(30) };

/// The events after the first `ScanDone`.
fn after_scan_done(events: &[(Instant, RadioEvent)]) -> Option<&[(Instant, RadioEvent)]> {
    let i = events.iter().position(|(_, e)| *e == RadioEvent::ScanDone)?;
    Some(&events[i + 1..])
}

#[test]
fn a_discovery_ends_by_itself_at_10_2_s() {
    let mut f = ready(1);
    f.sensor_on(Sensor::strap(1));
    let t0 = f.now();
    f.submit(Op::Scan(Some(SCAN_30))).unwrap();
    let events = run(&mut f, 40_000);
    let done = events.iter().find(|(_, e)| *e == RadioEvent::ScanDone).unwrap();
    assert_eq!(done.0.since(t0), Millis(10_200));
    assert!(!after_scan_done(&events).unwrap().iter().any(|(_, e)| matches!(e, RadioEvent::Report(_))));
    assert_eq!(f.activities().scanning, None);
}

#[test]
fn a_stopped_discovery_reports_nothing_more() {
    let mut f = ready(1);
    f.sensor_on(Sensor::strap(1));
    f.submit(Op::Scan(Some(SCAN_30))).unwrap();
    assert!(run(&mut f, 2_000).iter().any(|(_, e)| matches!(e, RadioEvent::Report(_))));
    f.submit(Op::Scan(None)).unwrap();
    let events = run(&mut f, 40_000);
    assert!(!after_scan_done(&events).unwrap().iter().any(|(_, e)| matches!(e, RadioEvent::Report(_))));
    assert_eq!(f.activities().scanning, None);
}

#[test]
fn an_accepted_answer_to_a_peers_request_goes_on_air_as_a_refusal() {
    let mut f = ready(1);
    let range = ParamRange { interval_min: Duration::from_u16(144), interval_max: Duration::from_u16(176), latency: 0, timeout: Duration::from_u16(600) };
    f.sensor_on(Sensor { asks: Some((Millis::secs(1), range)), ..Sensor::strap(1) });
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(40))).unwrap();
    let events = run(&mut f, 3_000);
    let link = events.iter().find_map(|(_, e)| if let RadioEvent::PeerParamRequest { link, .. } = e { Some(*link) } else { None }).unwrap();
    f.submit(Op::AnswerParamRequest(link, true)).unwrap();
    let after = run(&mut f, 10_000);
    assert!(!after.iter().any(|(_, e)| matches!(e, RadioEvent::Params { .. })));
    assert!(f.notes.iter().any(|(_, n)| *n == Note::RejectedOnAir(link)));
}

#[test]
fn a_second_link_to_a_connected_peer_fails_to_establish() {
    let mut f = ready(1);
    let s = Sensor::strap(1);
    f.sensor_on(s);
    f.submit(Op::Connect(s.addr, conn(40))).unwrap();
    run(&mut f, 2_000);
    f.submit(Op::Connect(s.addr, conn(40))).unwrap();
    let events = run(&mut f, 4_000);
    let (up, link) = link_up(&events);
    let down = events.iter().find(|(_, e)| matches!(e, RadioEvent::LinkDown { link: l, .. } if *l == link)).unwrap();
    assert_eq!(down.1, RadioEvent::LinkDown { link, reason: Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT });
    assert_eq!(down.0.since(up), Millis(6 * 50));
}

#[test]
fn some_first_connections_fail_to_establish() {
    let mut failed = 0;
    for seed in 0..300 {
        let mut f = ready(seed);
        let s = Sensor { not_established_permille: 200, ..Sensor::strap(1) };
        f.sensor_on(s);
        f.submit(Op::Connect(s.addr, conn(40))).unwrap();
        failed += usize::from(run(&mut f, 3_000).iter().any(|(_, e)| matches!(e, RadioEvent::LinkDown { reason: Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT, .. })));
    }
    assert!((40..80).contains(&failed), "{failed}");
}

#[test]
fn a_procedure_across_encryption_ends_the_link_with_a_mic_failure() {
    let mut f = ready(3);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone { console: false, ..Phone::new(Os::Mac, 1) });
    let events = run(&mut f, 1_000);
    let (_, link) = link_up(&events);
    assert!(kinds(&events).contains(&"encrypted"));
    f.phone_leaves(0);
    run(&mut f, 2_000);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone { console: false, ..Phone::new(Os::Mac, 1) });
    let first = loop {
        let (at, e) = f.advance(Instant(10_000)).unwrap();
        if let RadioEvent::LinkUp { link, .. } = e {
            break (at, link);
        }
    };
    assert_ne!(first.1, link);
    let ask = ConnParams { interval_min: Duration::from_u16(12), interval_max: Duration::from_u16(24), ..conn(12) };
    f.submit(Op::UpdateParams(first.1, ask)).unwrap();
    let events = run(&mut f, 12_000);
    assert!(events.iter().any(|(_, e)| *e == RadioEvent::LinkDown { link: first.1, reason: Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE }), "{events:?}");
    assert!(f.notes.iter().any(|(_, n)| *n == Note::CrossedEncryption(first.1)));
}

#[test]
fn a_phone_that_lost_its_bond_is_reported_in_either_order() {
    for first in [true, false] {
        let mut f = ready(5);
        f.submit(Op::Advertise(Some(ADV_100))).unwrap();
        f.phone_arrives(Phone { keys: Keys::PhoneLost, bond_lost_first: first, ..Phone::new(Os::Android, 2) });
        let order: Vec<_> = kinds(&run(&mut f, 5_000)).into_iter().filter(|k| ["bond-lost", "pairing"].contains(k)).collect();
        assert_eq!(order, if first { ["bond-lost", "pairing"] } else { ["pairing", "bond-lost"] });
    }
}

#[test]
fn a_phone_whose_bond_the_chip_lost_stays_up_unencrypted() {
    let mut f = ready(5);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone { keys: Keys::ChipLost, ..Phone::new(Os::Android, 2) });
    let shown: Vec<_> = kinds(&run(&mut f, 5_000)).into_iter().filter(|k| *k != "done" && *k != "params").collect();
    assert_eq!(shown, ["connected", "data-length", "not-encrypted"]);
    assert_eq!(f.phone_link().map(|(_, enc)| enc), Some(false));
}

#[test]
fn a_new_phone_pairs_by_numeric_comparison() {
    let mut f = ready(6);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone { keys: Keys::None, ..Phone::new(Os::Linux, 3) });
    let events = run(&mut f, 3_000);
    let (_, link) = link_up(&events);
    f.submit(Op::RequestSecurity(link)).unwrap();
    let value = run(&mut f, 4_000).into_iter().find_map(|(_, e)| if let RadioEvent::Pairing(p) = e { if let PairingStep::Numeric(v) = p.step { Some(v) } else { None } } else { None });
    assert!(value.is_some());
    f.submit(Op::ConfirmNumeric(link, true)).unwrap();
    let shown: Vec<_> = kinds(&run(&mut f, 12_000)).into_iter().filter(|k| *k != "done").collect();
    assert_eq!(shown, ["paired", "encrypted", "mtu", "subscribed"]);
}

/// A Pixel re-times its own link at 0.75, 1.1 and 1.9 s.
#[test]
fn an_android_phone_retimes_its_own_link() {
    let mut f = ready(7);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone::new(Os::Android, 4));
    let events = run(&mut f, 4_000);
    let (up, _) = link_up(&events);
    let at: Vec<u64> = events.iter().filter(|(_, e)| matches!(e, RadioEvent::Params { .. })).map(|(t, _)| t.since(up).0.into()).collect();
    assert_eq!(at.len(), 3);
    assert!(at[2] > 1_900 && at[2] < 2_200, "{at:?}");
}

#[test]
fn a_central_start_on_a_pending_update_loses_the_phone() {
    let mut f = ready(8);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone::new(Os::Android, 4));
    let (up, link) = until(&mut f, |e| if let RadioEvent::LinkUp { link, .. } = e { Some(*link) } else { None });
    run(&mut f, (up + Millis(770)).0);
    f.sensor_on(Sensor::strap(1));
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(40))).unwrap();
    let events = run(&mut f, 12_000);
    assert!(events.iter().any(|(_, e)| *e == RadioEvent::LinkDown { link, reason: Status::INSTANT_PASSED }));
}

/// Discovery and subscription take ATT round trips of one or two connection events each, so a
/// slow link discovers slowly.
#[test]
fn discovery_takes_longer_on_a_slower_link() {
    let took = |units: u16| {
        let mut f = ready(9);
        let s = Sensor::strap(1);
        f.sensor_on(s);
        f.submit(Op::Connect(s.addr, conn(units))).unwrap();
        let (up, link) = until(&mut f, |e| if let RadioEvent::LinkUp { link, .. } = e { Some(*link) } else { None });
        let id = f.submit(Op::Discover(link, DiscoverWhat::Service(s.service))).unwrap();
        let (_, service) = until(&mut f, |e| if let RadioEvent::Done(i, Ok(Outcome::Service(Some(sv)))) = e { (*i == id).then_some(*sv) } else { None });
        let id = f.submit(Op::Discover(link, DiscoverWhat::Characteristic(service, s.characteristic))).unwrap();
        let (_, ch) = until(&mut f, |e| if let RadioEvent::Done(i, Ok(Outcome::Characteristic(Some(c)))) = e { (*i == id).then_some(*c) } else { None });
        let id = f.submit(Op::Subscribe(link, ch, Cccd::Notify)).unwrap();
        let (at, ()) = until(&mut f, |e| matches!(e, RadioEvent::Done(i, _) if *i == id).then_some(()));
        at.since(up)
    };
    assert!(took(40) < Millis::secs(2), "{:?}", took(40));
    assert!(took(400) > Millis::secs(8), "{:?}", took(400));
}

#[test]
fn a_service_that_appears_late_is_absent_until_it_does() {
    let mut f = ready(10);
    let s = Sensor { service_after: Millis::secs(30), ..Sensor::strap(1) };
    f.sensor_on(s);
    f.submit(Op::Connect(s.addr, conn(40))).unwrap();
    let (_, link) = link_up(&run(&mut f, 2_000));
    let id = f.submit(Op::Discover(link, DiscoverWhat::Service(s.service))).unwrap();
    assert!(run(&mut f, 5_000).iter().any(|(_, e)| *e == RadioEvent::Done(id, Ok(Outcome::Service(None)))));
    run(&mut f, 31_000);
    let id = f.submit(Op::Discover(link, DiscoverWhat::Service(s.service))).unwrap();
    assert!(run(&mut f, 40_000).iter().any(|(_, e)| matches!(e, RadioEvent::Done(i, Ok(Outcome::Service(Some(_)))) if *i == id)));
}

#[test]
fn a_sensor_that_goes_away_times_its_link_out() {
    let mut f = ready(11);
    let s = Sensor::strap(1);
    let i = f.sensor_on(s);
    f.submit(Op::Connect(s.addr, conn(40))).unwrap();
    let (_, link) = link_up(&run(&mut f, 2_000));
    f.submit(Op::Subscribe(link, CharRef { value: wrist_ble::AttRef(0x12), cccd: Some(wrist_ble::AttRef(0x13)) }, Cccd::Notify)).unwrap();
    let readings = run(&mut f, 12_000).iter().filter(|(_, e)| matches!(e, RadioEvent::Notification { .. })).count();
    assert!((9..=11).contains(&readings), "{readings}");
    let gone = f.now();
    f.sensor_off(i);
    let events = run(&mut f, 30_000);
    let down = events.iter().find(|(_, e)| matches!(e, RadioEvent::LinkDown { .. })).unwrap();
    assert_eq!(down.1, RadioEvent::LinkDown { link, reason: Status::CONN_TIMEOUT });
    assert_eq!(down.0.since(gone), Millis::secs(4));
}

#[test]
fn a_reset_takes_every_link_and_operation_with_it() {
    let mut f = ready(12);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.phone_arrives(Phone::new(Os::Linux, 1));
    run(&mut f, 3_000);
    f.submit(Op::Connect(Sensor::strap(1).addr, conn(40))).unwrap();
    f.reset(ResetCause::Watchdog);
    assert_eq!(run(&mut f, 60_000).iter().map(|(_, e)| *e).collect::<Vec<_>>(), [RadioEvent::ChipReset(ResetCause::Watchdog)]);
    assert!(f.submit(Op::Advertise(Some(ADV_100))).is_err());
    assert_eq!(f.phone_link(), None);
}

#[test]
fn lost_events_are_reported_and_the_snapshot_tells_the_truth() {
    let mut f = ready(13);
    f.submit(Op::Advertise(Some(ADV_100))).unwrap();
    f.lose_events(2);
    f.phone_arrives(Phone::new(Os::Linux, 1));
    let events = run(&mut f, 5_000);
    assert_eq!(kinds(&events)[..1], ["other"]);
    assert_eq!(events[0].1, RadioEvent::EventsLost);
    let snap = f.snapshot();
    assert_eq!(snap.links.iter().flatten().filter(|l| l.role == LeConnRole::Peripheral).count(), 1);
    assert!(!snap.advertising);
}

#[test]
fn every_accepted_operation_ends_by_its_deadline() {
    let mut f = ready(14);
    let s = Sensor::strap(1);
    let start = f.now();
    let id = f.submit(Op::Connect(s.addr, conn(40))).unwrap();
    let events = run(&mut f, 40_000);
    let ended = events.iter().find(|(_, e)| matches!(e, RadioEvent::Done(i, Err(_)) if *i == id)).unwrap();
    assert_eq!(ended.0.since(start), Millis::secs(30));
    assert!(f.submit(Op::Advertise(Some(ADV_995))).is_ok(), "initiating ended with its deadline");
}

#[test]
fn the_fake_is_a_radio() {
    let mut f = Fake::new(15);
    let info = now(Radio::start(&mut f, &ServerTable { services: &[] })).unwrap();
    assert_eq!(info.firmware, 2353);
    assert_eq!(f.capabilities().adv_event_len.0, 14_600);
    let id = now(Radio::submit(&mut f, Op::Advertise(Some(ADV_995)))).unwrap();
    assert_eq!(now(Radio::next(&mut f)), RadioEvent::Done(id, Ok(Outcome::Done)));
    assert!(now(Radio::snapshot(&mut f)).unwrap().advertising);
    let _ = CONSOLE;
}

fn connect_link(f: &mut Fake, s: Sensor, units: u16) -> ConnHandle {
    f.sensor_on(s);
    f.submit(Op::Connect(s.addr, conn(units))).unwrap();
    until(f, |e| if let RadioEvent::LinkUp { link, .. } = e { Some(*link) } else { None }).1
}

/// A re-time holds the slot the scheduler gives it, latency and all, from when it is accepted.
#[test]
fn a_retime_holds_the_schedulers_slot() {
    let mut f = ready(16);
    connect_link(&mut f, Sensor::strap(1), 40);
    let slow = connect_link(&mut f, Sensor::strap(2), 400);
    let range = ConnParams { interval_min: Duration::from_u16(150), interval_max: Duration::from_u16(300), ..conn(200) };
    let want: Activities = f.activities().walk(&Scheduler, &[Change::Retime { slot: 1, params: range }]).unwrap();
    f.submit(Op::UpdateParams(slow, range)).unwrap();
    assert_eq!(f.activities().central, want.central);
    let later = f.now().0 + 20_000;
    let events = run(&mut f, later);
    assert_eq!(f.activities().central, want.central);
    let applied = events.iter().find_map(|(_, e)| match e {
        RadioEvent::Params { link, result: Ok(p) } if *link == slow => Some(p.interval),
        _ => None,
    });
    assert_eq!(applied, want.central[1].map(|s| s.interval));
}

#[test]
fn advertising_is_checked_against_a_pending_retime() {
    let mut f = ready(17);
    let l = connect_link(&mut f, Sensor::strap(1), 400);
    f.submit(Op::UpdateParams(l, conn(24))).unwrap();
    let r = f.submit(Op::Advertise(Some(AdvParams { interval_min: Duration::from_u16(792), interval_max: Duration::from_u16(792), connectable: true })));
    assert_eq!(r.map_err(|e| (e.kind, e.native)), Err((FailureKind::Schedule(SchedKind::IntervalTooLarge), 0x85)));
}

#[test]
fn a_link_refused_as_it_forms_carries_the_schedulers_code() {
    let mut f = ready(18);
    let first = connect_link(&mut f, Sensor::strap(1), 400);
    let s = Sensor::strap(2);
    f.sensor_on(s);
    let id = f.submit(Op::Connect(s.addr, conn(200))).unwrap();
    f.submit(Op::UpdateParams(first, conn(28))).unwrap();
    let (_, r) = until(&mut f, |e| if let RadioEvent::Done(i, r) = e { (*i == id).then_some(*r) } else { None });
    assert_eq!(r.map_err(|e| (e.kind, e.native)), Err((FailureKind::Schedule(SchedKind::IntervalTooLarge), 0x85)));
}

#[test]
fn nothing_is_reported_on_a_link_after_it_is_down() {
    for seed in 0..20 {
        for (keys, bond_lost_first) in [(Keys::Bonded, true), (Keys::PhoneLost, true), (Keys::PhoneLost, false)] {
            let mut f = ready(seed);
            f.submit(Op::Advertise(Some(ADV_100))).unwrap();
            let i = f.phone_arrives(Phone { keys, bond_lost_first, ..Phone::new(Os::Android, 2) });
            let (_, link) = until(&mut f, |e| match e {
                RadioEvent::LinkUp { link, .. } if keys == Keys::Bonded => Some(*link),
                RadioEvent::BondLost(link) => Some(*link),
                RadioEvent::Pairing(p) if p.step == PairingStep::Failed => Some(p.link),
                _ => None,
            });
            f.phone_leaves(i);
            let later = f.now().0 + 5_000;
            let events = run(&mut f, later);
            let down = events.iter().position(|(_, e)| matches!(e, RadioEvent::LinkDown { link: l, .. } if *l == link)).unwrap();
            let after: Vec<_> = events[down + 1..]
                .iter()
                .filter(|(_, e)| match e {
                    RadioEvent::DataLength { link: l, .. } | RadioEvent::BondLost(l) => *l == link,
                    RadioEvent::Pairing(p) => p.link == link,
                    _ => false,
                })
                .collect();
            assert!(after.is_empty(), "seed {seed} {keys:?} {bond_lost_first}: {after:?}");
        }
    }
}
