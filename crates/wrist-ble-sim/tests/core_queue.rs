//! The core's queue of operations, filled while the radio has yet to answer the last ask.

use wrist_ble::{
    AddrKind, BdAddr, ConnHandle, Duration, Instant, LeConnRole, LinkParams, Op, OpId, Outcome, PairingEvent, PairingStep, ParamRange, PeerAddr,
    RadioEvent,
};
use wrist_ble::policy::engine::{Core, Input};
use wrist_ble::policy::goals::Goals;
use wrist_ble::policy::phone::ASK_TO_PAIR_AFTER;
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES, INFO};

fn phone() -> ConnHandle {
    ConnHandle::new(0x801)
}
const RANGE: ParamRange = ParamRange { interval_min: Duration::from_u16(80), interval_max: Duration::from_u16(160), latency: 0, timeout: Duration::from_u16(600) };

struct Harness {
    core: Core<'static>,
    now: Instant,
    ops: Vec<Op>,
    next_id: u16,
}

impl Harness {
    /// Radio on with the phone linked, nothing asked.
    fn new() -> Self {
        let mut h = Self { core: Core::new(&CAPABILITIES, &Scheduler, &MENU), now: Instant(0), ops: Vec::new(), next_id: 1 };
        h.step(Input::Goals(Goals { radio_on: true, ..Goals::OFF }));
        h.step(Input::Started(h.core.state.chip.starting().expect("a start in flight"), Ok(INFO)));
        let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) };
        let params = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };
        h.step(Input::Radio(RadioEvent::LinkUp { link: phone(), role: LeConnRole::Peripheral, peer, params: Some(params) }));
        h
    }

    /// Accepts and completes every operation asked for.
    fn step(&mut self, input: Input) {
        if let Some(op) = self.core.step(self.now, input).op {
            self.ops.push(op);
            self.release();
        }
    }

    /// Steps without answering an operation asked for, which holds back the queue.
    fn hold(&mut self, input: Input) {
        if let Some(op) = self.core.step(self.now, input).op {
            self.ops.push(op);
        }
    }

    /// Accepts and completes the operation last asked for.
    fn release(&mut self) {
        let id = OpId(self.next_id);
        self.next_id += 1;
        self.step(Input::Submitted(Ok(id)));
        self.step(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
    }

    fn peer_request(&mut self, link: u16) {
        self.hold(Input::Radio(RadioEvent::PeerParamRequest { link: ConnHandle::new(link), range: RANGE }));
    }
}

#[test]
fn a_numeric_answer_survives_a_queue_of_parameter_answers() {
    let mut h = Harness::new();
    h.now = Instant(100);
    h.peer_request(0x810);
    assert_eq!(h.ops.last(), Some(&Op::AnswerParamRequest(ConnHandle::new(0x810), false)));
    for link in [0x801, 0x802, 0x803, 0x804] {
        h.peer_request(link);
    }
    h.hold(Input::Radio(RadioEvent::Pairing(PairingEvent { link: phone(), step: PairingStep::Numeric(123_456) })));
    h.hold(Input::Answer(true));
    h.release();
    assert!(h.ops.contains(&Op::ConfirmNumeric(phone(), true)), "{:?}", h.ops);
    assert_eq!(h.core.counters.dropped, 0);
}

#[test]
fn an_answer_takes_a_security_requests_place_and_the_request_is_counted() {
    let mut h = Harness::new();
    h.now = Instant(100);
    h.peer_request(0x810);
    h.now = Instant(0) + ASK_TO_PAIR_AFTER;
    h.hold(Input::Tick);
    let links = 0x820..0x820 + 7;
    for link in links.clone() {
        h.peer_request(link);
    }
    h.release();
    for link in links {
        assert!(h.ops.contains(&Op::AnswerParamRequest(ConnHandle::new(link), false)), "{link:#x}: {:?}", h.ops);
    }
    assert!(!h.ops.contains(&Op::RequestSecurity(phone())), "{:?}", h.ops);
    assert_eq!(h.core.counters.dropped, 1);
}

#[test]
fn an_answer_already_queued_is_not_queued_twice() {
    let mut h = Harness::new();
    h.now = Instant(100);
    h.peer_request(0x810);
    for _ in 0..8 {
        h.peer_request(0x820);
    }
    h.release();
    let answers = h.ops.iter().filter(|&&op| op == Op::AnswerParamRequest(ConnHandle::new(0x820), false)).count();
    assert_eq!(answers, 1, "{:?}", h.ops);
    assert_eq!(h.core.counters.dropped, 0);
}
