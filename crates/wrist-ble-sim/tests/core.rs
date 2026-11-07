//! `Core::step` driven input by input.

use wrist_ble::{
    AddrKind, BdAddr, BluetoothUuid, ConnHandle, Duration, Instant, LeConnRole, LinkParams, Millis, Op, OpId, PairingEvent, PairingStep,
    PeerAddr, RadioEvent, Status,
};
use wrist_ble_core::engine::{Core, Input, Outputs};
use wrist_ble_core::goals::{Goals, Power, Want};
use wrist_ble_core::peripheral::{Stage, StopReason, GIVE_UP};
use wrist_ble_core::phone::{ASK_TO_PAIR_AFTER, NUMERIC_DEADLINE};
use wrist_ble_core::profile::{Cadence, Contact, Kind, Profile, Reading, SecurityNeed, Silence};
use wrist_ble_core::status::{status, PeerState, PhoneStatus};
use wrist_ble_sim::fake::INFO;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};
use wrist_ble_sim::menu::MENU;

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
    h.at(ms, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: PHONE }));
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
        t = h.next().unwrap();
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
    h.at(1_000, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Central, peer: strap(), params: PHONE }));
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
    assert_eq!(out.tell[0], Some(wrist_ble_core::engine::Tell::RefusedPeerRequest(link)));
    assert_eq!(h.ops.last(), Some(&Op::AnswerParamRequest(link, false)));
    assert_eq!(h.core.counters.refused_peer_requests, 1);
}

#[test]
fn a_chip_that_never_comes_up_is_reset_within_its_budget() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let out = core.step(Instant(0), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
    let next = out.next.unwrap();
    let out = core.step(next, Input::Tick);
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
}

#[test]
fn a_chip_that_never_comes_up_is_started_once_an_hour_after_its_fault() {
    use wrist_ble_core::chip::{Action, FAULT_RETRY, RESETS_PER_HOUR};
    let start_within = CAPABILITIES.start_within;
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let mut now = Instant(0);
    let mut out = core.step(now, Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    let mut starts = Vec::new();
    while now < Instant(4 * 3_600_000) {
        if matches!(out.chip, Some(Action::Start(_))) {
            starts.push(now);
        }
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
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
    let out = core.step(Instant(100), Input::Radio(RadioEvent::EventsLost));
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
}

fn done(id: u16) -> Input {
    Input::Radio(RadioEvent::Done(OpId(id), Ok(wrist_ble::Outcome::Done)))
}

/// A core advertising on battery, then asked to advertise at the cable interval, that op accepted as `OpId(2)`.
fn retiming_advertising() -> Core<'static> {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    let out = core.step(Instant(1_000), Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    assert!(matches!(out.op, Some(Op::Advertise(Some(_)))), "{out:?}");
    core.step(Instant(1_000), Input::Submitted(Ok(OpId(2))));
    core
}

fn snapshot_with(link: ConnHandle) -> wrist_ble::Snapshot {
    let mut links = [None; wrist_ble::MAX_LINKS];
    links[0] = Some(wrist_ble::LinkStatus { link, role: LeConnRole::Peripheral, peer: phone_addr() });
    wrist_ble::Snapshot { at: Instant(0), links, advertising: false, scanning: false, initiating: false, anchor: None }
}

#[test]
fn advertising_done_after_the_phone_connects_stays_ended() {
    let mut core = retiming_advertising();
    let link = ConnHandle::new(0x801);
    core.step(Instant(1_001), Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer: phone_addr(), params: PHONE }));
    core.step(Instant(1_001), done(2));
    assert_eq!(core.state.radio.advertising, None);
}

#[test]
fn advertising_done_after_a_resync_finds_the_phone_stays_ended() {
    let mut core = retiming_advertising();
    core.step(Instant(1_001), Input::Radio(RadioEvent::EventsLost));
    core.step(Instant(1_002), Input::Snapshot(Ok(snapshot_with(ConnHandle::new(0x801)))));
    core.step(Instant(1_003), done(2));
    assert_eq!(core.state.radio.advertising, None);
}

#[test]
fn a_phone_link_found_by_resync_is_asked_for_its_parameters() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let old = ConnHandle::new(0x801);
    phone_up(&mut h, 0, old);
    h.at(1_500, Input::Tick);
    assert!(matches!(h.ops.last(), Some(Op::UpdateParams(l, _)) if *l == old), "{:?}", h.ops);
    let fits = LinkParams { interval: Duration::from_u16(120), latency: 4, timeout: Duration::from_u16(600) };
    h.at(1_600, Input::Radio(RadioEvent::Params { link: old, result: Ok(fits) }));
    let new = ConnHandle::new(0x802);
    h.at(10_000, Input::Radio(RadioEvent::EventsLost));
    h.at(10_000, Input::Snapshot(Ok(snapshot_with(new))));
    while h.now < Instant(12_000) {
        let next = h.next().unwrap();
        h.at(next.0, Input::Tick);
    }
    assert!(h.ops.iter().any(|op| matches!(op, Op::UpdateParams(l, _) if *l == new)), "{:?}", h.ops);
}

#[test]
fn a_refusal_the_plan_cannot_avoid_sends_the_chip_to_its_baseline() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    core.step(Instant(0), started(&core));
    core.step(Instant(0), Input::Submitted(Ok(OpId(1))));
    core.step(Instant(1), done(1));
    let hidden = Goals { radio_on: true, phone: wrist_ble_core::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
    let out = core.step(Instant(1_000), Input::Goals(hidden));
    assert_eq!(out.op, Some(Op::Advertise(None)));
    let refused = wrist_ble::Failure { kind: wrist_ble::FailureKind::ModeExclusion, native: 0x0C };
    let out = core.step(Instant(1_000), Input::Submitted(Err(refused)));
    assert_eq!(out.op, Some(Op::Advertise(None)));
    let out = core.step(Instant(1_000), Input::Submitted(Err(refused)));
    assert!(matches!((out.op, out.chip), (None, Some(wrist_ble_core::chip::Action::Start(_)))), "{out:?}");
}

#[test]
fn a_refusal_the_retry_gets_past_is_not_held_against_a_later_one() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let shown = Goals { radio_on: true, ..Goals::OFF };
    let hidden = Goals { radio_on: true, phone: wrist_ble_core::goals::PhoneGoal { discoverable: false, pairing_open: false }, ..Goals::OFF };
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
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
    assert_eq!(h.core.state.phone.link(), None);
    assert_eq!(h.core.counters.resets, 1);
    h.ops.clear();
    h.at(2_500, started(&h.core));
    h.at(3_000, Input::Tick);
    assert!(matches!(h.ops.first(), Some(Op::Advertise(Some(_)))), "{:?}", h.ops);
}

/// Each start's answer names it, so an answer to a start the deadline replaced is not taken for
/// the new one's, and the new one's answer then is no restart (F75).
#[test]
fn a_late_answer_for_an_earlier_start_is_not_this_ones() {
    use wrist_ble_core::chip::{Action, Chip};
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
/// driver that passed its boot's event before the answer does not loop resets (F75).
#[test]
fn the_chips_ready_during_a_start_answers_nothing() {
    use wrist_ble_core::chip::Chip;
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

/// A start that missed its deadline restarts the chip as surely as a reset does (F77).
#[test]
fn a_start_past_its_deadline_counts_as_a_reset() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let out = core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let out = core.step(out.next.unwrap(), Input::Tick);
    assert!(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))), "{out:?}");
    assert_eq!(core.counters.resets, 1);
}

/// `BlueZ` accepts a request that changes nothing and sends no update (`fixtures/traces/bluez-*.log`).
#[test]
fn a_centrals_acceptance_ends_the_request_though_nothing_changes() {
    let mut h = Harness::new(Goals { radio_on: true, ..Goals::OFF });
    let link = ConnHandle::new(0x801);
    phone_up(&mut h, 0, link);
    h.at(1_500, Input::Tick);
    assert!(matches!(h.ops.last(), Some(Op::UpdateParams(..))), "{:?}", h.ops);
    let asked = h.next_id - 1;
    h.at(1_800, done(asked));
    while h.now < Instant(6_000) {
        let next = h.next().unwrap();
        h.at(next.0, Input::Tick);
    }
    assert!(h.ops.iter().any(|op| matches!(op, Op::RequestSecurity(l) if *l == link)), "{:?}", h.ops);
}
