//! The core's operations as BlueNRG-2 commands, held to the commands the watch's driver sent in
//! every capture (`crates/wrist-ble-sim/fixtures/traces`).

use std::collections::BTreeMap;
use std::path::Path;

use wrist_ble::{AdvParams, CharDecl, ConnHandle, ConnParams, DisconnectReason, Interval, LeConnRole, Op, Payload, ServerAttr, ServiceDecl, Slots, Timeout};
use wrist_bluenrg2::commands::{command, MAX_PARAMS};
use wrist_bluenrg2::gatt::Handles;
use wrist_wake::ble::server::{device_information, services, TX};

const NAME: &[u8] = b"UNA";

fn u16_at(p: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([p[i], p[i + 1]])
}

/// The operation a driver command carries out, read from its parameters; `None` for one the core
/// does not ask for (a scan, which the console starts).
fn op_of(opcode: u16, p: &[u8]) -> Option<Op> {
    let link = || ConnHandle::new(u16_at(p, 0));
    Some(match opcode {
        0xFC83 => {
            assert_eq!(&p[9..9 + NAME.len()], NAME);
            Op::Advertise(Some(AdvParams {
                interval_min: Slots::from_u16(u16_at(p, 1)),
                interval_max: Slots::from_u16(u16_at(p, 3)),
                connectable: true,
            }))
        }
        0xFC81 => Op::Advertise(None),
        0xFD81 => Op::UpdateParams(
            link(),
            ConnParams {
                interval_min: Interval::from_u16(u16_at(p, 2)),
                interval_max: Interval::from_u16(u16_at(p, 4)),
                latency: u16_at(p, 6),
                timeout: Timeout::from_u16(u16_at(p, 8)),
                ce_len: Slots::from_u16(0),
            },
        ),
        0xFC8D => Op::RequestSecurity(link()),
        0xFC95 => Op::AllowRebond(link()),
        0xFCA5 => Op::ConfirmNumeric(link(), p[2] != 0),
        0xFC88 => Op::Passkey(link(), u32::from_le_bytes([p[2], p[3], p[4], p[5]])),
        0xFC93 => {
            assert_eq!(p[2], 0x13, "the driver only ever hangs up as the user");
            Op::Disconnect(link(), DisconnectReason::RemoteUserTerminatedConn)
        }
        0xFC97 | 0xFC9D => return None,
        _ => panic!("a command the fixture tool does not keep: {opcode:04X}"),
    })
}

#[test]
fn every_command_the_driver_sent_is_what_its_operation_encodes_to() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wrist-ble-sim/fixtures/traces");
    let mut checked: BTreeMap<u16, usize> = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.get(3) != Some(&"c") {
                continue;
            }
            let packet: Vec<u8> = (0..f[4].len() / 2).map(|i| u8::from_str_radix(&f[4][2 * i..2 * i + 2], 16).unwrap()).collect();
            let (opcode, params) = (u16_at(&packet, 1), &packet[4..]);
            assert_eq!(usize::from(packet[3]), params.len(), "{}: {line}", path.display());
            let Some(op) = op_of(opcode, params) else { continue };
            let c = command(&op, NAME, |_| Some(LeConnRole::Peripheral), None).unwrap_or_else(|| panic!("{}: nothing for {op:?}", path.display()));
            assert_eq!((c.opcode, c.params()), (opcode, params), "{}: {op:?}", path.display());
            *checked.entry(opcode).or_default() += 1;
        }
    }
    let kinds: Vec<u16> = checked.keys().copied().collect();
    assert_eq!(kinds, [0xFC81, 0xFC83, 0xFC8D, 0xFC93, 0xFC95, 0xFCA5, 0xFD81], "{checked:?}");
}

#[test]
fn advertising_gives_its_interval_min_then_max() {
    let range = AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(1636), connectable: true };
    let c = command(&Op::Advertise(Some(range)), NAME, |_| Some(LeConnRole::Peripheral), None).unwrap();
    assert_eq!(c.params()[..5], [0x00, 0xA0, 0x00, 0x64, 0x06]);
}

#[test]
fn a_passkey_is_the_link_then_the_key() {
    let c = command(&Op::Passkey(ConnHandle::new(0x0801), 123_456), NAME, |_| Some(LeConnRole::Peripheral), None).unwrap();
    assert_eq!((c.opcode, c.params()), (0xFC88, &[0x01, 0x08, 0x40, 0xE2, 0x01, 0x00][..]));
}

#[test]
fn a_disconnect_says_why() {
    let c = command(&Op::Disconnect(ConnHandle::new(0x0801), DisconnectReason::AuthenticationFailure), NAME, |_| Some(LeConnRole::Peripheral), None).unwrap();
    assert_eq!(c.params(), [0x01, 0x08, 0x05]);
}

#[test]
fn what_no_command_here_carries_out_is_none() {
    let link = ConnHandle::new(0x0801);
    let params = ConnParams {
        interval_min: Interval::from_u16(12),
        interval_max: Interval::from_u16(24),
        latency: 0,
        timeout: Timeout::from_u16(500),
        ce_len: Slots::from_u16(0),
    };
    let quiet = AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(160), connectable: false };
    assert_eq!(command(&Op::Advertise(Some(quiet)), NAME, |_| Some(LeConnRole::Peripheral), None), None);
    assert_eq!(command(&Op::UpdateParams(link, params), NAME, |_| Some(LeConnRole::Central), None), None);
    assert_eq!(command(&Op::UpdateParams(link, params), NAME, |_| None, None), None);
    assert_eq!(command(&Op::Scan(None), NAME, |_| Some(LeConnRole::Peripheral), None), None);
    assert_eq!(command(&Op::Disconnect(link, DisconnectReason::PairingWithUnitKeyNotSupported), NAME, |_| None, None), None);
    let long = [b'x'; 32];
    let open = AdvParams { connectable: true, ..quiet };
    assert_eq!(command(&Op::Advertise(Some(open)), &long, |_| Some(LeConnRole::Peripheral), None), None);
    // The advertisement holds 23 bytes of name beside the chip's flags and TX power (PM0257 Table 43).
    let c = command(&Op::Advertise(Some(open)), &long[..23], |_| Some(LeConnRole::Peripheral), None).unwrap();
    assert_eq!(&c.params()[9..9 + 23], &long[..23]);
    assert_eq!(command(&Op::Advertise(Some(open)), &long[..24], |_| Some(LeConnRole::Peripheral), None), None);
}


static DIS: [CharDecl; 3] = device_information(b"c629fcd8e8e0");
static SERVER: [ServiceDecl; 2] = services(&DIS);

/// The watch's server built from 0x000C: Device Information there, the console at 0x0013 with RX
/// declared at 0x0014 and TX at 0x0016.
fn served() -> Handles {
    let mut h = Handles::default();
    for (s, at, decls) in [(&SERVER[0], 0x000C, &[0x000D, 0x000F, 0x0011][..]), (&SERVER[1], 0x0013, &[0x0014, 0x0016][..])] {
        for (c, &decl) in s.chars.iter().zip(decls) {
            assert!(h.push(at, decl, c));
        }
    }
    h
}

#[test]
fn a_notification_sets_the_characteristics_value_as_ble_rs_did() {
    let h = served();
    let value = [b'x'; 244];
    let op = Op::Notify(ConnHandle::new(0x0801), TX, Payload::new(&value).unwrap());
    let c = command(&op, NAME, |_| Some(LeConnRole::Peripheral), Some(&h)).unwrap();
    assert_eq!((c.opcode, &c.params()[..6]), (0xFD06, &[0x13, 0x00, 0x16, 0x00, 0x00, 244][..]));
    assert_eq!(&c.params()[6..], &value[..]);
    assert_eq!(c.params().len(), MAX_PARAMS, "the longest command fills the buffer exactly");
    let empty = Op::Notify(ConnHandle::new(0x0801), TX, Payload::EMPTY);
    assert_eq!(command(&empty, NAME, |_| None, Some(&h)).unwrap().params(), [0x13, 0x00, 0x16, 0x00, 0x00, 0]);
}

#[test]
fn a_notification_needs_the_characteristic_in_the_server() {
    let op = |attr| Op::Notify(ConnHandle::new(0x0801), attr, Payload::new(b"ok").unwrap());
    assert_eq!(command(&op(TX), NAME, |_| None, None), None, "no server built");
    assert_eq!(command(&op(ServerAttr(5)), NAME, |_| None, Some(&served())), None);
}
