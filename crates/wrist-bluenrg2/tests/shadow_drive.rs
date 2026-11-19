//! The shadow driving: the core's operations handed to the driver and answered with the driver's
//! own answers, every command of the driver's set beside them.

use wrist_ble::{
    AddrKind, AdvParams, BdAddr, ChipInfo, ConnHandle, CoreSpecificationVersion, Fault, Instant, LeConnRole, LinkStatus, Op, OpId, Outcome, PeerAddr, RadioEvent,
    Slots, Snapshot, MAX_LINKS,
};
use wrist_ble::policy::chip::Action;
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::phone::{Phone, Security, ASK_TO_PAIR_AFTER};
use wrist_bluenrg2::shadow::verdict::{Act, Disagreement};
use wrist_bluenrg2::shadow::{Excuse, Mode, Observer, Shadow};
use wrist_bluenrg2::ops::refused;

#[derive(Default)]
struct Log {
    found: Vec<Disagreement>,
    excused: usize,
    snapshots: usize,
    done: Vec<RadioEvent>,
}

impl Observer for Log {
    fn disagreed(&mut self, d: Disagreement) {
        self.found.push(d);
    }

    fn excused(&mut self, _: Instant, _: Act, _: Excuse) {
        self.excused += 1;
    }

    fn snapshot(&mut self, _: &wrist_ble::Snapshot) {
        self.snapshots += 1;
    }

    fn event(&mut self, _: Instant, e: &RadioEvent) {
        if let RadioEvent::Done(..) = e {
            self.done.push(*e);
        }
    }
}

const CHIP: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0,
    firmware: 0,
};

const BOOTED: &[u8] = &[0x04, 0xFF, 0x03, 0x01, 0x00, 0x01];
/// The phone's link, 0x0801, at 45 ms, latency 0, 5 s.
const PHONE: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];
const ENCRYPTED: &[u8] = &[0x04, 0x08, 0x04, 0x00, 0x01, 0x08, 0x01];
/// The central's acceptance of the watch's ask on 0x0801.
const ACCEPTED: &[u8] = &[0x04, 0xFF, 0x06, 0x00, 0x08, 0x01, 0x08, 0x00, 0x00];
/// A packet shorter than its own length, after which the core asks for a snapshot.
const CUT: &[u8] = &[0x04, 0x3E, 0x13, 0x01];

const SECURITY_REQ: u16 = 0xFC8D;
const L2CAP_UPDATE_REQ: u16 = 0xFD81;

fn link() -> ConnHandle {
    ConnHandle::new(0x0801)
}

fn goals() -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF }
}

fn command_status(status: u8, opcode: u16) -> Vec<u8> {
    [&[0x04, 0x0F, 0x04, status, 0x01][..], &opcode.to_le_bytes()].concat()
}

/// The command `op` goes out as, its opcode and parameters, as the driver's builder writes it.
fn sent(op: &Op) -> (u16, Vec<u8>) {
    let c = wrist_bluenrg2::commands::command(op, b"UNA", |_| Some(wrist_ble::LeConnRole::Peripheral), None).expect("a command");
    (c.opcode, c.params().to_vec())
}

/// A shadow driving, its start answered by the chip's ready at 100 ms.
fn driving() -> Shadow<Log> {
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    assert!(s.set_mode(Mode::Drive));
    s.set_goals(Instant(0), goals());
    assert!(matches!(s.chip_action(), Some(Action::Start(_))));
    s.event(Instant(100), BOOTED);
    s
}

/// The next operation, sent as its command at `at` and answered with `r`; for one that ends at
/// once, the driver's report of its end.
fn carry_out(s: &mut Shadow<Log>, at: u64, r: Result<OpId, wrist_ble::Failure>) -> Op {
    let op = s.issue().expect("an operation to carry out");
    let (code, params) = sent(&op);
    s.command(Instant(at), code, &params);
    s.submitted(Instant(at), r);
    if let (Ok(id), false) = (r, op.kind().ends_with_event()) {
        s.reported(Instant(at), RadioEvent::Done(id, Ok(Outcome::Done)));
    }
    op
}

/// Advertising, then the phone's link up at 1 s, encrypted at 1.3 s: the core's ask handed out.
fn asked() -> Shadow<Log> {
    let mut s = driving();
    carry_out(&mut s, 100, Ok(OpId(1)));
    s.event(Instant(1_000), PHONE);
    s.event(Instant(1_300), ENCRYPTED);
    s
}

#[test]
fn the_cores_operation_waits_for_the_driver_and_its_answer() {
    let mut s = driving();
    assert!(s.issuing());
    s.advance(Instant(200));
    let op = s.issue().expect("advertising");
    assert!(matches!(op, Op::Advertise(Some(AdvParams { interval_min, .. })) if interval_min == Slots::from_u16(160)), "{op:?}");
    assert_eq!(s.issue(), None, "handed out once");
    s.advance(Instant(5_000));
    assert_eq!(s.issue(), None, "nothing more before its answer");
    assert_eq!(s.state().radio.advertising, None);
    s.submitted(Instant(5_000), Ok(OpId(9)));
    assert_eq!(s.state().radio.advertising, None, "not until it is done");
    s.reported(Instant(5_000), RadioEvent::Done(OpId(9), Ok(Outcome::Done)));
    assert!(s.state().radio.advertising.is_some());
    assert_eq!(s.observer.done.len(), 1);
}

#[test]
fn only_an_operations_end_is_taken_from_the_drivers_reports() {
    let mut s = driving();
    let op = carry_out(&mut s, 100, Ok(OpId(1)));
    assert!(matches!(op, Op::Advertise(Some(_))));
    s.reported(Instant(200), RadioEvent::LinkUp { link: link(), role: wrist_ble::LeConnRole::Peripheral, peer: wrist_ble::PeerAddr { kind: wrist_ble::AddrKind::RANDOM, addr: wrist_ble::BdAddr::new([0; 6]) }, params: None });
    assert_eq!(s.state().phone, Phone::Discoverable, "the chip's events come as packets");
}

#[test]
fn the_drivers_command_that_is_the_cores_agrees() {
    let mut s = asked();
    let op = carry_out(&mut s, 1_310, Ok(OpId(2)));
    assert!(matches!(op, Op::UpdateParams(l, _) if l == link()), "{op:?}");
    s.advance(Instant(10_000));
    assert_eq!(s.observer.found, []);
}

#[test]
fn an_operation_the_driver_never_sends_is_the_cores_alone() {
    let mut s = asked();
    let op = s.issue().expect("the ask");
    s.advance(Instant(2_000));
    assert_eq!(s.observer.found.len(), 1, "{:?}", s.observer.found);
    assert!(s.observer.found[0].core);
    assert_eq!(Act::of_core(&op), Some(s.observer.found[0].act));
}

/// No excuse while driving: the menu's ask on a link that fits it is a command the core didn't
/// ask for, and so is a security request on an encrypted link.
#[test]
fn a_command_the_core_did_not_ask_for_is_the_drivers_alone() {
    let mut s = asked();
    carry_out(&mut s, 1_310, Ok(OpId(2)));
    s.event(Instant(1_500), ACCEPTED);
    let (code, params) = sent(&Op::UpdateParams(link(), s.phone_want().expect("a want")));
    s.command(Instant(3_000), code, &params);
    s.command(Instant(3_000), SECURITY_REQ, &[0x01, 0x08]);
    s.advance(Instant(4_000));
    assert_eq!(s.observer.excused, 0);
    let acts: Vec<(bool, Act)> = s.observer.found.iter().map(|d| (d.core, d.act)).collect();
    assert_eq!(acts.len(), 2, "{acts:?}");
    assert!(acts.iter().all(|&(core, _)| !core), "{acts:?}");
    assert!(acts.contains(&(false, Act::RequestSecurity)), "{acts:?}");
}

/// The chip's refusal reaches the core as the driver's answer to the submission, never as a
/// second answer read from the reply packet.
#[test]
fn a_refusal_in_the_stream_is_not_a_second_answer() {
    let mut s = asked();
    let op = s.issue().expect("the ask");
    let (code, params) = sent(&op);
    s.command(Instant(1_310), code, &params);
    s.event(Instant(1_311), &command_status(0x64, L2CAP_UPDATE_REQ));
    assert!(s.state().phone.link().is_some());
    s.submitted(Instant(1_311), Ok(OpId(2)));
    s.advance(Instant(20_000));
    assert_eq!(s.issue(), None, "still on air: the chip's packet was not taken as its end");
}

/// A refusal for want of resources is asked again after the backoff.
#[test]
fn an_ask_refused_for_resources_is_asked_again() {
    let mut s = asked();
    let op = s.issue().expect("the ask");
    let (code, params) = sent(&op);
    s.command(Instant(1_310), code, &params);
    s.submitted(Instant(1_310), Err(refused(&op, 0x64)));
    s.advance(Instant(2_300));
    assert_eq!(s.issue(), None, "not within the backoff");
    s.advance(Instant(2_311));
    assert_eq!(s.issue(), Some(op));
    assert_eq!(s.counters().refused, 1);
}

/// An ask refused busy waits for the central's answer to the one before.
#[test]
fn an_ask_refused_busy_waits_for_the_centrals_answer() {
    let mut s = asked();
    let op = s.issue().expect("the ask");
    let (code, params) = sent(&op);
    s.command(Instant(1_310), code, &params);
    s.submitted(Instant(1_310), Err(refused(&op, 0x46)));
    s.advance(Instant(20_000));
    assert_eq!(s.issue(), None);
    s.event(Instant(20_100), ACCEPTED);
    assert_eq!(s.issue(), Some(op));
}

/// A security request the chip refuses for good fails the pairing at once, and the link may be
/// asked for parameters.
#[test]
fn a_security_request_refused_for_good_fails_the_pairing_at_once() {
    let mut s = driving();
    carry_out(&mut s, 100, Ok(OpId(1)));
    s.event(Instant(1_000), PHONE);
    s.advance(Instant(1_000 + u64::from(ASK_TO_PAIR_AFTER.0) + 1));
    let op = s.issue().expect("the security request");
    assert_eq!(op, Op::RequestSecurity(link()));
    let (code, params) = sent(&op);
    s.command(Instant(6_002), code, &params);
    s.submitted(Instant(6_002), Err(refused(&op, 0x40)));
    assert!(matches!(s.state().phone, Phone::Up(l) if l.security == Security::Unsecured));
    assert!(s.state().phone.may_ask_params(Instant(6_002)));
    assert!(matches!(s.issue(), Some(Op::UpdateParams(l, _)) if l == link()));
}

#[test]
fn the_cores_word_on_the_chip_is_handed_out() {
    let mut s = driving();
    assert_eq!(s.chip_action(), None, "taken once");
    carry_out(&mut s, 100, Ok(OpId(1)));
    s.radio(Instant(500), false);
    assert_eq!(s.chip_action(), Some(Action::Hold));
}

#[test]
fn the_mode_changes_only_with_the_radio_off() {
    let mut s = driving();
    assert!(!s.set_mode(Mode::Shadow));
    assert_eq!(s.mode(), Mode::Drive);
    s.radio(Instant(500), false);
    assert!(s.set_mode(Mode::Shadow));
    assert_eq!(s.mode(), Mode::Shadow);
    assert_eq!(s.chip_action(), None, "nothing of the other mode is handed out");
}

/// In the shadow's own mode nothing is handed out: each operation is taken as the driver's.
#[test]
fn a_shadow_hands_out_nothing() {
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    s.set_goals(Instant(0), goals());
    s.event(Instant(100), BOOTED);
    assert!(!s.issuing());
    assert_eq!(s.chip_action(), None);
    assert!(s.state().radio.advertising.is_some(), "answered at once");
    s.event(Instant(1_000), PHONE);
    s.event(Instant(1_300), ENCRYPTED);
    assert!(s.core_pending().is_some(), "the ask on air");
    let id = s.core_pending().map(|p| p.id).unwrap();
    s.reported(Instant(1_400), RadioEvent::Done(id, Ok(Outcome::Done)));
    assert!(s.core_pending().is_some(), "a driver's report is not the shadow's to take");
}

/// After lost events the core waits for the driver's snapshot, not the shadow's record.
#[test]
fn lost_events_are_resynced_from_the_drivers_snapshot() {
    let mut s = asked();
    carry_out(&mut s, 1_310, Ok(OpId(2)));
    s.event(Instant(2_000), CUT);
    assert!(s.snapshot_wanted());
    assert_eq!(s.observer.snapshots, 0, "the shadow's record is not the core's");
    assert!(s.take_snapshot());
    assert!(!s.snapshot_wanted(), "taken once");
    s.snapshotted(Instant(2_100), Ok(snapshot(Some(link()), false)));
    assert!(!s.snapshot_wanted());
    assert_eq!(s.observer.snapshots, 1);
    assert_eq!(s.state().phone.link(), Some(link()));
}

/// The chip's state as the driver read it: `phone` up as a peripheral link, `advertising` or not.
fn snapshot(phone: Option<ConnHandle>, advertising: bool) -> Snapshot {
    let mut links = [None; MAX_LINKS];
    links[0] = phone.map(|link| LinkStatus { link, role: LeConnRole::Peripheral, peer: PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([0; 6]) }, encrypted: Some(false) });
    Snapshot { at: Instant(0), links, advertising, scanning: false, initiating: false, anchor: None }
}

/// A phone's link lost in the driver's dropped read is found by its snapshot, so its end readvertises.
#[test]
fn a_link_lost_with_the_drivers_dropped_read_is_found_and_its_end_readvertised() {
    const DOWN: &[u8] = &[0x04, 0x05, 0x04, 0x00, 0x01, 0x08, 0x13];
    let mut s = driving();
    carry_out(&mut s, 100, Ok(OpId(1)));
    assert!(s.state().radio.advertising.is_some());
    s.reported(Instant(1_000), RadioEvent::EventsLost);
    assert!(s.take_snapshot());
    s.snapshotted(Instant(1_010), Ok(snapshot(Some(link()), false)));
    assert_eq!(s.state().radio.advertising, None);
    assert_eq!(s.state().phone.link(), Some(link()));
    s.event(Instant(5_000), DOWN);
    assert!(matches!(s.issue(), Some(Op::Advertise(Some(_)))));
}

/// Events lost while the driver takes a snapshot want another after it.
#[test]
fn a_snapshot_asked_for_while_one_is_taken_is_taken_after_it() {
    let mut s = driving();
    carry_out(&mut s, 100, Ok(OpId(1)));
    s.reported(Instant(1_000), RadioEvent::EventsLost);
    assert!(s.take_snapshot());
    s.reported(Instant(1_005), RadioEvent::EventsLost);
    s.snapshotted(Instant(1_010), Ok(snapshot(None, true)));
    assert!(s.take_snapshot());
}

/// A failed snapshot leaves the core as it was, and is asked for again.
#[test]
fn a_failed_snapshot_changes_nothing() {
    let mut s = driving();
    carry_out(&mut s, 100, Ok(OpId(1)));
    s.reported(Instant(1_000), RadioEvent::EventsLost);
    assert!(s.take_snapshot());
    s.snapshotted(Instant(1_010), Err(Fault::Silent));
    assert!(!s.snapshot_wanted());
    assert!(s.state().radio.advertising.is_some());
    assert_eq!(s.observer.snapshots, 0);
    s.advance(Instant(2_010));
    assert!(s.take_snapshot());
}

/// In the shadow's own mode the driver's lost reads are not the core's: its record is the shadow's.
#[test]
fn a_shadow_takes_no_lost_read_of_the_drivers() {
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    s.set_goals(Instant(0), goals());
    s.event(Instant(100), BOOTED);
    s.reported(Instant(1_000), RadioEvent::EventsLost);
    assert_eq!(s.observer.snapshots, 0);
    assert!(!s.snapshot_wanted());
}

/// The phone's link ends during the driver's scan, the chip refusing the menu's first interval
/// meanwhile: advertising goes back to it once the scan is over, by the chip's end or the driver's.
#[test]
fn a_refusal_during_the_drivers_scan_ends_with_it() {
    const SCAN: u16 = 0xFC97;
    const DOWN: &[u8] = &[0x04, 0x05, 0x04, 0x00, 0x01, 0x08, 0x13];
    const SCAN_DONE: &[u8] = &[0x04, 0xFF, 0x04, 0x07, 0x04, 0x02, 0x00];
    const SCAN_END: u16 = 0xFC9D;
    let interval = |op: Option<Op>| match op {
        Some(Op::Advertise(Some(a))) => Some(a.interval_min.as_u16()),
        _ => None,
    };
    for by_the_chip in [true, false] {
        let mut s = asked();
        carry_out(&mut s, 1_310, Ok(OpId(2)));
        s.command(Instant(2_000), SCAN, &[0xA0, 0x00, 0x60, 0x00, 0x01, 0x01]);
        s.event(Instant(2_001), &command_status(0x00, SCAN));
        s.event(Instant(3_000), DOWN);
        let first = s.issue();
        assert_eq!(interval(first), Some(160), "{first:?}");
        let op = first.unwrap();
        let (code, params) = sent(&op);
        s.command(Instant(3_010), code, &params);
        s.submitted(Instant(3_010), Err(refused(&op, 0x85)));
        let fallback = carry_out(&mut s, 3_020, Ok(OpId(7)));
        assert_ne!(interval(Some(fallback)), Some(160));
        s.advance(Instant(5_000));
        assert_eq!(s.issue(), None, "the fallback holds while the scan runs");
        if by_the_chip {
            s.event(Instant(13_000), SCAN_DONE);
        } else {
            s.command(Instant(13_000), SCAN_END, &[0x02]);
        }
        assert_eq!(s.issue(), Some(Op::Advertise(None)), "by the chip {by_the_chip}");
        s.submitted(Instant(13_010), Ok(OpId(8)));
        s.reported(Instant(13_010), RadioEvent::Done(OpId(8), Ok(Outcome::Done)));
        assert_eq!(interval(s.issue()), Some(160), "by the chip {by_the_chip}");
    }
}

#[test]
fn the_radio_shown_is_the_cores_record() {
    use wrist_bluenrg2::wake::ble::Radio;
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    assert!(s.set_mode(Mode::Drive));
    assert_eq!(Radio::of(s.state()), Radio::Off);
    s.set_goals(Instant(0), goals());
    assert_eq!(Radio::of(s.state()), Radio::Starting);
    s.event(Instant(100), BOOTED);
    assert_eq!(Radio::of(s.state()), Radio::Idle);
    carry_out(&mut s, 100, Ok(OpId(1)));
    assert_eq!(Radio::of(s.state()), Radio::Advertising);
    // A scan's pause: the core's own stop.
    s.set_goals(Instant(200), Goals { phone: PhoneGoal { discoverable: false, pairing_open: true }, ..goals() });
    assert_eq!(carry_out(&mut s, 200, Ok(OpId(2))), Op::Advertise(None));
    assert_eq!(Radio::of(s.state()), Radio::Idle);
    s.set_goals(Instant(300), goals());
    carry_out(&mut s, 300, Ok(OpId(3)));
    s.event(Instant(1_000), PHONE);
    assert_eq!(Radio::of(s.state()), Radio::Connected);
}
