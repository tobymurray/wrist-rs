//! A start against a scripted BlueNRG-2 that answers each command: the bring-up ble.rs sent, in its
//! order, and where each refusal ends it.

mod common;

use std::cell::Cell;

use bluenrg2::{opcode, BootReason};
use common::{driver, event, Shared};
use embassy_futures::block_on;
use wrist_ble::{CharDecl, RadioEvent, ResetCause, ServerTable, ServiceDecl};
use wrist_bluenrg2::bonds::PAIRING;
use wrist_bluenrg2::driver::{Got, Unanswered};
use wrist_bluenrg2::start::{Setup, Start, Starting, Step};
use wrist_bluenrg2::transport::MAX_READ;
use wrist_wake::ble::server::{device_information, services, RX, TX};

static DIS: [CharDecl; 3] = device_information(b"c629fcd8e8e0");
static SERVICES: [ServiceDecl; 2] = services(&DIS);
static SERVER: ServerTable = ServerTable { services: &SERVICES };

const ADDRESS: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0xC6];
const EVENT_MASK: [u8; 8] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xBF, 0x3D];

fn setup() -> Setup<'static> {
    Setup { address: ADDRESS, tx_power: [0x01, 0x04], event_mask: EVENT_MASK, name: b"UNA", name_room: 7, io_capability: 0x01, pairing: PAIRING }
}

thread_local! {
    /// The next handle the chip gives a service or characteristic.
    static NEXT: Cell<u16> = const { Cell::new(0x000C) };
    /// An opcode the chip refuses with a status at its `n`th sending, counting from 0, and the
    /// sendings so far.
    static REFUSE: Cell<Option<(u16, u8, usize)>> = const { Cell::new(None) };
    static SENT: Cell<usize> = const { Cell::new(0) };
    /// An opcode the chip answers by restarting.
    static RESTART_ON: Cell<Option<u16>> = const { Cell::new(None) };
}

fn complete(opcode: u16, status: u8, params: &[u8]) -> Vec<u8> {
    event(0x0E, &[&[0x01][..], &opcode.to_le_bytes(), &[status], params].concat())
}

fn boot(reason: u8) -> Vec<u8> {
    event(0xFF, &[0x01, 0x00, reason])
}

/// The chip's answer to one command.
fn answer(packet: &[u8]) -> Vec<Vec<u8>> {
    let opcode = u16::from_le_bytes([packet[1], packet[2]]);
    if RESTART_ON.with(Cell::get) == Some(opcode) {
        return vec![boot(0x05)];
    }
    let status = match REFUSE.with(Cell::get) {
        Some((op, status, nth)) if op == opcode => {
            let n = SENT.with(|s| s.replace(s.get() + 1));
            if n == nth { status } else { 0 }
        }
        _ => 0,
    };
    let params = match opcode {
        opcode::ACI_GAP_INIT => vec![0x05, 0x00, 0x06, 0x00, 0x08, 0x00],
        opcode::ACI_GATT_ADD_SERVICE | opcode::ACI_GATT_ADD_CHAR if status == 0 => NEXT.with(|n| {
            let h = n.get();
            n.set(h + if opcode == opcode::ACI_GATT_ADD_SERVICE { 1 } else { 3 });
            h.to_le_bytes().to_vec()
        }),
        _ => Vec::new(),
    };
    vec![complete(opcode, status, &params)]
}

/// Every note a start hands out, up to its end, but the commands and their replies.
fn start(c: &Shared, d: &mut common::TestDriver) -> Vec<Starting> {
    if c.borrow().answer.is_none() {
        c.borrow_mut().answer = Some(answer);
    }
    let mut s = Start::new(&SERVER, setup());
    let mut out = [0u8; MAX_READ];
    std::iter::from_fn(|| block_on(d.starting(&mut s, &mut out))).filter(|n| !matches!(n, Starting::Got(Got::Packet { event: None, .. }) | Starting::Sending(_))).collect()
}

/// The commands written, as opcode and parameters.
fn written(c: &Shared) -> Vec<(u16, Vec<u8>)> {
    c.borrow().spans.iter().filter(|s| s[0] == 0x0A && s.len() > 5).map(|s| (u16::from_le_bytes([s[6], s[7]]), s[9..].to_vec())).collect()
}

/// A chip that boots when let out of reset, nothing refused yet.
fn booted() -> (Shared, common::TestDriver) {
    NEXT.with(|n| n.set(0x000C));
    REFUSE.with(|r| r.set(None));
    SENT.with(|s| s.set(0));
    RESTART_ON.with(|r| r.set(None));
    let (c, d) = driver(128);
    c.borrow_mut().boot = Some(boot(0x01));
    (c, d)
}

#[test]
fn a_start_sends_the_bring_up_ble_rs_sent_and_builds_the_server() {
    let (c, mut d) = booted();
    let notes = start(&c, &mut d);
    let Some(Starting::Up(handles)) = notes.last() else { panic!("{notes:?}") };
    assert_eq!(notes[..3], [Starting::Got(Got::Boot { len: 6, reason: Some(BootReason::STARTED) }), Starting::Booted, Starting::Roles { central: true }]);
    assert_eq!(notes.len(), 4, "{notes:?}");
    let w = written(&c);
    let address = |slot| [&[slot, 6][..], &ADDRESS].concat();
    let bring_up = [
        (opcode::ACI_HAL_WRITE_CONFIG_DATA, address(0x2E)),
        (opcode::ACI_HAL_WRITE_CONFIG_DATA, address(0x00)),
        (opcode::ACI_HAL_SET_TX_POWER_LEVEL, vec![0x01, 0x04]),
        (opcode::HCI_SET_EVENT_MASK, EVENT_MASK.to_vec()),
        (opcode::ACI_GATT_INIT, vec![]),
        (opcode::ACI_GAP_INIT, vec![0x05, 0x00, 7]),
        (opcode::ACI_GATT_UPDATE_CHAR_VALUE, vec![0x05, 0x00, 0x06, 0x00, 0x00, 3, b'U', b'N', b'A']),
        (opcode::ACI_GAP_SET_IO_CAPABILITY, vec![0x01]),
        (opcode::ACI_GAP_SET_AUTHENTICATION_REQUIREMENT, PAIRING.params().to_vec()),
    ];
    assert_eq!(w[..bring_up.len()], bring_up);
    let server: Vec<u16> = w[bring_up.len()..].iter().map(|&(op, _)| op).collect();
    let (add, char, value) = (opcode::ACI_GATT_ADD_SERVICE, opcode::ACI_GATT_ADD_CHAR, opcode::ACI_GATT_UPDATE_CHAR_VALUE);
    assert_eq!(server, [add, char, value, char, value, char, value, add, char, char], "the server as ble.rs built it");
    assert_eq!((handles.get(RX), handles.get(TX)), (Some((0x0016, 0x0017)), Some((0x0016, 0x001A))));
}

#[test]
fn a_chip_refusing_the_central_role_is_asked_for_the_peripheral_alone() {
    let (c, mut d) = booted();
    REFUSE.with(|r| r.set(Some((opcode::ACI_GAP_INIT, 0x12, 0))));
    let notes = start(&c, &mut d);
    assert_eq!(notes[2..4], [Starting::GapRefused { roles: 0x05, status: 0x12 }, Starting::Roles { central: false }]);
    assert!(matches!(notes.last(), Some(Starting::Up(_))));
    let gap: Vec<Vec<u8>> = written(&c).into_iter().filter(|&(op, _)| op == opcode::ACI_GAP_INIT).map(|(_, p)| p).collect();
    assert_eq!(gap, [vec![0x05, 0x00, 7], vec![0x01, 0x00, 7]]);
}

#[test]
fn a_chip_refusing_every_role_stops_the_start_at_gap_init() {
    let (c, mut d) = booted();
    c.borrow_mut().answer = Some(|p| {
        let opcode = u16::from_le_bytes([p[1], p[2]]);
        vec![complete(opcode, if opcode == opcode::ACI_GAP_INIT { 0x12 } else { 0 }, &[])]
    });
    let notes = start(&c, &mut d);
    assert_eq!(
        notes[2..],
        [Starting::GapRefused { roles: 0x05, status: 0x12 }, Starting::GapRefused { roles: 0x01, status: 0x12 }, Starting::Failed(Step::GapInit)]
    );
}

#[test]
fn a_refused_step_ends_the_start_there() {
    let (c, mut d) = booted();
    REFUSE.with(|r| r.set(Some((opcode::ACI_HAL_SET_TX_POWER_LEVEL, 0x12, 0))));
    let notes = start(&c, &mut d);
    assert_eq!(notes.last(), Some(&Starting::Failed(Step::TxPower)));
    assert_eq!(written(&c).len(), 3, "nothing after the refused step");
}

#[test]
fn a_silent_chip_is_said_to_be_and_fails_at_the_first_step() {
    let (c, mut d) = booted();
    c.borrow_mut().boot = None;
    c.borrow_mut().never_ready = true;
    let notes = start(&c, &mut d);
    assert_eq!(notes, [Starting::Silent, Starting::Booted, Starting::Failed(Step::StaticRandomAddress)]);
}

#[test]
fn a_restart_during_the_start_refuses_what_follows_as_ble_rs_did() {
    let (c, mut d) = booted();
    RESTART_ON.with(|r| r.set(Some(opcode::ACI_GAP_INIT)));
    let notes = start(&c, &mut d);
    assert_eq!(
        notes[2..],
        [
            Starting::Got(Got::Packet { len: 6, event: Some(RadioEvent::ChipReset(ResetCause::Watchdog)) }),
            Starting::GapUnanswered { why: Unanswered::NoReply },
            Starting::GapUnanswered { why: Unanswered::Restarted },
            Starting::Failed(Step::GapInit),
        ]
    );
    let gaps = written(&c).iter().filter(|&&(op, _)| op == opcode::ACI_GAP_INIT).count();
    assert_eq!(gaps, 1, "the second ask is never sent");
}

#[test]
fn a_refused_characteristic_names_its_service() {
    let (c, mut d) = booted();
    REFUSE.with(|r| r.set(Some((opcode::ACI_GATT_ADD_CHAR, 0x1F, 3))));
    let notes = start(&c, &mut d);
    assert_eq!(notes.last(), Some(&Starting::Failed(Step::Characteristic("console"))));
}

#[test]
fn each_command_is_handed_out_before_it_is_sent_and_none_the_restart_refused() {
    let (c, mut d) = booted();
    RESTART_ON.with(|r| r.set(Some(opcode::ACI_GAP_INIT)));
    c.borrow_mut().answer = Some(answer);
    let mut s = Start::new(&SERVER, setup());
    let mut out = [0u8; MAX_READ];
    let notes: Vec<Starting> = std::iter::from_fn(|| block_on(d.starting(&mut s, &mut out))).collect();
    let sent: Vec<(u16, Vec<u8>)> = notes.iter().filter_map(|n| match n { Starting::Sending(c) => Some((c.opcode, c.params().to_vec())), _ => None }).collect();
    assert_eq!(sent, written(&c), "one note a command, the refused GAP init's second ask none");
}
