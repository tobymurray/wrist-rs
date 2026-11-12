//! The operations the chip has taken: ids, deadlines, and the event that ends each.

use wrist_ble::{
    AddrKind, AdvParams, BdAddr, ChipInfo, ConnHandle, ConnParams, CoreSpecificationVersion, DisconnectReason, Failure, FailureKind, Interval,
    LeConnRole, LinkParams, Op, OpId, Outcome, PairingEvent, PairingStep, PeerAddr, RadioEvent, ResetCause, SchedKind, Slots, Status, Timeout,
};
use wrist_bluenrg2::driver::Unanswered;
use wrist_bluenrg2::ops::{refusal, unanswered, Ops, MAX_PENDING};

fn link(n: u16) -> ConnHandle {
    ConnHandle::new(0x0800 + n)
}

const ADV: AdvParams = AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(160), connectable: true };

fn ask(l: ConnHandle) -> Op {
    let params = ConnParams {
        interval_min: Interval::from_u16(12),
        interval_max: Interval::from_u16(24),
        latency: 0,
        timeout: Timeout::from_u16(500),
        ce_len: Slots::from_u16(0),
    };
    Op::UpdateParams(l, params)
}

fn done(id: u16) -> RadioEvent {
    RadioEvent::Done(OpId(id), Ok(Outcome::Done))
}

fn failed(id: u16, kind: FailureKind) -> RadioEvent {
    RadioEvent::Done(OpId(id), Err(Failure { kind, native: 0 }))
}

#[test]
fn an_operation_its_answer_ends_is_done_at_once_and_ids_count_up() {
    let mut ops = Ops::default();
    assert_eq!(ops.answered(&Op::Advertise(Some(ADV)), 0, 0), Ok(OpId(0)));
    assert_eq!(ops.answered(&Op::AllowRebond(link(1)), 0, 0), Ok(OpId(1)));
    assert_eq!((ops.ended(), ops.ended(), ops.ended()), (Some(done(0)), Some(done(1)), None));
    assert_eq!(ops.due(), None);
}

#[test]
fn a_refusal_keeps_the_chips_code_and_takes_no_id() {
    let mut ops = Ops::default();
    let r = ops.answered(&Op::Advertise(Some(ADV)), 0x0C, 0);
    assert_eq!(r, Err(Failure { kind: FailureKind::ModeExclusion, native: 0x0C }));
    assert_eq!(ops.ended(), None);
    assert_eq!(ops.answered(&Op::Advertise(None), 0, 0), Ok(OpId(0)), "the refused one used no id");
}

#[test]
fn the_chips_refusals_read_as_the_core_reads_them() {
    for (status, kind) in [
        (0x0C, FailureKind::ModeExclusion),
        (0x46, FailureKind::ModeExclusion),
        (0x82, FailureKind::Schedule(SchedKind::NoSlot)),
        (0x84, FailureKind::Schedule(SchedKind::NewIntervalFailed)),
        (0x85, FailureKind::Schedule(SchedKind::IntervalTooLarge)),
        (0x86, FailureKind::Schedule(SchedKind::LengthFailed)),
        (0x64, FailureKind::NoResources),
        (0x02, FailureKind::NotConnected),
        (0x3E, FailureKind::NotEstablished),
        (0x41, FailureKind::Unknown),
    ] {
        assert_eq!(refusal(status), Failure { kind, native: u16::from(status) }, "{status:02x}");
    }
    assert_eq!(unanswered(Unanswered::NoReply).kind, FailureKind::Timeout);
    assert_eq!(unanswered(Unanswered::NotTaken).kind, FailureKind::Busy);
    assert_eq!(unanswered(Unanswered::Restarted).kind, FailureKind::Busy);
    assert_eq!(unanswered(Unanswered::TooLong).kind, FailureKind::Unknown);
}

#[test]
fn an_operation_ended_by_an_event_waits_for_it_and_no_other() {
    let mut ops = Ops::default();
    let params = LinkParams { interval: Interval::from_u16(24), latency: 0, timeout: Timeout::from_u16(500) };
    ops.answered(&ask(link(1)), 0, 0).unwrap();
    ops.answered(&Op::RequestSecurity(link(1)), 0, 0).unwrap();
    ops.answered(&Op::Disconnect(link(2), DisconnectReason::RemoteUserTerminatedConn), 0, 0).unwrap();
    ops.saw(&RadioEvent::Params { link: link(2), result: Ok(params) });
    ops.saw(&RadioEvent::Params { link: link(1), result: Ok(params) });
    ops.saw(&RadioEvent::Pairing(PairingEvent { link: link(1), step: PairingStep::Numeric(123_456) }));
    ops.saw(&RadioEvent::LinkDown { link: link(2), reason: Status::new(0x16) });
    assert_eq!((ops.ended(), ops.due()), (None, None), "each ended by its own event, which is its report");
}

#[test]
fn a_link_going_down_fails_what_waited_on_it() {
    let mut ops = Ops::default();
    ops.answered(&ask(link(1)), 0, 0).unwrap();
    ops.answered(&ask(link(2)), 0, 0).unwrap();
    ops.saw(&RadioEvent::LinkDown { link: link(1), reason: Status::new(0x13) });
    assert_eq!(ops.ended(), Some(failed(0, FailureKind::NotConnected)));
    assert_eq!(ops.ended(), None);
    assert!(ops.due().is_some(), "link 2's ask still waits");
}

#[test]
fn the_central_taking_the_ask_ends_it() {
    let mut ops = Ops::default();
    ops.answered(&ask(link(1)), 0, 0).unwrap();
    ops.accepted(link(2));
    assert_eq!(ops.ended(), None);
    ops.accepted(link(1));
    assert_eq!(ops.ended(), Some(done(0)));
}

#[test]
fn a_chip_that_restarts_holds_nothing() {
    for event in [RadioEvent::ChipReset(ResetCause::Watchdog), RadioEvent::Ready(INFO)] {
        let mut ops = Ops::default();
        ops.answered(&ask(link(1)), 0, 0).unwrap();
        ops.saw(&event);
        assert_eq!((ops.due(), ops.ended()), (None, None), "{event:?}");
    }
}

const INFO: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0x30,
    firmware: 2353,
};

#[test]
fn an_operation_past_its_deadline_times_out() {
    let mut ops = Ops::default();
    ops.answered(&ask(link(1)), 0, 5_000).unwrap();
    assert_eq!(ops.due(), Some(5_000));
    ops.expire(4_999);
    assert_eq!(ops.ended(), None);
    ops.expire(5_000);
    assert_eq!(ops.ended(), Some(failed(0, FailureKind::Timeout)));
}

#[test]
fn more_waiting_than_fit_is_refused() {
    let mut ops = Ops::default();
    for n in 0..MAX_PENDING {
        ops.answered(&ask(link(u16::try_from(n).unwrap())), 0, 0).unwrap();
    }
    assert_eq!(ops.answered(&ask(link(99)), 0, 0).map_err(|f| f.kind), Err(FailureKind::NoResources));
}

#[test]
fn endings_past_what_the_queue_holds_are_lost_and_said_to_be() {
    let mut ops = Ops::default();
    for _ in 0..17 {
        ops.answered(&Op::Advertise(None), 0, 0).unwrap();
    }
    assert_eq!(ops.ended(), Some(RadioEvent::EventsLost));
    assert_eq!(ops.ended(), Some(done(0)), "the oldest first");
}

#[test]
fn an_operation_taken_on_a_link_already_down_is_ended_at_once() {
    let mut ops = Ops::default();
    ops.saw(&RadioEvent::LinkDown { link: link(1), reason: Status::new(0x13) });
    assert_eq!(ops.answered(&ask(link(1)), 0, 9), Ok(OpId(0)));
    assert_eq!(ops.ended(), Some(failed(0, FailureKind::NotConnected)));
    assert_eq!(ops.answered(&Op::Disconnect(link(1), DisconnectReason::RemoteUserTerminatedConn), 0, 9), Ok(OpId(1)));
    assert_eq!(ops.ended(), Some(RadioEvent::Done(OpId(1), Ok(Outcome::Done))), "its going down came first: the hang-up is done");
    assert_eq!(ops.due(), None);
    let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([0; 6]) };
    ops.saw(&RadioEvent::LinkUp { link: link(1), role: LeConnRole::Peripheral, peer, params: None });
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    ops.saw(&RadioEvent::LinkDown { link: link(2), reason: Status::new(0x13) });
    ops.saw(&RadioEvent::EventsLost);
    ops.answered(&ask(link(2)), 0, 9).unwrap();
    assert_eq!((ops.ended(), ops.due()), (None, Some(9)), "made again, the link holds an ask; after a loss, nothing is known to be down");
}

#[test]
fn a_link_stays_down_while_others_come_and_go() {
    let mut ops = Ops::default();
    let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([0; 6]) };
    ops.saw(&RadioEvent::LinkDown { link: link(1), reason: Status::new(0x13) });
    for _ in 0..9 {
        ops.saw(&RadioEvent::LinkUp { link: link(2), role: LeConnRole::Peripheral, peer, params: None });
        ops.saw(&RadioEvent::LinkDown { link: link(2), reason: Status::new(0x13) });
    }
    for n in 3..9 {
        ops.saw(&RadioEvent::LinkDown { link: link(n), reason: Status::new(0x13) });
    }
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    assert_eq!(ops.ended(), Some(failed(0, FailureKind::NotConnected)), "eight links down fill the record; link 1 is still in it");
    ops.saw(&RadioEvent::LinkDown { link: link(10), reason: Status::new(0x13) });
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    assert_eq!((ops.ended(), ops.due()), (None, Some(9)), "a ninth pushes out the oldest");
}

#[test]
fn the_central_taking_an_ask_ends_the_oldest_on_its_link() {
    let mut ops = Ops::default();
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    ops.accepted(link(1));
    assert_eq!(ops.ended(), Some(RadioEvent::Done(OpId(0), Ok(Outcome::Done))));
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    ops.accepted(link(1));
    assert_eq!(ops.ended(), Some(RadioEvent::Done(OpId(1), Ok(Outcome::Done))), "not the newer one in the freed slot");
}

#[test]
fn the_down_record_holds_each_link_once_and_lets_the_oldest_go_first() {
    let mut ops = Ops::default();
    let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([0; 6]) };
    let down = |ops: &mut Ops, n| ops.saw(&RadioEvent::LinkDown { link: link(n), reason: Status::new(0x13) });
    let up = |ops: &mut Ops, n| ops.saw(&RadioEvent::LinkUp { link: link(n), role: LeConnRole::Peripheral, peer, params: None });
    down(&mut ops, 1);
    down(&mut ops, 1);
    up(&mut ops, 1);
    ops.answered(&ask(link(1)), 0, 9).unwrap();
    assert_eq!(ops.ended(), None, "down twice and up once is up");
    for n in 2..10 {
        down(&mut ops, n);
    }
    up(&mut ops, 2);
    down(&mut ops, 10);
    down(&mut ops, 11);
    ops.answered(&ask(link(10)), 0, 9).unwrap();
    assert_eq!(ops.ended(), Some(failed(1, FailureKind::NotConnected)), "the newest stays");
    ops.answered(&ask(link(3)), 0, 9).unwrap();
    assert_eq!(ops.ended(), None, "the oldest went");
}
