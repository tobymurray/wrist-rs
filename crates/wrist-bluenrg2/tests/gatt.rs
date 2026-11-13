//! A declared GATT server as the chip's commands; the watch's own server in the bytes ble.rs sent
//! when it built each command by hand.

use bluenrg2::permission;
use wrist_ble::{BluetoothUuid, CharDecl, Permission, Props, ServerAttr, ServiceDecl};
use wrist_bluenrg2::commands::MAX_PARAMS;
use wrist_bluenrg2::gatt::{add_char, add_service, initial, records, set_value, Handles, Part, MAX_CHARS, MAX_VALUE};
use wrist_wake::ble::server::{device_information, services, CONSOLE, RX, TX};

static DIS: [CharDecl; 3] = device_information(b"0123456789abcdef0123456789abcdef-dirty");
static SERVER: [ServiceDecl; 2] = services(&DIS);

const NUS: [u8; 16] = [0x9E, 0xCA, 0xDC, 0x24, 0x0E, 0xE5, 0xA9, 0xE0, 0x93, 0xF3, 0xA3, 0xB5, 0x00, 0x00, 0x40, 0x6E];

fn nus(last: u8) -> Vec<u8> {
    let mut b = NUS.to_vec();
    b[12] = last;
    b
}

/// Each command in order, with the handles a chip might answer: services at 0x10 and 0x20, each
/// characteristic three after the one before.
fn commands() -> (Vec<(u16, Vec<u8>)>, Handles) {
    let mut out = Vec::new();
    let mut handles = Handles::default();
    for (s, service) in SERVER.iter().enumerate() {
        let sh = 0x10 * (u16::try_from(s).unwrap() + 1);
        let c = add_service(service).unwrap();
        out.push((c.opcode, c.params().to_vec()));
        for (i, ch) in service.chars.iter().enumerate() {
            let chh = sh + 1 + 3 * u16::try_from(i).unwrap();
            let c = add_char(sh, ch).unwrap();
            out.push((c.opcode, c.params().to_vec()));
            if let Some(c) = initial(sh, chh, ch) {
                out.push((c.opcode, c.params().to_vec()));
            }
            assert!(handles.push(sh, chh, ch));
        }
    }
    (out, handles)
}

fn string(service: u16, ch: u16, uuid: u16, value: &[u8]) -> [(u16, Vec<u8>); 2] {
    let n = value.len().min(32);
    let [u0, u1] = uuid.to_le_bytes();
    let [s0, s1] = service.to_le_bytes();
    let [c0, c1] = ch.to_le_bytes();
    let len = u8::try_from(n).unwrap();
    [
        (0xFD04, vec![s0, s1, 0x01, u0, u1, len, 0, 0x02, 0x00, 0x00, 7, 0x01]),
        (0xFD06, [&[s0, s1, c0, c1, 0, len][..], &value[..n]].concat()),
    ]
}

#[test]
fn the_server_is_built_with_the_bytes_ble_rs_sent() {
    let (got, _) = commands();
    let mut want = vec![(0xFD02, vec![0x01, 0x0A, 0x18, 0x01, 7])];
    want.extend(string(0x10, 0x11, 0x2A29, b"wrist-rs"));
    want.extend(string(0x10, 0x14, 0x2A24, b"UNA Watch"));
    want.extend(string(0x10, 0x17, 0x2A26, b"0123456789abcdef0123456789abcdef-dirty"));
    want.push((0xFD02, [&[0x02][..], &nus(0x01), &[0x01, 6]].concat()));
    // RX: write and write without response, authenticated and encrypted writes, writes reported;
    // TX: notify, open, none reported; both variable.
    want.push((0xFD04, [&[0x20, 0x00, 0x02][..], &nus(0x02), &[244, 0, 0x0C, 0x28, 0x01, 7, 0x01]].concat()));
    want.push((0xFD04, [&[0x20, 0x00, 0x02][..], &nus(0x03), &[244, 0, 0x10, 0x00, 0x00, 7, 0x01]].concat()));
    assert_eq!(got, want);
}

#[test]
fn the_console_is_found_by_its_handles() {
    let (_, h) = commands();
    assert_eq!(h.get(RX), Some((0x20, 0x21)));
    assert_eq!(h.get(TX), Some((0x20, 0x24)));
    assert_eq!(h.find(0x22), Some((RX, Part::Value)));
    assert_eq!(h.find(0x25), Some((TX, Part::Value)));
    assert_eq!(h.find(0x26), Some((TX, Part::Config)));
    assert_eq!(h.find(0x23), None, "RX has no configuration descriptor");
    assert_eq!(h.find(0x21), None, "a declaration is neither");
    assert_eq!(h.find(0x12), Some((ServerAttr(0), Part::Value)));
    assert_eq!(h.get(ServerAttr(5)), None);
}

#[test]
fn a_value_fits_one_notification_or_is_refused() {
    let full = [7u8; MAX_VALUE];
    let c = set_value(0x20, 0x24, &full).unwrap();
    assert_eq!(c.params()[..6], [0x20, 0x00, 0x24, 0x00, 0x00, 244]);
    assert_eq!(&c.params()[6..], &full[..]);
    assert_eq!(set_value(0x20, 0x24, &[7u8; MAX_VALUE + 1]), None);
    assert_eq!(c.params().len(), MAX_PARAMS, "the longest command fills the buffer exactly");
    assert_eq!(initial(0x20, 0x24, &CONSOLE.chars[0]), None, "RX starts empty");
}

fn decl(props: Props, permission: Permission) -> CharDecl {
    CharDecl { uuid: BluetoothUuid::from_u16(0x2A00), props, permission, max_len: 20, value: &[] }
}

#[test]
fn a_write_asks_for_the_permission_declared_and_a_read_only_open() {
    let security = |c: &CharDecl| add_char(1, c).map(|c| (c.params()[8], c.params()[9]));
    assert_eq!(security(&decl(Props::WRITE, Permission::Open)), Some((0x00, 0x01)));
    assert_eq!(security(&decl(Props::WRITE_WITHOUT_RESPONSE, Permission::Encrypted)), Some((0x20, 0x01)));
    assert_eq!(security(&decl(Props::WRITE, Permission::Authenticated)), Some((permission::AUTHEN_WRITE | permission::ENCRY_WRITE, 0x01)));
    assert_eq!(security(&decl(Props::NOTIFY, Permission::Authenticated)), Some((0x00, 0x00)), "nothing to write");
    assert_eq!(security(&decl(Props::READ, Permission::Open)), Some((0x00, 0x00)));
    assert_eq!(security(&decl(Props::READ, Permission::Encrypted)), None);
    assert_eq!(security(&decl(Props::READ.with(Props::WRITE), Permission::Authenticated)), None);
}

#[test]
fn a_service_counts_its_declaration_two_records_a_characteristic_and_each_descriptor() {
    static CHARS: [CharDecl; 3] = [
        CharDecl { uuid: BluetoothUuid::from_u16(1), props: Props::READ, permission: Permission::Open, max_len: 1, value: &[] },
        CharDecl { uuid: BluetoothUuid::from_u16(2), props: Props::NOTIFY, permission: Permission::Open, max_len: 1, value: &[] },
        CharDecl { uuid: BluetoothUuid::from_u16(3), props: Props::INDICATE, permission: Permission::Open, max_len: 1, value: &[] },
    ];
    let s = ServiceDecl { name: "test", uuid: BluetoothUuid::from_u32(0x1234_5678), chars: &CHARS };
    assert_eq!(records(&s), 1 + 2 + 3 + 3);
    let c = add_service(&s).unwrap();
    assert_eq!(c.params()[0], 0x02, "a 32-bit UUID goes as 128 bits");
    assert_eq!(c.params()[1..17], s.uuid.to_u128().to_le_bytes());
    assert_eq!(c.params()[17..], [0x01, 9]);
}

#[test]
fn the_handles_hold_what_fits() {
    let mut h = Handles::default();
    let c = decl(Props::NOTIFY, Permission::Open);
    for i in 0..MAX_CHARS {
        assert!(h.push(1, 2 + 3 * u16::try_from(i).unwrap(), &c));
    }
    assert!(!h.push(1, 0x100, &c));
    assert_eq!(h.find(0x100 + 1), None);
    assert_eq!(h.find(1), None);
}
