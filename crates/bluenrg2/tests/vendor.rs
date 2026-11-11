//! ST's vendor events, each decoded only as the event its code names.

mod common;

use bluenrg2::{
    vendor, BleStatus, BootReason, ConnHandle, Duration, Event, PairingReason, PairingStatus, Uuid,
    VendorEvent,
};
use common::{vn, Ev, Vn};

fn link() -> ConnHandle {
    ConnHandle::new(0x0801)
}

/// A vendor event packet: `04 FF len code params`.
fn packet(code: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, 0xFF, u8::try_from(2 + params.len()).unwrap()];
    p.extend_from_slice(&code.to_le_bytes());
    p.extend_from_slice(params);
    p
}

/// The event `params` make under `code`, or `None` for a malformed packet.
fn event(code: u16, params: &[u8]) -> Option<VendorEvent<'static>> {
    let p: &'static [u8] = Box::leak(packet(code, params).into_boxed_slice());
    match bluenrg2::parse(p) {
        Event::Vendor(e) => Some(e),
        Event::Malformed(_) => None,
        e => panic!("{e:?}"),
    }
}

/// [`event`], field by field.
fn decode(code: u16, params: &[u8]) -> Option<Vn<'static>> {
    event(code, params).map(vn)
}

/// The whole event decodes as `want`, as do its first `need` bytes; one byte fewer is malformed.
fn whole_and_short(code: u16, params: &[u8], need: usize, want: Vn<'_>) {
    assert_eq!(decode(code, params), Some(want), "{code:#06x}");
    assert_eq!(event(code, &params[..need]).map(|e| e.code()), Some(code), "{code:#06x} at {need}");
    if need > 0 {
        assert_eq!(decode(code, &params[..need - 1]), None, "{code:#06x} short of {need}");
    }
}

#[test]
fn the_chip_says_why_it_started_and_a_start_without_its_reason_is_still_a_start() {
    let reason = Some(BootReason::UPDATER_BY_COMMAND);
    assert_eq!(decode(vendor::BLUE_INITIALIZED, &[0x02]), Some(Vn::BlueInitialized { reason }));
    let none = Vn::BlueInitialized { reason: None };
    assert_eq!(decode(vendor::BLUE_INITIALIZED, &[]), Some(none));
}

#[test]
fn lost_events_come_as_a_bitmap() {
    let lost = [0x01, 0, 0, 0, 0, 0, 0, 0x80];
    whole_and_short(
        vendor::BLUE_EVENTS_LOST,
        &lost,
        8,
        Vn::EventsLost { lost: 0x8000_0000_0000_0001 },
    );
}

#[test]
fn gap_events_decode() {
    let status = PairingStatus::LTK_MISSING_LOCAL;
    whole_and_short(
        vendor::GAP_PAIRING_COMPLETE,
        &[0x01, 0x08, 0x03, 0x05],
        4,
        Vn::PairingComplete { handle: link(), status, reason: None },
    );
    whole_and_short(
        vendor::GAP_PAIRING_COMPLETE,
        &[0x01, 0x08, 0x02, 0x0B],
        4,
        Vn::PairingComplete {
            handle: link(),
            status: PairingStatus::FAILED,
            reason: Some(PairingReason::SMP_SC_DHKEY_CHECK_FAILED),
        },
    );
    whole_and_short(
        vendor::GAP_PASS_KEY_REQ,
        &[0x01, 0x08],
        2,
        Vn::PassKeyRequest { handle: link() },
    );
    whole_and_short(vendor::GAP_SLAVE_SECURITY_INITIATED, &[], 0, Vn::SlaveSecurityInitiated);
    whole_and_short(vendor::GAP_BOND_LOST, &[], 0, Vn::BondLost);
    let status = BleStatus::new(0x48);
    whole_and_short(
        vendor::GAP_PROC_COMPLETE,
        &[0x02, 0x48, 0x00],
        2,
        Vn::GapProcComplete { procedure: 0x02, status },
    );
    let mut number = vec![0x01, 0x08];
    number.extend(123_456u32.to_le_bytes());
    whole_and_short(
        vendor::GAP_NUMERIC_COMPARISON_VALUE,
        &number,
        6,
        Vn::NumericComparison { handle: link(), value: 123_456 },
    );
}

#[test]
fn the_centrals_answer_to_a_parameter_request_is_accepted_only_at_zero() {
    let resp = vendor::L2CAP_CONNECTION_UPDATE_RESP;
    whole_and_short(
        resp,
        &[0x01, 0x08, 0x00, 0x00],
        4,
        Vn::L2capUpdateResponse { handle: link(), accepted: true },
    );
    for result in [[0x01, 0x00], [0x00, 0x01]] {
        let r = decode(resp, &[&[0x01, 0x08][..], &result].concat());
        assert_eq!(
            r,
            Some(Vn::L2capUpdateResponse { handle: link(), accepted: false }),
            "{result:02X?}"
        );
    }
}

#[test]
fn a_peripherals_parameter_request_keeps_its_units() {
    // Connection 0x0801, identifier 7, L2CAP length 8, 400 to 800 ms, latency 3, 6 s.
    let p = [0x01, 0x08, 7, 8, 0, 0x40, 0x01, 0x80, 0x02, 3, 0, 0x58, 0x02];
    let want = Vn::L2capUpdateRequest {
        handle: link(),
        identifier: 7,
        interval_min: Duration::from_millis(400),
        interval_max: Duration::from_millis(800),
        latency: 3,
        timeout: Duration::from_secs(6),
    };
    whole_and_short(vendor::L2CAP_CONNECTION_UPDATE_REQ, &p, 13, want);
}

#[test]
fn an_attribute_write_gives_its_connection_offset_and_whether_more_follows() {
    // Connection 0x0801, attribute 0x0019, offset 0, 217 bytes.
    let mut params = vec![0x01, 0x08, 0x19, 0x00, 0x00, 0x00, 217, 0x00];
    params.extend((0..217u8).map(|i| i.wrapping_mul(7)));
    let Some(Vn::AttributeModified { handle, attr, offset, more, data }) =
        decode(vendor::GATT_ATTRIBUTE_MODIFIED, &params)
    else {
        panic!()
    };
    assert_eq!((handle, attr, offset, more, data.len(), data[1]), (link(), 0x19, 0, false, 217, 7));
    // Offset 0x0102 with more to come; a length past the event is cut to it.
    let piece = [0x01, 0x08, 0x1C, 0x00, 0x02, 0x81, 0x09, 0x00, 0x01, 0x00];
    let want = Vn::AttributeModified {
        handle: link(),
        attr: 0x1C,
        offset: 0x0102,
        more: true,
        data: &[0x01, 0x00],
    };
    whole_and_short(vendor::GATT_ATTRIBUTE_MODIFIED, &piece, 8, want);
    let empty = decode(vendor::GATT_ATTRIBUTE_MODIFIED, &piece[..8]);
    assert_eq!(
        empty,
        Some(Vn::AttributeModified {
            handle: link(),
            attr: 0x1C,
            offset: 0x0102,
            more: true,
            data: &[]
        })
    );
}

#[test]
fn gatt_client_events_decode() {
    whole_and_short(
        vendor::GATT_PROC_TIMEOUT,
        &[0x01, 0x08],
        2,
        Vn::GattProcTimeout { handle: link() },
    );
    whole_and_short(
        vendor::ATT_EXCHANGE_MTU_RESP,
        &[0x01, 0x08, 0xDC, 0x00],
        4,
        Vn::ExchangeMtuResponse { handle: link(), server_mtu: 220 },
    );
    let ok = Vn::GattProcComplete { handle: link(), status: BleStatus::SUCCESS };
    whole_and_short(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00], 3, ok);
    let refused =
        Vn::GattErrorResponse { handle: link(), request_opcode: 0x0A, attr: 0x0B, error: 0x05 };
    whole_and_short(vendor::GATT_ERROR_RESP, &[0x01, 0x08, 0x0A, 0x0B, 0x00, 0x05], 6, refused);
    whole_and_short(
        vendor::GATT_TX_POOL_AVAILABLE,
        &[0x01, 0x08, 0x05, 0x00],
        4,
        Vn::TxPoolAvailable { handle: link(), available: 5 },
    );
    whole_and_short(
        vendor::GATT_SERVER_CONFIRMATION,
        &[0x01, 0x08],
        2,
        Vn::ServerConfirmation { handle: link() },
    );
}

#[test]
fn values_read_notified_and_indicated_are_cut_to_their_event() {
    whole_and_short(
        vendor::ATT_READ_RESP,
        &[0x01, 0x08, 2, 0x01, 0x02],
        3,
        Vn::ReadResponse { handle: link(), value: &[0x01, 0x02] },
    );
    assert_eq!(
        decode(vendor::ATT_READ_RESP, &[0x01, 0x08, 9, 0x01, 0x02]),
        Some(Vn::ReadResponse { handle: link(), value: &[0x01, 0x02] })
    );
    let value = [0x16, 72, 0x2C];
    let p = [0x01, 0x08, 8, 0, 3, 0x16, 72, 0x2C];
    whole_and_short(
        vendor::GATT_NOTIFICATION,
        &p,
        5,
        Vn::Notification { handle: link(), attr: 8, value: &value },
    );
    whole_and_short(
        vendor::GATT_INDICATION,
        &p,
        5,
        Vn::Indication { handle: link(), attr: 8, value: &value },
    );
    assert_eq!(
        decode(vendor::GATT_NOTIFICATION, &[0x01, 0x08, 8, 0, 9, 0x16]),
        Some(Vn::Notification { handle: link(), attr: 8, value: &[0x16] })
    );
}

fn services(params: &[u8]) -> Vec<(u16, u16, Uuid)> {
    let Some(Vn::ReadByGroupTypeResponse { handle, services }) =
        decode(vendor::ATT_READ_BY_GROUP_TYPE_RESP, params)
    else {
        panic!()
    };
    assert_eq!(handle, link());
    services
}

#[test]
fn primary_services_decode() {
    // Elements of 6: (1, 5, 0x1800), (6, 9, 0x180D).
    let p = [0x01, 0x08, 6, 12, 1, 0, 5, 0, 0x00, 0x18, 6, 0, 9, 0, 0x0D, 0x18];
    assert_eq!(services(&p), [(1, 5, Uuid::U16(0x1800)), (6, 9, Uuid::U16(0x180D))]);
    // A 128-bit service, element length 20.
    let mut q = vec![0x01, 0x08, 20, 20, 0x10, 0, 0x20, 0];
    q.extend([0xAB; 16]);
    assert_eq!(services(&q), [(0x10, 0x20, Uuid::U128([0xAB; 16]))]);
    // An element length that fits no UUID yields nothing; a data length past the event, the whole elements.
    assert_eq!(services(&[0x01, 0x08, 5, 5, 1, 0, 2, 0, 3]), []);
    assert_eq!(services(&p[..13]).len(), 1);
    assert_eq!(decode(vendor::ATT_READ_BY_GROUP_TYPE_RESP, &p[..3]), None);
}

fn characteristics(params: &[u8]) -> Vec<(u16, u8, u16, Uuid)> {
    let Some(Vn::ReadByTypeResponse { handle, characteristics }) =
        decode(vendor::ATT_READ_BY_TYPE_RESP, params)
    else {
        panic!()
    };
    assert_eq!(handle, link());
    characteristics
}

#[test]
fn characteristics_decode() {
    // Heart Rate Measurement: declaration 7, notify, value 8; Body Sensor Location 10, read, 11.
    let p = [0x01, 0x08, 7, 14, 7, 0, 0x10, 8, 0, 0x37, 0x2A, 10, 0, 0x02, 11, 0, 0x38, 0x2A];
    assert_eq!(
        characteristics(&p),
        [(7, 0x10, 8, Uuid::U16(0x2A37)), (10, 0x02, 11, Uuid::U16(0x2A38))]
    );
    assert_eq!(characteristics(&p[..13]).len(), 1);
    let mut q = vec![0x01, 0x08, 21, 21, 0x11, 0, 0x12, 0x12, 0];
    q.extend([0xCD; 16]);
    assert_eq!(characteristics(&q), [(0x11, 0x12, 0x12, Uuid::U128([0xCD; 16]))]);
    assert_eq!(characteristics(&[0x01, 0x08, 6, 6, 7, 0, 0x10, 8, 0, 0x37]), []);
    assert_eq!(decode(vendor::ATT_READ_BY_TYPE_RESP, &p[..3]), None);
}

fn descriptors(params: &[u8]) -> Vec<(u16, Uuid)> {
    let Some(Vn::FindInfoResponse { handle, descriptors }) =
        decode(vendor::ATT_FIND_INFO_RESP, params)
    else {
        panic!()
    };
    assert_eq!(handle, link());
    descriptors
}

#[test]
fn descriptors_decode_by_their_format() {
    assert_eq!(descriptors(&[0x01, 0x08, 1, 4, 9, 0, 0x02, 0x29]), [(9, Uuid::U16(0x2902))]);
    assert_eq!(descriptors(&[0x01, 0x08, 3, 4, 9, 0, 0x02, 0x29]), []);
    let mut d = vec![0x01, 0x08, 2, 18, 0x13, 0];
    d.extend([0xEF; 16]);
    assert_eq!(descriptors(&d), [(0x13, Uuid::U128([0xEF; 16]))]);
    assert_eq!(decode(vendor::ATT_FIND_INFO_RESP, &[0x01, 0x08, 1]), None);
}

#[test]
fn handle_ranges_stop_at_their_count_or_the_events_end() {
    let ranges = |params: &[u8]| {
        let Some(Vn::FindByTypeValueResponse { handle, ranges }) =
            decode(vendor::ATT_FIND_BY_TYPE_VALUE_RESP, params)
        else {
            panic!()
        };
        assert_eq!(handle, link());
        ranges
    };
    let two = [0x01, 0x08, 2, 6, 0, 9, 0, 0x10, 0, 0x1F, 0];
    assert_eq!(ranges(&two), [(6, 9), (0x10, 0x1F)]);
    assert_eq!(ranges(&[&two[..2], &[1], &two[3..]].concat()), [(6, 9)]);
    assert_eq!(ranges(&[&two[..2], &[5], &two[3..]].concat()), [(6, 9), (0x10, 0x1F)]);
    assert_eq!(ranges(&two[..10]), [(6, 9)]);
    assert_eq!(decode(vendor::ATT_FIND_BY_TYPE_VALUE_RESP, &two[..2]), None);
}

#[test]
fn a_code_this_crate_does_not_decode_keeps_its_bytes() {
    assert_eq!(
        decode(0x0C13, &[0x01, 0x08, 0xAA]),
        Some(Vn::Unknown { code: 0x0C13, params: &[0x01, 0x08, 0xAA] })
    );
    assert_eq!(decode(0x0C13, &[]), Some(Vn::Unknown { code: 0x0C13, params: &[] }));
}

#[test]
fn every_event_names_the_code_it_came_with() {
    let codes = [
        vendor::BLUE_INITIALIZED,
        vendor::BLUE_EVENTS_LOST,
        vendor::GAP_PAIRING_COMPLETE,
        vendor::GAP_PASS_KEY_REQ,
        vendor::GAP_SLAVE_SECURITY_INITIATED,
        vendor::GAP_BOND_LOST,
        vendor::GAP_PROC_COMPLETE,
        vendor::GAP_NUMERIC_COMPARISON_VALUE,
        vendor::L2CAP_CONNECTION_UPDATE_RESP,
        vendor::L2CAP_CONNECTION_UPDATE_REQ,
        vendor::GATT_ATTRIBUTE_MODIFIED,
        vendor::GATT_PROC_TIMEOUT,
        vendor::ATT_EXCHANGE_MTU_RESP,
        vendor::ATT_FIND_INFO_RESP,
        vendor::ATT_FIND_BY_TYPE_VALUE_RESP,
        vendor::ATT_READ_BY_TYPE_RESP,
        vendor::ATT_READ_RESP,
        vendor::ATT_READ_BY_GROUP_TYPE_RESP,
        vendor::GATT_INDICATION,
        vendor::GATT_NOTIFICATION,
        vendor::GATT_PROC_COMPLETE,
        vendor::GATT_ERROR_RESP,
        vendor::GATT_TX_POOL_AVAILABLE,
        vendor::GATT_SERVER_CONFIRMATION,
        0x0C13,
    ];
    for code in codes {
        let e = event(code, &[0x01, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(e.code(), code, "{e:?}");
        assert!(!matches!(e, VendorEvent::Unknown { .. }) || code == 0x0C13, "{e:?}");
    }
}

/// One `GATT_NOTIFICATION` payload is no other event, whatever decoder would have read it.
#[test]
fn a_payload_decodes_only_as_the_event_its_code_names() {
    let p = packet(vendor::GATT_NOTIFICATION, &[0x01, 0x08, 0x12, 0x00, 0x02, 0xEF, 0x00]);
    assert_eq!(
        common::parse(&p),
        Ev::Vendor(Vn::Notification { handle: link(), attr: 0x12, value: &[0xEF, 0x00] })
    );
}
