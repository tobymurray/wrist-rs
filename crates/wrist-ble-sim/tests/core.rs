//! `Core::step` driven input by input.

use wrist_ble::{
    AddrKind, BdAddr, BluetoothUuid, ConnHandle, Duration, Instant, LeConnRole, LinkParams, Millis, Op, OpId, PairingEvent, PairingStep,
    PeerAddr, RadioEvent, Status,
};
use wrist_ble::policy::engine::{Core, Input, Outputs};
use wrist_ble::policy::goals::{Goals, Power, Want};
use wrist_ble::policy::peripheral::{Stage, StopReason, GIVE_UP};
use wrist_ble::policy::phone::{ASK_TO_PAIR_AFTER, NUMERIC_DEADLINE};
use wrist_ble::policy::profile::{Cadence, Contact, Kind, Profile, Reading, SecurityNeed, Silence};
use wrist_ble::policy::status::{status, PeerState, PhoneStatus};
use wrist_bluenrg2::INFO;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};
use wrist_bluenrg2::shadow::menu::MENU;

fn hr(data: &[u8]) -> Option<Reading> {
    data.get(1).map(|&v| Reading { value: u16::from(v), contact: Contact::Detected })
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

const PHONE: LinkParams = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };

fn strap() -> PeerAddr {
    PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([1, 2, 3, 4, 5, 6]) }
}

fn phone_addr() -> PeerAddr {
    PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) }
}

/// Answers every operation as accepted at once, and every start as ready.
struct Harness {
    core: Core<'static>,
    now: Instant,
    ops: Vec<Op>,
    next_id: u16,
}

impl Harness {
    fn new(goals: Goals) -> Self {
        let mut h = Self { core: Core::new(&CAPABILITIES, &Scheduler, &MENU), now: Instant(0), ops: Vec::new(), next_id: 1 };
        h.step(Input::Goals(goals));
        let up = started(&h.core);
        h.step(up);
        h
    }

    fn step(&mut self, input: Input) -> Outputs {
        let out = self.core.step(self.now, input);
        if let Some(op) = out.op {
            self.ops.push(op);
            let id = OpId(self.next_id);
            self.next_id += 1;
            self.step(Input::Submitted(Ok(id)));
            if matches!(op, Op::Advertise(_)) {
                self.step(Input::Radio(RadioEvent::Done(id, Ok(wrist_ble::Outcome::Done))));
            }
        }
        out
    }

    fn at(&mut self, ms: u64, input: Input) -> Outputs {
        self.now = Instant(ms);
        self.step(input)
    }

    fn next(&mut self) -> Option<Instant> {
        self.core.step(self.now, Input::Tick).next
    }
}

/// The answer to the start in flight: the chip up.
fn started(core: &Core<'_>) -> Input {
    Input::Started(core.state.chip.starting().expect("a start in flight"), Ok(INFO))
}

fn phone_up(h: &mut Harness, ms: u64, link: ConnHandle) {
    h.at(ms, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: Some(PHONE) }));
}

#[test]
fn the_deadline_announces_securing_turning_unsecured() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    phone_up(&mut h, 1_000, link);
    h.at(1_200, Input::Radio(RadioEvent::Pairing(PairingEvent { link, step: PairingStep::PasskeyWanted })));
    assert_eq!(status(&h.core.state, h.now).phone, PhoneStatus::Securing);
    let next = h.next().unwrap();
    assert!(next <= Instant(1_000) + ASK_TO_PAIR_AFTER + Millis(1), "{next:?}");
    h.at(next.0, Input::Tick);
    let mut t = next;
    while status(&h.core.state, t).phone == PhoneStatus::Securing {
        let next = h.next().unwrap();
        assert!(next > t, "{next:?} at {t:?}");
        t = next;
        h.at(t.0, Input::Tick);
    }
    assert!(t.since(Instant(1_000) + ASK_TO_PAIR_AFTER) <= Millis(1), "{t:?}");
}

/// Advertising refused for want of resources backs off, and the core wakes to show the phone
/// unreachable.
#[test]
fn the_deadline_announces_the_phone_turning_unreachable() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let out = core.step(Instant(0), started(&core));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))));
    let busy = wrist_ble::Failure { kind: wrist_ble::FailureKind::Busy, native: 0x0C };
    let mut now = Instant(0);
    let mut out = core.step(now, Input::Submitted(Err(busy)));
    while matches!(status(&core.state, now).phone, PhoneStatus::Discoverable { .. }) {
        now = out.next.unwrap();
        out = core.step(now, Input::Tick);
        if out.op.is_some() {
            out = core.step(now, Input::Submitted(Err(busy)));
        }
    }
    assert!(matches!(status(&core.state, now).phone, PhoneStatus::Unreachable { .. }));
    assert_eq!(now, Instant(3_001));
}

#[test]
fn the_deadline_counts_the_pairing_down_each_second() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    phone_up(&mut h, 0, link);
    h.at(500, Input::Radio(RadioEvent::Pairing(PairingEvent { link, step: PairingStep::Numeric(123_456) })));
    let mut marks = Vec::new();
    for _ in 0..3 {
        let t = h.next().unwrap();
        marks.push(t.0);
        h.at(t.0, Input::Tick);
    }
    assert_eq!(marks, [1_500, 2_500, 3_500]);
    let PhoneStatus::Pairing { left, .. } = status(&h.core.state, Instant(3_500)).phone else { panic!() };
    assert_eq!(left, Millis(NUMERIC_DEADLINE.0 - 3_000));
}

/// A tap while the link is still being dropped towards a stop is kept until the stop, then acted on.
#[test]
fn tap_to_retry_works_while_the_link_drops() {
    let goals = Goals { radio_on: true, peripherals: [Some(Want { profile: &HR, peer: strap(), priority: 0 }), None, None], ..Goals::OFF };
    let mut h = Harness::new(goals);
    let link = ConnHandle::new(0x802);
    h.at(1_000, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Central, peer: strap(), params: Some(PHONE) }));
    let give_up = u64::from(GIVE_UP.0) + 10;
    h.at(give_up, Input::Tick);
    let machine = |h: &Harness| h.core.state.peers[0].unwrap().machine;
    assert!(matches!(machine(&h).stage, Stage::Dropping { link: l, .. } if l == link), "{:?}", machine(&h));
    assert_eq!(h.ops.last(), Some(&Op::Disconnect(link, wrist_ble::DisconnectReason::RemoteUserTerminatedConn)));
    h.at(give_up + 100, Input::Retry(0));
    h.at(give_up + 200, Input::Radio(RadioEvent::LinkDown { link, reason: Status::CONN_TERMINATED_BY_LOCAL_HOST }));
    let m = machine(&h);
    assert!(matches!(m.stage, Stage::Seeking { .. }), "{m:?}");
    assert_eq!(m.chosen_at, Instant(give_up + 200));
    assert!(matches!(status(&h.core.state, h.now).peers[0].unwrap().state, PeerState::Finding { .. }));
    // A tap with nothing stopped is not kept for a later stop.
    h.at(give_up + 300, Input::Retry(0));
    let stopped = Instant(give_up + 200) + GIVE_UP + Millis(10);
    h.at(stopped.0, Input::Tick);
    assert_eq!(machine(&h).stage, Stage::Stopped(StopReason::NotFound));
}

#[test]
fn a_peers_parameter_request_is_refused_and_said_so() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x803);
    let range = wrist_ble::ParamRange { interval_min: Duration::from_u16(80), interval_max: Duration::from_u16(160), latency: 0, timeout: Duration::from_u16(600) };
    let out = h.at(100, Input::Radio(RadioEvent::PeerParamRequest { link, range }));
    assert_eq!(out.tell[0], Some(wrist_ble::policy::engine::Tell::RefusedPeerRequest(link)));
    assert_eq!(h.ops.last(), Some(&Op::AnswerParamRequest(link, false)));
    assert_eq!(h.core.counters.refused_peer_requests, 1);
}

#[test]
fn a_chip_that_never_comes_up_is_reset_within_its_budget() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let out = core.step(Instant(0), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
    let next = out.next.unwrap();
    let out = core.step(next, Input::Tick);
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
}

#[test]
fn a_chip_that_never_comes_up_is_started_once_an_hour_after_its_fault() {
    use wrist_ble::policy::chip::{Action, FAULT_RETRY, RESETS_PER_HOUR};
    let start_within = CAPABILITIES.start_within;
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let mut now = Instant(0);
    let mut out = core.step(now, Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    let mut starts = Vec::new();
    while now < Instant(4 * 3_600_000) {
        if matches!(out.chip, Some(Action::Start(_))) {
            starts.push(now);
        }
        assert!(out.next.is_some_and(|t| t > now), "{:?} at {now:?}", out.next);
        now = out.next.unwrap();
        out = core.step(now, Input::Tick);
    }
    let (budget, hourly) = starts.split_at(RESETS_PER_HOUR + 1);
    assert_eq!(budget.last(), Some(&(Instant(0) + start_within + start_within + start_within)), "{starts:?}");
    assert_eq!(hourly.len(), 3, "{starts:?}");
    assert!(hourly.windows(2).all(|w| w[1].since(w[0]) == FAULT_RETRY + start_within), "{starts:?}");
}

/// A starting chip's first event is its ready one, so events lost while it starts mean the start
/// failed: it is reset at once, not left silent until the start's deadline.
#[test]
fn events_lost_while_the_chip_starts_reset_it_at_once() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let out = core.step(Instant(0), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
    let out = core.step(Instant(100), Input::Radio(RadioEvent::EventsLost));
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
}

fn done(id: u16) -> Input {
    Input::Radio(RadioEvent::Done(OpId(id), Ok(wrist_ble::Outcome::Done)))
}

/// A core advertising on battery, then asked to advertise at the cable interval: the stop
/// (`OpId(2)`) done, and the start at the new interval accepted as `OpId(3)`.
fn retiming_advertising() -> Core<'static> {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    let out = core.step(Instant(1_000), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    assert!(matches!(out.op, Some(Op::Advertise(None))), "{out:?}");
    core.step(Instant(1_000), Input::Submitted(Ok(OpId(2))));
    let out = core.step(Instant(1_000), done(2));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))), "{out:?}");
    core.step(Instant(1_000), Input::Submitted(Ok(OpId(3))));
    core
}

/// The chip refuses advertising over advertising (HARDWARE.md), so a new interval is a stop
/// and then a start.
#[test]
fn a_new_advertising_interval_is_a_stop_then_a_start() {
    let core = retiming_advertising();
    assert!(matches!(core.pending().map(|p| p.op), Some(Op::Advertise(Some(_)))));
}

fn unanswered_start() -> Core<'static> {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let out = core.step(Instant(0), started(&core));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))), "{out:?}");
    core.step(Instant(0), Input::Submitted(Err(wrist_ble::Failure { kind: wrist_ble::FailureKind::Timeout, native: 0 })));
    core
}

/// The first operation `core` asks for within a minute of `from`, and when.
fn next_op(core: &mut Core<'_>, from: u64) -> Option<(u64, Op)> {
    (1..=60).find_map(|s| core.step(Instant(from + s * 1_000), Input::Tick).op.map(|op| (from + s * 1_000, op)))
}

/// A start the chip never answered may have been taken, so the next is a stop; the chip refusing
/// it for nothing advertising (HARDWARE.md) settles that it wasn't.
#[test]
fn a_start_with_no_answer_is_stopped_before_the_next() {
    let mut core = unanswered_start();
    let (at, next) = next_op(&mut core, 0).expect("nothing more");
    assert_eq!(next, Op::Advertise(None));
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x46 };
    let out = core.step(Instant(at), Input::Submitted(Err(refused)));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))), "{out:?}");
    assert_eq!(core.counters.divergences, 0);
    // Nothing connectable stopped: the radio has been silent since the chip came up.
    assert_eq!(core.state.silent_since, Instant(0));
}

/// A phone's link or the chip's reset ends whatever advertising there was.
#[test]
fn a_link_or_a_reset_settles_a_start_with_no_answer() {
    let mut core = unanswered_start();
    let link = ConnHandle::new(0x801);
    core.step(Instant(10), Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: Some(PHONE) }));
    assert!(!matches!(next_op(&mut core, 10), Some((_, Op::Advertise(None)))));
    let mut core = unanswered_start();
    core.step(Instant(10), Input::Radio(RadioEvent::ChipReset(wrist_ble::ResetCause::Watchdog)));
    let out = core.step(Instant(20), started(&core));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))), "{out:?}");
}

#[test]
fn a_link_or_a_snapshot_settles_the_stop_for_unrecorded_advertising() {
    for snapshot in [false, true] {
        let mut core = unanswered_start();
        let (at, next) = next_op(&mut core, 0).expect("nothing more");
        assert_eq!(next, Op::Advertise(None));
        let ended = if snapshot {
            Input::Snapshot(Ok(wrist_ble::Snapshot { at: Instant(at), links: [None; wrist_ble::MAX_LINKS], advertising: false, scanning: false, initiating: false, anchor: None }))
        } else {
            Input::Radio(RadioEvent::LinkUp { link: ConnHandle::new(0x801), role: LeConnRole::Peripheral, peer: phone_addr(), params: Some(PHONE) })
        };
        core.step(Instant(at), ended);
        let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x46 };
        let out = core.step(Instant(at), Input::Submitted(Err(refused)));
        assert_eq!((core.counters.divergences, out.tell), (0, [None, None]), "{ended:?}");
    }
}

#[test]
fn a_refused_stop_after_a_snapshot_finds_the_recorded_advertising_is_a_divergence() {
    let hidden = Goals { radio_on: true, phone: wrist_ble::policy::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    assert_eq!(core.step(Instant(1_000), Input::Goals(hidden)).op, Some(Op::Advertise(None)));
    core.step(Instant(1_000), Input::Submitted(Err(wrist_ble::Failure { kind: wrist_ble::FailureKind::Timeout, native: 0 })));
    let (at, next) = next_op(&mut core, 1_000).expect("nothing more");
    assert_eq!(next, Op::Advertise(None));
    let s = wrist_ble::Snapshot { at: Instant(at), links: [None; wrist_ble::MAX_LINKS], advertising: true, scanning: false, initiating: false, anchor: None };
    core.step(Instant(at), Input::Snapshot(Ok(s)));
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x46 };
    core.step(Instant(at), Input::Submitted(Err(refused)));
    assert_eq!(core.counters.divergences, 1);
}

#[test]
fn advertising_a_snapshot_finds_unrecorded_is_stopped() {
    let goals = Goals { radio_on: true, phone: wrist_ble::policy::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(goals));
    assert_eq!(core.step(Instant(0), started(&core)).op, None);
    assert!(core.step(Instant(100), Input::Radio(RadioEvent::EventsLost)).snapshot);
    let s = wrist_ble::Snapshot { at: Instant(100), links: [None; wrist_ble::MAX_LINKS], advertising: true, scanning: false, initiating: false, anchor: None };
    let out = core.step(Instant(100), Input::Snapshot(Ok(s)));
    assert_eq!(out.op, Some(Op::Advertise(None)), "{out:?}");
}

fn snapshot_with(link: ConnHandle, encrypted: Option<bool>) -> wrist_ble::Snapshot {
    let mut links = [None; wrist_ble::MAX_LINKS];
    links[0] = Some(wrist_ble::LinkStatus { link, role: LeConnRole::Peripheral, peer: phone_addr(), encrypted });
    wrist_ble::Snapshot { at: Instant(0), links, advertising: false, scanning: false, initiating: false, anchor: None }
}

#[test]
fn advertising_done_after_the_phone_connects_stays_ended() {
    let mut core = retiming_advertising();
    let link = ConnHandle::new(0x801);
    core.step(Instant(1_001), Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: Some(PHONE) }));
    core.step(Instant(1_001), done(3));
    assert_eq!(core.state.radio.advertising, None);
}

#[test]
fn advertising_done_after_a_resync_finds_the_phone_stays_ended() {
    let mut core = retiming_advertising();
    core.step(Instant(1_001), Input::Radio(RadioEvent::EventsLost));
    core.step(Instant(1_002), Input::Snapshot(Ok(snapshot_with(ConnHandle::new(0x801), Some(false)))));
    core.step(Instant(1_003), done(3));
    assert_eq!(core.state.radio.advertising, None);
}

#[test]
fn a_phone_link_without_its_timing_is_up_and_asked_for_its_parameters() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    h.at(1_000, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: None }));
    assert_eq!(h.core.state.phone.link(), Some(link));
    h.at(1_500, Input::Radio(RadioEvent::Encryption { link, on: true }));
    assert!(matches!(h.ops.last(), Some(Op::UpdateParams(l, _)) if *l == link), "{:?}", h.ops);
}

#[test]
fn a_phone_link_found_by_resync_is_asked_for_its_parameters() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let old = ConnHandle::new(0x801);
    phone_up(&mut h, 0, old);
    h.at(1_500, Input::Radio(RadioEvent::Encryption { link: old, on: true }));
    assert!(matches!(h.ops.last(), Some(Op::UpdateParams(l, _)) if *l == old), "{:?}", h.ops);
    let fits = LinkParams { interval: Duration::from_u16(120), latency: 4, timeout: Duration::from_u16(600) };
    h.at(1_600, Input::Radio(RadioEvent::Params { link: old, result: Ok(fits) }));
    let new = ConnHandle::new(0x802);
    h.at(10_000, Input::Radio(RadioEvent::EventsLost));
    h.at(10_000, Input::Snapshot(Ok(snapshot_with(new, Some(false)))));
    // A link the resync found is asked once it is encrypted, as any other.
    h.at(10_500, Input::Radio(RadioEvent::Encryption { link: new, on: true }));
    while h.now < Instant(12_000) {
        let next = h.next().unwrap();
        h.at(next.0, Input::Tick);
    }
    assert!(h.ops.iter().any(|op| matches!(op, Op::UpdateParams(l, _) if *l == new)), "{:?}", h.ops);
}

/// An ask on a link the watch is central of ends with that link's update, where the phone's
/// waits for the central's answer: here the phone's link with its ask on air, which a resync finds
/// is a central link. Another link's update leaves the ask on air.
#[test]
fn a_central_links_ask_ends_with_its_update() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    let other = ConnHandle::new(0x802);
    phone_up(&mut h, 1_000, link);
    h.at(1_500, Input::Radio(RadioEvent::Encryption { link, on: true }));
    let asked = |h: &Harness| h.core.pending().is_some_and(|p| matches!(p.op, Op::UpdateParams(l, _) if l == link));
    assert!(asked(&h), "{:?}", h.ops);
    h.at(2_000, Input::Radio(RadioEvent::EventsLost));
    let mut s = snapshot_with(link, Some(true));
    s.links[0] = s.links[0].map(|l| wrist_ble::LinkStatus { role: LeConnRole::Central, ..l });
    s.links[1] = s.links[0].map(|l| wrist_ble::LinkStatus { link: other, ..l });
    h.at(2_000, Input::Snapshot(Ok(s)));
    assert_eq!(h.core.state.phone.link(), None);
    assert!(asked(&h));
    h.at(2_100, Input::Radio(RadioEvent::Params { link: other, result: Ok(PHONE) }));
    assert!(asked(&h));
    h.at(2_200, Input::Radio(RadioEvent::Params { link, result: Ok(PHONE) }));
    assert!(h.core.pending().is_none(), "{:?}", h.core.pending());
}

/// A phone link secured and fitting its ask, then found again by a resync as `found`.
fn resynced(found: ConnHandle, encrypted: Option<bool>) -> (Harness, usize) {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let old = ConnHandle::new(0x801);
    phone_up(&mut h, 1_000, old);
    h.at(1_500, Input::Radio(RadioEvent::Encryption { link: old, on: true }));
    let fits = LinkParams { interval: Duration::from_u16(120), latency: 4, timeout: Duration::from_u16(600) };
    h.at(1_600, Input::Radio(RadioEvent::ParamsAnswered { link: old, accepted: true }));
    h.at(1_600, Input::Radio(RadioEvent::Params { link: old, result: Ok(fits) }));
    h.at(10_000, Input::Radio(RadioEvent::EventsLost));
    let before = h.ops.len();
    h.at(10_000, Input::Snapshot(Ok(snapshot_with(found, encrypted))));
    (h, before)
}

fn run_to(h: &mut Harness, until: u64) {
    while h.now.0 < until {
        let next = h.next().map_or(until, |t| t.0.max(h.now.0 + 1).min(until));
        h.at(next, Input::Tick);
    }
}

fn asked(ops: &[Op]) -> bool {
    ops.iter().any(|op| matches!(op, Op::UpdateParams(..)))
}

fn asked_to_pair(ops: &[Op]) -> bool {
    ops.iter().any(|op| matches!(op, Op::RequestSecurity(_)))
}

#[test]
fn a_link_a_resync_finds_encrypted_is_asked_and_not_asked_to_pair() {
    for found in [ConnHandle::new(0x801), ConnHandle::new(0x802)] {
        let (mut h, before) = resynced(found, Some(true));
        run_to(&mut h, 70_000);
        assert!(asked(&h.ops[before..]), "{found:?}: {:?}", &h.ops[before..]);
        assert!(!asked_to_pair(&h.ops[before..]), "{found:?}: {:?}", &h.ops[before..]);
    }
}

#[test]
fn a_link_whose_security_a_resync_cannot_tell_is_left_alone_until_an_event_tells() {
    for found in [ConnHandle::new(0x801), ConnHandle::new(0x802)] {
        let (mut h, before) = resynced(found, None);
        run_to(&mut h, 70_000);
        assert!(!asked(&h.ops[before..]), "{found:?}: {:?}", &h.ops[before..]);
        assert!(!asked_to_pair(&h.ops[before..]), "{found:?}: {:?}", &h.ops[before..]);
        h.at(70_000, Input::Radio(RadioEvent::Encryption { link: found, on: true }));
        assert!(asked(&h.ops[before..]), "{found:?}: {:?}", &h.ops[before..]);
    }
}

#[test]
fn a_secure_link_a_resync_finds_unencrypted_is_held_and_asked_to_pair() {
    let link = ConnHandle::new(0x801);
    let (mut h, before) = resynced(link, Some(false));
    run_to(&mut h, 10_001 + u64::from(ASK_TO_PAIR_AFTER.0));
    assert!(!asked(&h.ops[before..]), "{:?}", &h.ops[before..]);
    assert_eq!(&h.ops[before..], [Op::RequestSecurity(link)]);
}

/// The chip refuses the security request for good, at submission or as its outcome: the pairing
/// fails at once and the link may be asked for parameters, without the outcome's deadline.
#[test]
fn a_security_request_refused_for_good_fails_the_pairing_at_once() {
    let link = ConnHandle::new(0x801);
    let never = wrist_ble::Failure { kind: wrist_ble::FailureKind::Security, native: 0x40 };
    let busy = wrist_ble::Failure { kind: wrist_ble::FailureKind::Busy, native: 0x0C };
    for (refusal, fails) in [(Input::Submitted(Err(never)), true), (Input::Submitted(Err(busy)), false), (Input::Radio(RadioEvent::Done(OpId(900), Err(never))), true)] {
        let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
        phone_up(&mut h, 1_000, link);
        let mut now = Instant(1_000);
        let out = (0..100)
            .find_map(|_| {
                now = h.core.step(now, Input::Tick).next.unwrap();
                Some(h.core.step(now, Input::Tick)).filter(|out| out.op.is_some())
            })
            .expect("an operation");
        assert_eq!(out.op, Some(Op::RequestSecurity(link)));
        let refusal = match refusal {
            Input::Radio(RadioEvent::Done(_, r)) => {
                h.core.step(now, Input::Submitted(Ok(OpId(900))));
                Input::Radio(RadioEvent::Done(OpId(900), r))
            }
            r => r,
        };
        h.core.step(now + Millis(10), refusal);
        let open = h.core.state.phone.may_ask_params(now + Millis(10));
        assert_eq!(open, fails, "{refusal:?}");
    }
}

#[test]
fn a_refusal_the_plan_cannot_avoid_sends_the_chip_to_its_baseline() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    let hidden = Goals { radio_on: true, phone: wrist_ble::policy::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
    let out = core.step(Instant(1_000), Input::Goals(hidden));
    assert_eq!(out.op, Some(Op::Advertise(None)));
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x0C };
    let out = core.step(Instant(1_000), Input::Submitted(Err(refused)));
    assert_eq!(out.op, Some(Op::Advertise(None)));
    let out = core.step(Instant(1_000), Input::Submitted(Err(refused)));
    assert!(matches!((out.op, out.chip), (None, Some(wrist_ble::policy::chip::Action::Start(_)))), "{out:?}");
}

#[test]
fn a_refusal_the_retry_gets_past_is_not_held_against_a_later_one() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let shown = Goals { radio_on: true, ..Goals::OFF };
    let hidden = Goals { radio_on: true, phone: wrist_ble::policy::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x0C };
    core.step(Instant(0), Input::Goals(shown));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    core.step(Instant(1_000), Input::Goals(hidden));
    core.step(Instant(1_000), Input::Submitted(Err(refused)));
    core.step(Instant(1_000), Input::Submitted(Ok(OpId(2))));
    core.step(Instant(1_001), done(2));
    core.step(Instant(3_600_000), Input::Goals(shown));
    core.step(Instant(3_600_000), Input::Submitted(Ok(OpId(3))));
    core.step(Instant(3_600_001), done(3));
    let out = core.step(Instant(7_200_000), Input::Goals(hidden));
    assert_eq!(out.op, Some(Op::Advertise(None)));
    let out = core.step(Instant(7_200_000), Input::Submitted(Err(refused)));
    assert_eq!((out.op, out.chip), (Some(Op::Advertise(None)), None));
}

#[test]
fn a_refusal_before_the_chip_resets_is_forgotten_with_it() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x0C };
    let busy = wrist_ble::Failure { kind: wrist_ble::FailureKind::Busy, native: 0x0C };
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    let first = core.step(Instant(0), started(&core)).op;
    let second = core.step(Instant(0), Input::Submitted(Err(refused))).op;
    assert_ne!(first, second);
    core.step(Instant(0), Input::Submitted(Err(busy)));
    core.step(Instant(5), Input::Radio(RadioEvent::ChipReset(wrist_ble::ResetCause::Watchdog)));
    assert_eq!(core.step(Instant(10), started(&core)).op, first);
    let out = core.step(Instant(10), Input::Submitted(Err(refused)));
    assert_eq!((out.op, out.chip), (second, None));
}

/// The BlueNRG-2 announces a start it was not asked for with the same event as one it was (Linux
/// check 8: the updater entered mid-link, then the chip silent).
#[test]
fn a_chip_starting_again_unasked_is_a_reset_that_takes_the_link() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    phone_up(&mut h, 1_000, link);
    let out = h.at(2_000, Input::Radio(RadioEvent::Ready(INFO)));
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
    assert_eq!(h.core.state.phone.link(), None);
    assert_eq!(h.core.counters.resets, 1);
    h.ops.clear();
    h.at(2_500, started(&h.core));
    h.at(3_000, Input::Tick);
    assert!(matches!(h.ops.first(), Some(Op::Advertise(Some(_)))), "{:?}", h.ops);
}

/// Each start's answer names it, so an answer to a start the deadline replaced is not taken for
/// the new one's, and the new one's answer then is no restart.
#[test]
fn a_late_answer_for_an_earlier_start_is_not_this_ones() {
    use wrist_ble::policy::chip::{Action, Chip};
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let Some(Action::Start(first)) = core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF })).chip else { panic!() };
    let late = Instant(0) + CAPABILITIES.start_within;
    let Some(Action::Start(second)) = core.step(late, Input::Tick).chip else { panic!() };
    assert_ne!(first, second);
    core.step(late, Input::Started(first, Ok(INFO)));
    assert!(matches!(core.state.chip.state, Chip::Starting { .. }), "{:?}", core.state.chip.state);
    let out = core.step(late + Millis(300), Input::Started(second, Ok(INFO)));
    assert_eq!((core.state.chip.state, out.chip), (Chip::Ready(INFO), None));
    assert_eq!(core.counters.resets, 1, "the missed deadline alone");
}

/// The chip's own ready event is a boot nobody asked for: during a start it answers nothing, so a
/// driver that passed its boot's event before the answer does not loop resets.
#[test]
fn the_chips_ready_during_a_start_answers_nothing() {
    use wrist_ble::policy::chip::Chip;
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    for i in 0..10 {
        let t = Instant(i * 600_000);
        assert_eq!(core.step(t, Input::Radio(RadioEvent::Ready(INFO))).chip, None);
        assert!(matches!(core.state.chip.state, Chip::Starting { .. }));
        assert_eq!(core.step(t, started(&core)).chip, None);
        assert_eq!(core.state.chip.state, Chip::Ready(INFO));
        core.step(t + Millis(1), Input::Goals(Goals::OFF));
        core.step(t + Millis(2), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    }
    assert_eq!(core.counters.resets, 0);
}

/// A start that missed its deadline restarts the chip as surely as a reset does.
#[test]
fn a_start_past_its_deadline_counts_as_a_reset() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let out = core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let out = core.step(out.next.unwrap(), Input::Tick);
    assert!(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))), "{out:?}");
    assert_eq!(core.counters.resets, 1);
}

/// `BlueZ` accepts a request that changes nothing and sends no update (`fixtures/traces/bluez-*.log`).
#[test]
fn a_centrals_acceptance_ends_the_request_though_nothing_changes() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    phone_up(&mut h, 0, link);
    h.at(1_500, Input::Radio(RadioEvent::Encryption { link, on: true }));
    assert!(matches!(h.ops.last(), Some(Op::UpdateParams(..))), "{:?}", h.ops);
    let asked = h.next_id - 1;
    h.at(1_800, done(asked));
    // Nothing is left pending: the console's own ask goes out at once.
    assert!(h.core.pending().is_none());
    let now = wrist_bluenrg2::shadow::menu::interval_now_ask(6, 6);
    h.at(2_000, Input::IntervalNow(now));
    let asks = h.ops.iter().filter(|op| matches!(op, Op::UpdateParams(l, _) if *l == link)).count();
    assert_eq!(asks, 2, "{:?}", h.ops);
}

/// Events taken in the order they came, then a snapshot holding those after its reading: the two agree.
#[test]
fn a_snapshot_taken_after_the_events_it_holds_agrees_with_them() {
    let link = ConnHandle::new(0x801);
    let none = wrist_ble::Snapshot { at: Instant(0), links: [None; wrist_ble::MAX_LINKS], advertising: false, scanning: false, initiating: false, anchor: None };

    // The phone's link ends after the reading: no phantom link, and advertising comes back.
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    phone_up(&mut h, 1_000, link);
    assert!(h.at(10_000, Input::Radio(RadioEvent::EventsLost)).snapshot);
    h.at(10_001, Input::Radio(RadioEvent::LinkDown { link, reason: Status::new(0x13) }));
    let before = h.ops.len();
    h.at(10_002, Input::Snapshot(Ok(none)));
    assert_eq!(h.core.state.phone.link(), None);
    run_to(&mut h, 12_000);
    assert!(h.ops[before..].iter().any(|op| matches!(op, Op::Advertise(Some(_)))), "{:?}", h.ops);
    assert!(h.core.state.radio.advertising.is_some());

    // The phone connects after the reading: its link kept, and no advertising over it.
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    assert!(h.core.state.radio.advertising.is_some());
    h.at(10_000, Input::Radio(RadioEvent::EventsLost));
    phone_up(&mut h, 10_001, link);
    let before = h.ops.len();
    h.at(10_002, Input::Snapshot(Ok(snapshot_with(link, Some(false)))));
    run_to(&mut h, 12_000);
    assert_eq!(h.core.state.phone.link(), Some(link));
    assert!(!h.ops[before..].iter().any(|op| matches!(op, Op::Advertise(_))), "{:?}", h.ops);
    assert_eq!(h.core.counters.audit_mismatches, 0);
}

/// A snapshot that failed is asked for again after a while, longer each time it fails, and the
/// wait starts again after one that worked or with the radio.
#[test]
fn a_failed_snapshot_is_asked_for_again() {
    use wrist_ble::policy::engine::SNAPSHOT_RETRY;
    let retry = u64::from(SNAPSHOT_RETRY.0);
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    assert!(h.at(10_000, Input::Radio(RadioEvent::EventsLost)).snapshot);
    let mut at = 10_000;
    for wait in [retry, 2 * retry, 4 * retry, 8 * retry, 16 * retry, 32 * retry, 32 * retry] {
        assert!(!h.at(at, Input::Snapshot(Err(wrist_ble::Fault::Silent))).snapshot);
        assert_eq!(h.next(), Some(Instant(at + wait)));
        assert!(!h.at(at + wait - 1, Input::Tick).snapshot);
        at += wait;
        assert!(h.at(at, Input::Tick).snapshot);
    }
    let none = wrist_ble::Snapshot { at: Instant(0), links: [None; wrist_ble::MAX_LINKS], advertising: true, scanning: false, initiating: false, anchor: None };
    // One that worked between a failure and its retry: no retry, and the next failure waits the least.
    h.at(at, Input::Snapshot(Err(wrist_ble::Fault::Silent)));
    h.at(at + 1, Input::Radio(RadioEvent::EventsLost));
    h.at(at + 2, Input::Snapshot(Ok(none)));
    assert!(h.next().is_none_or(|t| t.0 > at + 32 * retry), "{:?}", h.next());
    h.at(at + 3, Input::Radio(RadioEvent::EventsLost));
    h.at(at + 4, Input::Snapshot(Err(wrist_ble::Fault::Silent)));
    assert_eq!(h.next(), Some(Instant(at + 4 + retry)));
    // The radio off lets the retry go.
    for _ in 0..3 {
        let t = h.next().unwrap().0;
        h.at(t, Input::Tick);
        h.at(t + 1, Input::Snapshot(Err(wrist_ble::Fault::Silent)));
    }
    h.at(at + 60_000, Input::Goals(Goals::OFF));
    assert!(h.next().is_none(), "{:?}", h.next());
    // Back on, a first failure waits the least again.
    h.at(at + 61_000, Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let up = started(&h.core);
    h.step(up);
    h.at(at + 62_000, Input::Radio(RadioEvent::EventsLost));
    h.at(at + 62_000, Input::Snapshot(Err(wrist_ble::Fault::Silent)));
    assert_eq!(h.next(), Some(Instant(at + 62_000 + retry)));
}

fn wanting(peer: PeerAddr, profile: &'static Profile) -> Goals {
    Goals { radio_on: true, peripherals: [Some(Want { profile, peer, priority: 0 }), None, None], ..Goals::OFF }
}

fn strap_linked(h: &mut Harness, ms: u64) -> ConnHandle {
    let link = ConnHandle::new(0x802);
    h.at(ms, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Central, peer: strap(), params: Some(PHONE) }));
    assert!(matches!(h.core.state.peers[0].unwrap().machine.stage, Stage::Linked { .. }));
    link
}

static CSC: Profile = Profile { kind: Kind::Csc, service: BluetoothUuid::from_u16(0x1816), subscribe: &[BluetoothUuid::from_u16(0x2A5B)], cadence: Cadence::Events, silence: Silence::Idle, ..HR };

/// The same goals again leave each peripheral where it is.
#[test]
fn goals_set_again_keep_each_peripheral_as_it_is() {
    let mut h = Harness::new(wanting(strap(), &HR));
    strap_linked(&mut h, 1_000);
    let before = h.core.state.peers[0].unwrap().machine;
    let ops = h.ops.len();
    h.at(2_000, Input::Goals(wanting(strap(), &HR)));
    assert_eq!(h.core.state.peers[0].unwrap().machine, before);
    assert_eq!(h.ops.len(), ops, "{:?}", &h.ops[ops..]);
}

/// A peripheral wanted with another profile is let go and set up afresh, its link first.
#[test]
fn a_peripheral_wanted_with_another_profile_starts_afresh() {
    let mut h = Harness::new(wanting(strap(), &HR));
    let link = strap_linked(&mut h, 1_000);
    h.at(2_000, Input::Goals(wanting(strap(), &CSC)));
    assert_eq!(h.ops.last(), Some(&Op::Disconnect(link, wrist_ble::DisconnectReason::RemoteUserTerminatedConn)));
    h.at(2_100, Input::Radio(RadioEvent::LinkDown { link, reason: Status::CONN_TERMINATED_BY_LOCAL_HOST }));
    let p = h.core.state.peers[0].unwrap();
    assert_eq!(p.want.profile.kind, Kind::Csc);
    assert_eq!((p.machine.stage, p.machine.chosen_at), (Stage::Seeking { since: Instant(2_100), quick: 0 }, Instant(2_100)));
}

/// Another peripheral in a slot is looked for at once, or once the link to the one it replaces
/// is down.
#[test]
fn a_peripheral_replaced_by_another_is_looked_for() {
    let other = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([6, 5, 4, 3, 2, 1]) };
    let mut h = Harness::new(wanting(strap(), &HR));
    h.at(2_000, Input::Goals(wanting(other, &HR)));
    let p = h.core.state.peers[0].unwrap();
    assert_eq!((p.want.peer, p.machine.stage), (other, Stage::Seeking { since: Instant(2_000), quick: 0 }));

    let mut h = Harness::new(wanting(strap(), &HR));
    let link = strap_linked(&mut h, 1_000);
    h.at(2_000, Input::Goals(wanting(other, &HR)));
    assert_eq!(h.ops.last(), Some(&Op::Disconnect(link, wrist_ble::DisconnectReason::RemoteUserTerminatedConn)));
    h.at(2_100, Input::Radio(RadioEvent::LinkDown { link, reason: Status::CONN_TERMINATED_BY_LOCAL_HOST }));
    let p = h.core.state.peers[0].unwrap();
    assert_eq!((p.want.peer, p.machine.stage), (other, Stage::Seeking { since: Instant(2_100), quick: 0 }));
    assert!(matches!(status(&h.core.state, h.now).peers[0].unwrap().state, PeerState::Finding { .. }));
}

/// A peripheral no longer wanted keeps its slot until its link is down.
#[test]
fn a_peripheral_let_go_is_disconnected_before_it_goes() {
    let mut h = Harness::new(wanting(strap(), &HR));
    let link = strap_linked(&mut h, 1_000);
    h.at(2_000, Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    assert_eq!(h.ops.last(), Some(&Op::Disconnect(link, wrist_ble::DisconnectReason::RemoteUserTerminatedConn)));
    assert!(matches!(h.core.state.peers[0].unwrap().machine.stage, Stage::Dropping { .. }));
    h.at(2_100, Input::Radio(RadioEvent::LinkDown { link, reason: Status::CONN_TERMINATED_BY_LOCAL_HOST }));
    assert!(h.core.state.peers[0].is_none());
}

/// Goals set again unchanged leave the settle clock; a change to what the radio plans for
/// restarts it.
#[test]
fn the_settle_clock_restarts_with_each_change_to_the_radios_goals() {
    use wrist_ble::policy::goals::PhoneGoal;
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let g = *h.core.goals();
    h.at(1_000, Input::Goals(g));
    assert_eq!(h.core.settled_since, Instant(0));
    let changes = [
        Goals { power: Power::Cable, ..g },
        Goals { transfer: true, ..g },
        Goals { phone: PhoneGoal { discoverable: false, ..g.phone }, ..g },
        Goals { radio_on: false, ..g },
    ];
    for (i, changed) in changes.into_iter().enumerate() {
        let ms = 2_000 * (i as u64 + 1);
        h.at(ms, Input::Goals(changed));
        assert_eq!(h.core.settled_since, Instant(ms), "{changed:?}");
        h.at(ms + 1_000, Input::Goals(g));
        if g.radio_on && !changed.radio_on {
            let up = started(&h.core);
            h.step(up);
        }
    }
}

/// A security request goes out beside an operation on another link
/// (`a_security_request_waits_for_its_links_ask` in `reconciler.rs` for its own link's).
#[test]
fn a_security_request_goes_beside_another_links_operation() {
    let mut h = Harness::new(wanting(strap(), &HR));
    let strap_link = strap_linked(&mut h, 1_000);
    phone_up(&mut h, 1_100, ConnHandle::new(0x801));
    h.at(2_000, Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    assert_eq!(h.core.pending().map(|p| p.op), Some(Op::Disconnect(strap_link, wrist_ble::DisconnectReason::RemoteUserTerminatedConn)));
    h.at(6_100, Input::Tick);
    assert_eq!(h.ops.last(), Some(&Op::RequestSecurity(ConnHandle::new(0x801))));
}

/// A peripheral stopped for a busy radio looks again once the planner fits it.
#[test]
fn a_peripheral_stopped_for_a_busy_radio_resumes_when_it_fits() {
    use wrist_ble::policy::plan::Conflict;
    let mut h = Harness::new(wanting(strap(), &HR));
    h.core.state.peers[0].as_mut().unwrap().machine.stage = Stage::Stopped(StopReason::RadioBusy(Conflict::Transfer));
    h.at(5_000, Input::Tick);
    let m = h.core.state.peers[0].unwrap().machine;
    assert_eq!((m.stage, m.heard_at), (Stage::Seeking { since: Instant(5_000), quick: 0 }, Instant(5_000)));
}
