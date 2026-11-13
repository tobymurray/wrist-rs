//! A snapshot against a scripted BlueNRG-2: the links, the anchor and each link's security as the
//! chip reads them, and where a refusal or a silence ends it.

mod common;

use std::cell::RefCell;

use bluenrg2::hal::link_state;
use bluenrg2::opcode::{ACI_GAP_GET_SECURITY_LEVEL, ACI_HAL_GET_ANCHOR_PERIOD, ACI_HAL_GET_LINK_STATUS};
use common::{driver, event, Shared, TestDriver};
use embassy_futures::block_on;
use wrist_ble::{
    AddrKind, Anchor, BdAddr, ConnHandle, ConnParams, Failure, FailureKind, Fault, Instant, Interval, LeConnRole, LinkStatus, Micros, Op, OpId,
    PeerAddr, RadioEvent, Slots, Snapshot, Timeout, MAX_LINKS,
};
use wrist_bluenrg2::driver::{Got, Reply};
use wrist_bluenrg2::snapshot::{Snap, Snapping, UNSEEN};
use wrist_bluenrg2::transport::MAX_READ;

const PEER: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

/// What the chip answers: a status and return parameters for each read, `None` for no answer.
#[derive(Clone, Default)]
struct Script {
    links: Option<(u8, Vec<u8>)>,
    anchor: Option<(u8, Vec<u8>)>,
    /// Each link's security mode and level by handle; any other handle is refused.
    security: Vec<(u16, u8, u8)>,
    /// A packet the chip sends ahead of each answer.
    ahead: Option<Vec<u8>>,
    /// The command whose answer the chip follows by restarting.
    restart_after: Option<u16>,
}

thread_local! {
    static SCRIPT: RefCell<Script> = RefCell::new(Script::default());
}

fn complete(opcode: u16, status: u8, params: &[u8]) -> Vec<u8> {
    event(0x0E, &[&[0x01][..], &opcode.to_le_bytes(), &[status], params].concat())
}

fn answer(packet: &[u8]) -> Vec<Vec<u8>> {
    let opcode = u16::from_le_bytes([packet[1], packet[2]]);
    SCRIPT.with_borrow(|s| {
        let reply = match opcode {
            ACI_HAL_GET_LINK_STATUS => s.links.clone(),
            ACI_HAL_GET_ANCHOR_PERIOD => s.anchor.clone(),
            ACI_GAP_GET_SECURITY_LEVEL => {
                let handle = u16::from_le_bytes([packet[4], packet[5]]);
                Some(s.security.iter().find(|x| x.0 == handle).map_or((0x12, vec![]), |&(_, mode, level)| (0x00, vec![mode, level])))
            }
            _ => None,
        };
        let boot = (s.restart_after == Some(opcode)).then(|| event(0xFF, &[0x01, 0x00, 0x01]));
        s.ahead.iter().cloned().chain(reply.map(|(status, p)| complete(opcode, status, &p))).chain(boot).collect()
    })
}

/// `ACI_HAL_GET_LINK_STATUS`'s return parameters, the slots given each with a state and handle.
fn link_status(slots: &[(u8, u16)]) -> Vec<u8> {
    let mut states = [0u8; 8];
    let mut handles = [0xFFu8; 16];
    for (i, &(state, handle)) in slots.iter().enumerate() {
        states[i] = state;
        handles[2 * i..2 * i + 2].copy_from_slice(&handle.to_le_bytes());
    }
    [&states[..], &handles[..]].concat()
}

fn anchor_period(period: u32, free: u32) -> Vec<u8> {
    [period.to_le_bytes(), free.to_le_bytes()].concat()
}

/// LE Connection Complete: `handle` connected in `role` (0 central, 1 peripheral) to [`PEER`].
fn connection(handle: u16, role: u8) -> Vec<u8> {
    let h = handle.to_le_bytes();
    event(0x3E, &[&[0x01, 0x00, h[0], h[1], role, 0x01][..], &PEER, &[24, 0, 0, 0, 0xF4, 0x01, 0x00]].concat())
}

fn disconnection(handle: u16) -> Vec<u8> {
    let h = handle.to_le_bytes();
    event(0x05, &[0x00, h[0], h[1], 0x13])
}

fn peer() -> PeerAddr {
    PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new(PEER) }
}

fn link(handle: u16, role: LeConnRole, peer: PeerAddr, encrypted: Option<bool>) -> LinkStatus {
    LinkStatus { link: ConnHandle::new(handle), role, peer, encrypted }
}

/// A chip answering as `script` says.
fn scripted(script: Script) -> (Shared, TestDriver) {
    SCRIPT.set(script);
    let (c, d) = driver(128);
    c.borrow_mut().answer = Some(answer);
    (c, d)
}

/// What a drain hands out of `packets`, the chip's peers as the driver learns them.
fn drain(c: &Shared, d: &mut TestDriver, packets: &[Vec<u8>]) {
    c.borrow_mut().queued.extend(packets.iter().cloned());
    d.drain();
    let mut out = [0u8; MAX_READ];
    while block_on(d.next(&mut out)).is_some() {}
}

/// A whole snapshot: every note before its end, the end, and what is handed out after it.
fn snapshot(d: &mut TestDriver) -> (Vec<Snapping>, Result<Snapshot, Fault>, Vec<Snapping>) {
    let mut s = Snap::default();
    let mut out = [0u8; MAX_READ];
    let (mut notes, mut taken) = (Vec::new(), None);
    for _ in 0..100 {
        match block_on(d.snapshotting(&mut s, &mut out)) {
            Some(Snapping::Taken(r)) => {
                assert!(taken.is_none(), "taken twice");
                taken = Some((std::mem::take(&mut notes), r));
            }
            Some(note) => notes.push(note),
            None => {
                let (before, r) = taken.expect("over without a snapshot");
                return (before, r, notes);
            }
        }
    }
    panic!("the snapshot never ended");
}

/// The commands a snapshot's notes say it sent: each opcode and its parameters.
fn sent(notes: &[Snapping]) -> Vec<(u16, Vec<u8>)> {
    notes
        .iter()
        .filter_map(|n| match n {
            Snapping::Sending(c) => Some((c.opcode, c.params().to_vec())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_snapshot_reads_the_links_the_anchor_and_each_links_security_in_turn() {
    let (c, mut d) = scripted(Script {
        links: Some((0x00, link_status(&[(link_state::ADVERTISING, 0), (link_state::PERIPHERAL, 0x0801), (link_state::CENTRAL, 0x0802)]))),
        anchor: Some((0x00, anchor_period(168, 139))),
        security: vec![(0x0801, 1, 2), (0x0802, 1, 1)],
        ..Script::default()
    });
    drain(&c, &mut d, &[connection(0x0801, 0x01)]);
    c.borrow_mut().now = 5_000_000;
    let (notes, r, _) = snapshot(&mut d);
    let security = |h: u16| (ACI_GAP_GET_SECURITY_LEVEL, h.to_le_bytes().to_vec());
    assert_eq!(sent(&notes), [(ACI_HAL_GET_LINK_STATUS, vec![]), (ACI_HAL_GET_ANCHOR_PERIOD, vec![]), security(0x0801), security(0x0802)]);
    let spans = c.borrow().spans.iter().filter(|s| s.len() > 5 && s[0] == 0x0A).map(|s| s[5..].to_vec()).collect::<Vec<_>>();
    assert_eq!(spans, [vec![0x01, 0x17, 0xFC, 0x00], vec![0x01, 0x19, 0xFC, 0x00], vec![0x01, 0x90, 0xFC, 0x02, 0x01, 0x08], vec![0x01, 0x90, 0xFC, 0x02, 0x02, 0x08]]);
    let mut links = [None; MAX_LINKS];
    links[1] = Some(link(0x0801, LeConnRole::Peripheral, peer(), Some(true)));
    links[2] = Some(link(0x0802, LeConnRole::Central, UNSEEN, Some(false)));
    let anchor = Some(Anchor { period: Micros(105_000), max_free_slot: Micros(86_875) });
    assert_eq!(r, Ok(Snapshot { at: Instant(5_000), links, advertising: true, scanning: false, initiating: false, anchor }));
}

#[test]
fn each_slots_state_says_what_it_holds() {
    let slots = [
        (link_state::SCANNING, 0),
        (link_state::INITIATING, 0),
        (link_state::PERIPHERAL, 0x0F00),
        (0x00, 0x0803),
        (0x06, 0x0804),
        (link_state::CENTRAL, 0x0EFF),
    ];
    let (_, mut d) = scripted(Script { links: Some((0x00, link_status(&slots))), anchor: Some((0x00, anchor_period(0, 0))), ..Script::default() });
    let s = snapshot(&mut d).1.unwrap();
    let mut links = [None; MAX_LINKS];
    links[5] = Some(link(0x0EFF, LeConnRole::Central, UNSEEN, None));
    assert_eq!(s.links, links, "a handle past 0x0EFF, an idle slot and a test mode hold no link");
    assert_eq!((s.advertising, s.scanning, s.initiating), (false, true, true));
    assert_eq!(s.anchor, None, "0 for both is no anchor");
}

#[test]
fn a_link_seen_to_go_down_or_reset_away_has_no_peer_the_driver_knows() {
    let script = Script {
        links: Some((0x00, link_status(&[(link_state::PERIPHERAL, 0x0801), (link_state::PERIPHERAL, 0x0802)]))),
        anchor: Some((0x0C, vec![])),
        ..Script::default()
    };
    let (c, mut d) = scripted(script.clone());
    drain(&c, &mut d, &[connection(0x0801, 0x01), connection(0x0802, 0x01), disconnection(0x0801)]);
    let s = snapshot(&mut d).1.unwrap();
    assert_eq!(s.links[..2], [Some(link(0x0801, LeConnRole::Peripheral, UNSEEN, None)), Some(link(0x0802, LeConnRole::Peripheral, peer(), None))]);
    assert_eq!(s.anchor, None, "a refused anchor reading is none");
    let (c, mut d) = scripted(script);
    drain(&c, &mut d, &[connection(0x0801, 0x01)]);
    c.borrow_mut().boot = Some(event(0xFF, &[0x01, 0x00, 0x01]));
    block_on(d.reset());
    assert!(block_on(d.boot()));
    drain(&c, &mut d, &[]);
    assert_eq!(snapshot(&mut d).1.unwrap().links[0], Some(link(0x0801, LeConnRole::Peripheral, UNSEEN, None)));
}

#[test]
fn a_link_is_encrypted_at_security_level_2_or_above_in_mode_1() {
    let handles = [0x0801, 0x0802, 0x0803, 0x0804, 0x0805];
    let (_, mut d) = scripted(Script {
        links: Some((0x00, link_status(&handles.map(|h| (link_state::PERIPHERAL, h))))),
        anchor: Some((0x00, anchor_period(0, 0))),
        security: vec![(0x0801, 1, 1), (0x0802, 1, 2), (0x0803, 1, 4), (0x0804, 2, 2)],
        ..Script::default()
    });
    let encrypted = snapshot(&mut d).1.unwrap().links.map(|l| l.and_then(|l| l.encrypted));
    assert_eq!(encrypted[..5], [Some(false), Some(true), Some(true), Some(false), None], "the last refused");
}

#[test]
fn what_comes_meanwhile_is_handed_out_ahead_of_the_answer_it_came_with() {
    let (_, mut d) = scripted(Script {
        links: Some((0x00, link_status(&[]))),
        anchor: Some((0x00, anchor_period(0, 0))),
        ahead: Some(disconnection(0x0807)),
        ..Script::default()
    });
    let (notes, r, _) = snapshot(&mut d);
    let kinds: Vec<&str> = notes
        .iter()
        .map(|n| match n {
            Snapping::Sending(_) => "sending",
            Snapping::Got(Got::Packet { event: Some(RadioEvent::LinkDown { .. }), .. }) => "down",
            Snapping::Got(Got::Packet { event: None, .. }) => "reply",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["sending", "down", "reply", "sending", "down", "reply"]);
    assert!(r.is_ok());
}

#[test]
fn a_snapshot_fails_where_the_link_status_or_any_answer_does() {
    let refused = |native: u16| Err(Fault::Refused(Failure { kind: FailureKind::Unknown, native }));
    let (_, mut d) = scripted(Script { links: Some((0x12, vec![])), ..Script::default() });
    assert_eq!(snapshot(&mut d).1, refused(0x12));
    let (_, mut d) = scripted(Script { links: Some((0x00, vec![0; 23])), ..Script::default() });
    assert_eq!(snapshot(&mut d).1, refused(0x00), "a short reply");
    let (_, mut d) = scripted(Script::default());
    assert_eq!(snapshot(&mut d).1, Err(Fault::Silent), "no reply");
    let (_, mut d) = scripted(Script { links: Some((0x00, link_status(&[]))), ..Script::default() });
    assert_eq!(snapshot(&mut d).1, Err(Fault::Silent), "no anchor reading");
    let (c, mut d) = scripted(Script::default());
    c.borrow_mut().never_ready = true;
    assert_eq!(snapshot(&mut d).1, Err(Fault::Bus), "not taken");
}

#[test]
fn a_chip_that_restarted_is_asked_nothing() {
    let (c, mut d) = scripted(Script { links: Some((0x00, link_status(&[]))), ..Script::default() });
    drain(&c, &mut d, &[event(0xFF, &[0x01, 0x00, 0x01])]);
    let (notes, r, _) = snapshot(&mut d);
    assert_eq!(sent(&notes), []);
    assert_eq!(r, Err(Fault::Silent));
    assert!(c.borrow().spans.iter().all(|s| s.first() != Some(&0x0A)), "nothing written");
}

#[test]
fn an_operation_on_a_link_the_snapshot_lacks_ends_as_not_connected() {
    let (_, mut d) = scripted(Script {
        links: Some((0x00, link_status(&[(link_state::PERIPHERAL, 0x0802)]))),
        anchor: Some((0x00, anchor_period(0, 0))),
        security: vec![(0x0802, 1, 2)],
        ..Script::default()
    });
    let params = ConnParams { interval_min: Interval::from_u16(12), interval_max: Interval::from_u16(24), latency: 0, timeout: Timeout::from_u16(500), ce_len: Slots::from_u16(0) };
    let taken = Ok(Reply { status: 0x00, params: [0; bluenrg2::RETURN_PARAMS_MAX], len: 0 });
    assert_eq!(d.settle(&Op::UpdateParams(ConnHandle::new(0x0801), params), &taken), Ok(OpId(0)));
    assert_eq!(d.settle(&Op::UpdateParams(ConnHandle::new(0x0802), params), &taken), Ok(OpId(1)));
    let (_, r, after) = snapshot(&mut d);
    assert!(r.is_ok());
    let not_connected = Err(Failure { kind: FailureKind::NotConnected, native: 0 });
    assert_eq!(after, [Snapping::Got(Got::Event(RadioEvent::Done(OpId(0), not_connected)))]);
    assert!(d.due().is_some(), "the ask on the link the snapshot found still waits");
}

#[test]
fn an_anchor_with_no_slot_free_is_still_an_anchor() {
    let (_, mut d) = scripted(Script { links: Some((0x00, link_status(&[]))), anchor: Some((0x00, anchor_period(168, 0))), ..Script::default() });
    assert_eq!(snapshot(&mut d).1.unwrap().anchor, Some(Anchor { period: Micros(105_000), max_free_slot: Micros(0) }));
}

#[test]
fn a_chip_that_restarts_after_the_last_answer_has_no_snapshot() {
    let (_, mut d) = scripted(Script {
        links: Some((0x00, link_status(&[(link_state::PERIPHERAL, 0x0801)]))),
        anchor: Some((0x00, anchor_period(0, 0))),
        security: vec![(0x0801, 1, 2)],
        restart_after: Some(ACI_GAP_GET_SECURITY_LEVEL),
        ..Script::default()
    });
    assert_eq!(snapshot(&mut d).1, Err(Fault::Silent));
    let (_, mut d) = scripted(Script { links: Some((0x00, link_status(&[]))), anchor: Some((0x00, anchor_period(0, 0))), restart_after: Some(ACI_HAL_GET_ANCHOR_PERIOD), ..Script::default() });
    assert_eq!(snapshot(&mut d).1, Err(Fault::Silent), "no links: the anchor's answer is the last");
}
