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
use wrist_bluenrg2::{Scheduler, CAPABILITIES, MENU};

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
        h.step(Input::Started(Ok(INFO)));
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

/// Advertising refused for want of resources backs off; the status turns unreachable at 3 s, and
/// the core is awake to show it.
#[test]
fn the_deadline_announces_the_phone_turning_unreachable() {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    core.step(Instant(0), Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
    let out = core.step(Instant(0), Input::Started(Ok(INFO)));
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
    assert_eq!(out.chip, Some(wrist_ble_core::chip::Action::Start));
    let next = out.next.unwrap();
    let out = core.step(next, Input::Tick);
    assert_eq!(out.chip, Some(wrist_ble_core::chip::Action::Start));
}

#[test]
fn a_chip_that_never_comes_up_is_started_once_an_hour_after_its_fault() {
    use wrist_ble_core::chip::{Action, FAULT_RETRY, RESETS_PER_HOUR, START_WITHIN};
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let mut now = Instant(0);
    let mut out = core.step(now, Input::Goals(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }));
    let mut starts = Vec::new();
    while now < Instant(4 * 3_600_000) {
        if out.chip == Some(Action::Start) {
            starts.push(now);
        }
        now = out.next.unwrap();
        out = core.step(now, Input::Tick);
    }
    let (budget, hourly) = starts.split_at(RESETS_PER_HOUR + 1);
    assert_eq!(budget.last(), Some(&(Instant(0) + START_WITHIN + START_WITHIN + START_WITHIN)), "{starts:?}");
    assert_eq!(hourly.len(), 3, "{starts:?}");
    assert!(hourly.windows(2).all(|w| w[1].since(w[0]) == FAULT_RETRY + START_WITHIN), "{starts:?}");
}
