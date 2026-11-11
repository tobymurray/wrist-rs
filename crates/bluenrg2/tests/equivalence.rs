//! [`parse`] against a reference decoder written as slice patterns over the packet.

mod common;

use bluenrg2::{packet_len, vendor, AddrKind, Address, BdAddr, ConnHandle, LeConnRole};
use common::{parse, Ev, Timing};
use proptest::prelude::*;

mod reference {
    use super::common::{Ev, Timing, Vn};
    use bluenrg2::bt_hci::FromHciBytes;
    use bluenrg2::{
        event_code, le_subevent, packet, packet_len, packet_whole, AclBroadcastFlag,
        AclPacketBoundary, AddrKind, Address, BdAddr, BleStatus, ConnHandle, Duration,
        EncryptionEnabledLevel, LeConnRole,
    };
    use bluenrg2::{vendor as v, BootReason, PairingStatus, Uuid};

    /// The event in one HCI packet, read no further than the packet's own length, with no
    /// connection handle above 0x0EFF.
    #[must_use]
    pub fn parse(p: &[u8]) -> Ev<'_> {
        match any_handle(p) {
            Ev::LeConnectionComplete { handle, .. }
            | Ev::LeConnectionUpdateComplete { handle, .. }
            | Ev::LeConnectionUpdateFailed { handle, .. }
            | Ev::DisconnectionComplete { handle, .. }
            | Ev::EncryptionChange { handle, .. }
            | Ev::EncryptionKeyRefresh { handle, .. }
            | Ev::LeDataLengthChange { handle, .. }
                if handle.raw() > 0x0EFF =>
            {
                Ev::Malformed(&p[..packet_len(p).unwrap()])
            }
            Ev::Acl { handle, .. } if handle.raw() > 0x0EFF => {
                Ev::Malformed(&p[..packet_len(p).unwrap()])
            }
            Ev::Vendor(ref e) if vendor_handle(e).is_some_and(|h| h.raw() > 0x0EFF) => {
                Ev::Malformed(&p[..packet_len(p).unwrap()])
            }
            e => e,
        }
    }

    fn vendor_handle(e: &Vn<'_>) -> Option<ConnHandle> {
        match *e {
            Vn::PairingComplete { handle, .. }
            | Vn::PassKeyRequest { handle }
            | Vn::NumericComparison { handle, .. }
            | Vn::L2capUpdateResponse { handle, .. }
            | Vn::L2capUpdateRequest { handle, .. }
            | Vn::AttributeModified { handle, .. }
            | Vn::GattProcTimeout { handle }
            | Vn::ExchangeMtuResponse { handle, .. }
            | Vn::FindInfoResponse { handle, .. }
            | Vn::FindByTypeValueResponse { handle, .. }
            | Vn::ReadByTypeResponse { handle, .. }
            | Vn::ReadResponse { handle, .. }
            | Vn::ReadByGroupTypeResponse { handle, .. }
            | Vn::Indication { handle, .. }
            | Vn::Notification { handle, .. }
            | Vn::GattProcComplete { handle, .. }
            | Vn::GattErrorResponse { handle, .. }
            | Vn::TxPoolAvailable { handle, .. }
            | Vn::ServerConfirmation { handle } => Some(handle),
            _ => None,
        }
    }

    /// `n` bytes of `rest`, or as many as it holds.
    fn cut(n: u8, rest: &[u8]) -> &[u8] {
        &rest[..usize::from(n).min(rest.len())]
    }

    /// A UUID from its two or sixteen bytes.
    fn uuid(b: &[u8]) -> Uuid {
        match *b {
            [lo, hi] => Uuid::U16(u16::from_le_bytes([lo, hi])),
            _ => Uuid::U128(b.try_into().unwrap()),
        }
    }

    /// ST's vendor events by code (`bluenrg1_events.h`), the handle unchecked; `None` when the
    /// parameters are short of the layout.
    #[allow(clippy::too_many_lines)]
    fn vendor(code: u16, p: &[u8]) -> Option<Vn<'_>> {
        let word = |lo: &u8, hi: &u8| u16::from_le_bytes([*lo, *hi]);
        let conn = |lo: &u8, hi: &u8| ConnHandle::from_hci_bytes(&[*lo, *hi]).unwrap().0;
        Some(match (code, p) {
            (v::BLUE_INITIALIZED, [reason, ..]) => {
                Vn::BlueInitialized { reason: BootReason::new(*reason) }
            }
            (v::HAL_EVENTS_LOST, [b0, b1, b2, b3, b4, b5, b6, b7, ..]) => Vn::EventsLost {
                lost: u64::from_le_bytes([*b0, *b1, *b2, *b3, *b4, *b5, *b6, *b7]),
            },
            (v::GAP_PAIRING_COMPLETE, [lo, hi, status, reason, ..]) => Vn::PairingComplete {
                handle: conn(lo, hi),
                status: PairingStatus::new(*status),
                reason: *reason,
            },
            (v::GAP_PASS_KEY_REQ, [lo, hi, ..]) => Vn::PassKeyRequest { handle: conn(lo, hi) },
            (v::GAP_SLAVE_SECURITY_INITIATED, _) => Vn::SlaveSecurityInitiated,
            (v::GAP_BOND_LOST, _) => Vn::BondLost,
            (v::GAP_PROC_COMPLETE, [procedure, status, ..]) => {
                Vn::GapProcComplete { procedure: *procedure, status: BleStatus::new(*status) }
            }
            (v::GAP_NUMERIC_COMPARISON_VALUE, [lo, hi, v0, v1, v2, v3, ..]) => {
                Vn::NumericComparison {
                    handle: conn(lo, hi),
                    value: u32::from_le_bytes([*v0, *v1, *v2, *v3]),
                }
            }
            (v::L2CAP_CONNECTION_UPDATE_RESP, [lo, hi, r0, r1, ..]) => {
                Vn::L2capUpdateResponse { handle: conn(lo, hi), accepted: [*r0, *r1] == [0, 0] }
            }
            (
                v::L2CAP_CONNECTION_UPDATE_REQ,
                [lo, hi, identifier, _, _, a0, a1, b0, b1, l0, l1, t0, t1, ..],
            ) => Vn::L2capUpdateRequest {
                handle: conn(lo, hi),
                identifier: *identifier,
                interval_min: Duration::from_u16(word(a0, a1)),
                interval_max: Duration::from_u16(word(b0, b1)),
                latency: word(l0, l1),
                timeout: Duration::from_u16(word(t0, t1)),
            },
            (v::GATT_ATTRIBUTE_MODIFIED, [lo, hi, a0, a1, o0, o1, n0, n1, data @ ..]) => {
                Vn::AttributeModified {
                    handle: conn(lo, hi),
                    attr: word(a0, a1),
                    offset: word(o0, &(o1 & 0x7F)),
                    more: o1 & 0x80 != 0,
                    data: &data[..usize::from(word(n0, n1)).min(data.len())],
                }
            }
            (v::GATT_PROC_TIMEOUT, [lo, hi, ..]) => Vn::GattProcTimeout { handle: conn(lo, hi) },
            (v::ATT_EXCHANGE_MTU_RESP, [lo, hi, m0, m1, ..]) => {
                Vn::ExchangeMtuResponse { handle: conn(lo, hi), server_mtu: word(m0, m1) }
            }
            (v::ATT_FIND_INFO_RESP, [lo, hi, format, n, rest @ ..]) => {
                let each = match format {
                    1 => 4,
                    2 => 18,
                    _ => usize::MAX,
                };
                let data = cut(*n, rest);
                let descriptors = (0..data.len().checked_div(each).unwrap_or(0))
                    .map(|i| &data[i * each..(i + 1) * each]);
                Vn::FindInfoResponse {
                    handle: conn(lo, hi),
                    descriptors: descriptors.map(|e| (word(&e[0], &e[1]), uuid(&e[2..]))).collect(),
                }
            }
            (v::ATT_FIND_BY_TYPE_VALUE_RESP, [lo, hi, count, rest @ ..]) => {
                let pairs = rest.len() / 4;
                let ranges = (0..pairs.min(usize::from(*count))).map(|i| {
                    (word(&rest[4 * i], &rest[4 * i + 1]), word(&rest[4 * i + 2], &rest[4 * i + 3]))
                });
                Vn::FindByTypeValueResponse { handle: conn(lo, hi), ranges: ranges.collect() }
            }
            (v::ATT_READ_BY_TYPE_RESP, [lo, hi, each, n, rest @ ..]) => {
                let each = usize::from(*each);
                let data = cut(*n, rest);
                let all = if each == 7 || each == 21 { data.len() / each } else { 0 };
                let characteristics = (0..all)
                    .map(|i| &data[i * each..(i + 1) * each])
                    .map(|e| (word(&e[0], &e[1]), e[2], word(&e[3], &e[4]), uuid(&e[5..])));
                Vn::ReadByTypeResponse {
                    handle: conn(lo, hi),
                    characteristics: characteristics.collect(),
                }
            }
            (v::ATT_READ_RESP, [lo, hi, n, rest @ ..]) => {
                Vn::ReadResponse { handle: conn(lo, hi), value: cut(*n, rest) }
            }
            (v::ATT_READ_BY_GROUP_TYPE_RESP, [lo, hi, each, n, rest @ ..]) => {
                let each = usize::from(*each);
                let data = cut(*n, rest);
                let all = if each == 6 || each == 20 { data.len() / each } else { 0 };
                let services = (0..all)
                    .map(|i| &data[i * each..(i + 1) * each])
                    .map(|e| (word(&e[0], &e[1]), word(&e[2], &e[3]), uuid(&e[4..])));
                Vn::ReadByGroupTypeResponse { handle: conn(lo, hi), services: services.collect() }
            }
            (v::GATT_INDICATION, [lo, hi, a0, a1, n, rest @ ..]) => {
                Vn::Indication { handle: conn(lo, hi), attr: word(a0, a1), value: cut(*n, rest) }
            }
            (v::GATT_NOTIFICATION, [lo, hi, a0, a1, n, rest @ ..]) => {
                Vn::Notification { handle: conn(lo, hi), attr: word(a0, a1), value: cut(*n, rest) }
            }
            (v::GATT_PROC_COMPLETE, [lo, hi, status, ..]) => {
                Vn::GattProcComplete { handle: conn(lo, hi), status: BleStatus::new(*status) }
            }
            (v::GATT_ERROR_RESP, [lo, hi, request_opcode, a0, a1, error, ..]) => {
                Vn::GattErrorResponse {
                    handle: conn(lo, hi),
                    request_opcode: *request_opcode,
                    attr: word(a0, a1),
                    error: *error,
                }
            }
            (v::GATT_TX_POOL_AVAILABLE, [lo, hi, a0, a1, ..]) => {
                Vn::TxPoolAvailable { handle: conn(lo, hi), available: word(a0, a1) }
            }
            (v::GATT_SERVER_CONFIRMATION, [lo, hi, ..]) => {
                Vn::ServerConfirmation { handle: conn(lo, hi) }
            }
            (
                v::BLUE_INITIALIZED
                | v::HAL_EVENTS_LOST
                | v::GAP_PAIRING_COMPLETE
                | v::GAP_PASS_KEY_REQ
                | v::GAP_PROC_COMPLETE
                | v::GAP_NUMERIC_COMPARISON_VALUE
                | v::L2CAP_CONNECTION_UPDATE_RESP
                | v::L2CAP_CONNECTION_UPDATE_REQ
                | v::GATT_ATTRIBUTE_MODIFIED
                | v::GATT_PROC_TIMEOUT
                | v::ATT_EXCHANGE_MTU_RESP
                | v::ATT_FIND_INFO_RESP
                | v::ATT_FIND_BY_TYPE_VALUE_RESP
                | v::ATT_READ_BY_TYPE_RESP
                | v::ATT_READ_RESP
                | v::ATT_READ_BY_GROUP_TYPE_RESP
                | v::GATT_INDICATION
                | v::GATT_NOTIFICATION
                | v::GATT_PROC_COMPLETE
                | v::GATT_ERROR_RESP
                | v::GATT_TX_POOL_AVAILABLE
                | v::GATT_SERVER_CONFIRMATION,
                _,
            ) => return None,
            (code, params) => Vn::Unknown { code, params },
        })
    }

    #[allow(clippy::too_many_lines)]
    fn any_handle(p: &[u8]) -> Ev<'_> {
        // The interval after the peer's address; the Enhanced event puts two more addresses first.
        const LEGACY_INTERVAL_AT: usize = 15;
        use event_code as ev;
        use le_subevent as le;
        use packet::{ACL, EVENT};
        if !packet_whole(p) {
            return Ev::Malformed(p);
        }
        let p = &p[..packet_len(p).unwrap()];
        let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        // Unchecked, for `parse` above to judge.
        let handle_at = |i: usize| ConnHandle::from_hci_bytes(&p[i..]).unwrap().0;
        match p {
            [EVENT, ev::COMMAND_COMPLETE, _, _num, 0, 0, return_param_bytes @ ..] => {
                Ev::CommandComplete { cmd_opcode: 0, status: None, return_param_bytes }
            }
            [EVENT, ev::COMMAND_COMPLETE, _, _num, lo, hi, status, return_param_bytes @ ..] => {
                Ev::CommandComplete {
                    cmd_opcode: u16::from_le_bytes([*lo, *hi]),
                    status: Some(BleStatus::new(*status)),
                    return_param_bytes,
                }
            }
            [EVENT, ev::COMMAND_STATUS, _, status, _num, lo, hi, ..] => Ev::CommandStatus {
                cmd_opcode: u16::from_le_bytes([*lo, *hi]),
                status: BleStatus::new(*status),
            },
            [EVENT, ev::DISCONNECTION_COMPLETE, _, status, _, _, reason, ..] => {
                Ev::DisconnectionComplete {
                    handle: handle_at(4),
                    status: BleStatus::new(*status),
                    reason: BleStatus::new(*reason),
                }
            }
            // A reserved Encryption_Enabled (Core Vol 4 Part E §7.7.8) has no `EncryptionEnabledLevel`.
            [EVENT, ev::ENCRYPTION_CHANGE, _, status, _, _, enabled, ..] => {
                match EncryptionEnabledLevel::from_hci_bytes(&[*enabled]) {
                    Ok((enabled, _)) => Ev::EncryptionChange {
                        handle: handle_at(4),
                        status: BleStatus::new(*status),
                        enabled,
                    },
                    Err(_) => Ev::Malformed(p),
                }
            }
            [EVENT, ev::ENCRYPTION_KEY_REFRESH_COMPLETE, _, status, _, _, ..] => {
                Ev::EncryptionKeyRefresh { handle: handle_at(4), status: BleStatus::new(*status) }
            }
            // Status, handle, role, peer address type and address, then (Enhanced) two resolvable
            // addresses, then interval and latency.
            [EVENT, ev::LE_META, _, sub @ (le::CONNECTION_COMPLETE | le::ENHANCED_CONNECTION_COMPLETE), 0x00, _, _, role, kind, a0, a1, a2, a3, a4, a5, ..] =>
            {
                let at = if *sub == le::CONNECTION_COMPLETE {
                    LEGACY_INTERVAL_AT
                } else {
                    LEGACY_INTERVAL_AT + 12
                };
                Ev::LeConnectionComplete {
                    handle: handle_at(5),
                    role: if *role == 0 { LeConnRole::Central } else { LeConnRole::Peripheral },
                    // An Enhanced event says 2 or 3 for an identity it resolved: still public or random.
                    peer: Address {
                        kind: AddrKind::new(kind & 0x01),
                        addr: BdAddr([*a0, *a1, *a2, *a3, *a4, *a5]),
                    },
                    timing: p.get(at..at + 6).map(|v| Timing {
                        conn_interval: Duration::from_u16(u16::from_le_bytes([v[0], v[1]])),
                        peripheral_latency: u16::from_le_bytes([v[2], v[3]]),
                        supervision_timeout: Duration::from_u16(u16::from_le_bytes([v[4], v[5]])),
                    }),
                }
            }
            [EVENT, ev::LE_META, _, le::CONNECTION_COMPLETE | le::ENHANCED_CONNECTION_COMPLETE, status @ 0x01..=0xFF, ..] => {
                Ev::LeConnectionFailed { status: BleStatus::new(*status) }
            }
            [EVENT, ev::LE_META, _, le::CONNECTION_UPDATE_COMPLETE, 0x00, _, _, _, _, _, _, ..] => {
                Ev::LeConnectionUpdateComplete {
                    handle: handle_at(5),
                    conn_interval: Duration::from_u16(u16_at(7)),
                    peripheral_latency: u16_at(9),
                    // A shorter one is malformed, so what stands in here is never compared.
                    supervision_timeout: Duration::from_u16(
                        p.get(11..13).map_or(0, |v| u16::from_le_bytes([v[0], v[1]])),
                    ),
                }
            }
            [EVENT, ev::LE_META, _, le::CONNECTION_UPDATE_COMPLETE, status @ 0x01..=0xFF, _, _, ..] => {
                Ev::LeConnectionUpdateFailed {
                    handle: handle_at(5),
                    status: BleStatus::new(*status),
                }
            }
            [EVENT, ev::LE_META, _, le::DATA_LENGTH_CHANGE, _, _, _, _, _, _, _, _, _, _, ..] => {
                Ev::LeDataLengthChange {
                    handle: handle_at(4),
                    max_tx_octets: u16_at(6),
                    max_tx_time: u16_at(8),
                    max_rx_octets: u16_at(10),
                    max_rx_time: u16_at(12),
                }
            }
            [EVENT, ev::LE_META, _, le::ADVERTISING_REPORT, data @ ..] => {
                Ev::LeAdvertisingReport { data }
            }
            [EVENT, ev::LE_META, _, sub, ..]
                if !matches!(
                    *sub,
                    le::CONNECTION_COMPLETE
                        | le::CONNECTION_UPDATE_COMPLETE
                        | le::DATA_LENGTH_CHANGE
                        | le::ENHANCED_CONNECTION_COMPLETE
                ) =>
            {
                Ev::LeUnknown { subevent_code: *sub }
            }
            [EVENT, ev::VENDOR, _, lo, hi, params @ ..] => {
                vendor(u16::from_le_bytes([*lo, *hi]), params).map_or(Ev::Malformed(p), Ev::Vendor)
            }
            // Handle, then boundary and broadcast flags, in the first two bytes (Core Vol 4 Part E §5.4.2).
            [ACL, lo, hi, _, _, data @ ..] => Ev::Acl {
                handle: ConnHandle::from_hci_bytes(
                    &(u16::from_le_bytes([*lo, *hi]) & 0x0FFF).to_le_bytes(),
                )
                .unwrap()
                .0,
                boundary: [
                    AclPacketBoundary::FirstNonFlushable,
                    AclPacketBoundary::Continuing,
                    AclPacketBoundary::FirstFlushable,
                    AclPacketBoundary::Complete,
                ][usize::from((hi >> 4) & 0x03)],
                broadcast: [
                    AclBroadcastFlag::PointToPoint,
                    AclBroadcastFlag::BrEdrBroadcast,
                    AclBroadcastFlag::Reserved,
                    AclBroadcastFlag::Reserved,
                ][usize::from(hi >> 6)],
                data,
            },
            [EVENT, code, _, ..]
                if !matches!(
                    *code,
                    ev::DISCONNECTION_COMPLETE
                        | ev::ENCRYPTION_CHANGE
                        | ev::ENCRYPTION_KEY_REFRESH_COMPLETE
                        | ev::COMMAND_COMPLETE
                        | ev::COMMAND_STATUS
                        | ev::LE_META
                        | ev::VENDOR
                ) =>
            {
                Ev::Unknown { code: *code }
            }
            _ => Ev::Malformed(p),
        }
    }
}

/// What [`parse`] must give: the reference's event, except where the reference takes a
/// packet the Core Specification's layout does not allow.
fn expected(p: &[u8]) -> Ev<'_> {
    let reference = reference::parse(p);
    let whole = &p[..packet_len(p).map_or(p.len(), |len| len.min(p.len()))];
    let params = whole.get(3..).unwrap_or_default();
    match reference {
        // Num_Reports starts an advertising report (Core Vol 4 Part E §7.7.65.2).
        Ev::LeAdvertisingReport { data: [] } => Ev::Malformed(whole),
        // Supervision_Timeout ends a success (Core Vol 4 Part E §7.7.65.3).
        Ev::LeConnectionUpdateComplete { .. } if params.len() < 1 + 9 => Ev::Malformed(whole),
        // Role is 0 or 1, and the timing comes only with a whole event, its Central_Clock_Accuracy
        // not reserved (Core Vol 4 Part E §7.7.65.1, §7.7.65.10).
        Ev::LeConnectionComplete { .. } if params[1 + 3] > 1 => Ev::Malformed(whole),
        Ev::LeConnectionComplete { handle, role, peer, timing } => {
            let (len, accuracy_at) =
                if params[0] == 0x01 { (1 + 18, 1 + 17) } else { (1 + 30, 1 + 29) };
            let full = params.len() >= len && params[accuracy_at] <= 7;
            Ev::LeConnectionComplete { handle, role, peer, timing: timing.filter(|_| full) }
        }
        e => e,
    }
}

fn check(p: &[u8]) {
    assert_eq!(parse(p), expected(p), "{p:02X?}");
}

fn hci_event(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, code, u8::try_from(params.len()).unwrap()];
    p.extend_from_slice(params);
    p
}

fn le_event(sub: u8, params: &[u8]) -> Vec<u8> {
    let mut all = vec![sub];
    all.extend_from_slice(params);
    hci_event(0x3E, &all)
}

fn vendor_event(code: u16, params: &[u8]) -> Vec<u8> {
    let mut all = code.to_le_bytes().to_vec();
    all.extend_from_slice(params);
    hci_event(0xFF, &all)
}

fn command_complete(opcode: u16, status: u8, params: &[u8]) -> Vec<u8> {
    let mut all = vec![0x01];
    all.extend(opcode.to_le_bytes());
    all.push(status);
    all.extend_from_slice(params);
    hci_event(0x0E, &all)
}

#[allow(clippy::too_many_lines)]
fn corpus() -> Vec<Vec<u8>> {
    let connection = le_event(
        0x01,
        &[
            0x00, 0x01, 0x08, 0x01, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 24, 0, 0, 0, 0xF4,
            0x01, 0x00,
        ],
    );
    let mut enhanced_params = vec![0x00, 0x01, 0x08, 0x01, 0x00];
    enhanced_params.extend([0x11; 6]);
    enhanced_params.extend([0; 12]);
    enhanced_params.extend([39, 0, 0, 0, 0xF4, 0x01, 0x00]);
    let enhanced = le_event(0x0A, &enhanced_params);
    let mut enhanced_central = enhanced.clone();
    enhanced_central[7] = 0x00;
    enhanced_central[8] = 0x03;
    let update = le_event(0x03, &[0x00, 0x01, 0x08, 12, 0, 0, 0, 0xF4, 0x01]);

    let mut ad = vec![
        0x02, 0x01, 0x06, 0x05, 0x03, 0x0D, 0x18, 0x0F, 0x18, 0x05, 0x09, b'H', b'1', b'0', b' ',
    ];
    let mut strap =
        vec![0x00, 0x01, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xC6, u8::try_from(ad.len()).unwrap()];
    strap.append(&mut ad);
    strap.push((-67i8).cast_unsigned());
    let mut reports = vec![0x04, 0x3E, 0x00, 0x02, 0x02];
    reports.extend(&strap);
    reports.extend([0x04, 0x00, 1, 2, 3, 4, 5, 6, 4, 3, 0x08, b'P', b'M', 0xB0]);
    reports[2] = u8::try_from(reports.len() - 3).unwrap();

    let mut long_write = vec![0x01, 0x08, 0x19, 0x00, 0x00, 0x00, 217, 0x00];
    long_write.extend((0..217u8).map(|i| i.wrapping_mul(7)));
    let mut acl = vec![0x02, 0x01, 0x20, 0x02, 0x01];
    acl.extend([0x5A; 258]);
    acl.extend(vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]));
    let mut two = hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]);
    two.extend(vendor_event(vendor::BLUE_INITIALIZED, &[0x01]));
    let mut two_vendor_first = vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]);
    two_vendor_first.extend(hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]));

    let firmware = [2, 1, 0, 0x02, 0x34, 0x12, 2, 2, 1, 0, 0x0F, 0x00, 0x31, 0x09];
    let mut all = vec![
        connection,
        enhanced,
        enhanced_central,
        update,
        reports,
        two,
        two_vendor_first,
        acl[263..].to_vec(),
        acl,
        vec![0x04, 0x0E, 0x05, 0x01, 0x06, 0xFD, 0x00, 0xAA],
        vec![0x04, 0x0E, 0x03, 0x01, 0x00, 0x00],
        vec![0x04, 0x0E, 0x04, 0x01, 0x00, 0x00, 0x00],
        vec![0x04, 0x0E, 0x03, 0x01, 0x01, 0x10],
        vec![0x04, 0x0E, 0x04, 0x01, 0x01, 0x10, 0x00],
        vec![0x04, 0x0F, 0x04, 0x00, 0x01, 0x13, 0xFD],
        vec![0x04, 0x0F, 0x04, 0x0C, 0x01, 0x9E, 0xFC],
        le_event(0x07, &[0x01, 0x08, 0xFB, 0x00, 0x48, 0x08, 0x1B, 0x00, 0x48, 0x01]),
        hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]),
        hci_event(0x05, &[0x00, 0x02, 0x08, 0x08]),
        hci_event(0x05, &[0x00, 0x01, 0x08, 0x3D]),
        hci_event(0x05, &[0x0C, 0x01, 0x08, 0x00]),
        hci_event(0x08, &[0x00, 0x01, 0x08, 0x01]),
        hci_event(0x08, &[0x06, 0x01, 0x08, 0x00]),
        le_event(0x04, &[0x00, 0x01, 0x08]),
        le_event(0x0C, &[0x00, 0x01, 0x08]),
        vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]),
        vendor_event(vendor::ATT_EXCHANGE_MTU_RESP, &[0x01, 0x08, 0xDC, 0x00]),
        vendor_event(vendor::GATT_ATTRIBUTE_MODIFIED, &[0u8; 225]),
        vendor_event(vendor::GATT_ATTRIBUTE_MODIFIED, &long_write),
        vendor_event(vendor::GAP_SLAVE_SECURITY_INITIATED, &[]),
        vendor_event(vendor::L2CAP_CONNECTION_UPDATE_RESP, &[0x01, 0x08, 0x00, 0x00]),
        vendor_event(vendor::L2CAP_CONNECTION_UPDATE_RESP, &[0x01, 0x08, 0x01, 0x00]),
        vendor_event(vendor::L2CAP_CONNECTION_UPDATE_RESP, &[0x01, 0x08]),
        command_complete(bluenrg2::opcode::ACI_HAL_GET_FIRMWARE_DETAILS, 0, &firmware),
        command_complete(bluenrg2::opcode::ACI_HAL_GET_FIRMWARE_DETAILS, 0, &firmware[..5]),
        command_complete(bluenrg2::opcode::ACI_HAL_GET_UPDATER_VERSION, 0, &[3]),
        command_complete(bluenrg2::opcode::ACI_HAL_GET_ANCHOR_PERIOD, 0, &[0; 8]),
        command_complete(
            bluenrg2::opcode::ACI_HAL_GET_ANCHOR_PERIOD,
            0,
            &[0x20, 0x03, 0, 0, 0xE8, 0x02, 0, 0],
        ),
        command_complete(
            bluenrg2::opcode::ACI_HAL_GET_ANCHOR_PERIOD,
            0,
            &[0x38, 0, 0, 0, 0x10, 0, 0, 0],
        ),
        command_complete(bluenrg2::opcode::ACI_HAL_GET_ANCHOR_PERIOD, 0x0C, &[]),
        vec![],
        vec![0x04],
        vec![0x04, 0x05],
        vec![0x04, 0x3E, 0x00],
        vec![0x04, 0xFF, 0x01, 0x01],
        vec![0x01, 0x01, 0x10, 0x00],
        vec![0x02, 0x01, 0x20, 0x01, 0x00, 0x5A],
        vec![0x02, 0x01, 0xF8, 0x00, 0x00],
        vec![0x02, 0x00, 0x2F, 0x00, 0x00],
        vec![0x00, 0x04, 0x05, 0x00],
        vec![0xFF, 0x04, 0x05],
    ];
    for code in [0x10, 0x13, 0x1A, 0x30] {
        all.push(hci_event(code, &[0x00, 0x01, 0x08]));
        all.push(hci_event(code, &[]));
    }
    // Every vendor event, its lengths and counts mostly in range.
    for code in VENDOR_CODES {
        all.push(vendor_event(
            code,
            &[
                0x01, 0x08, 0x02, 0x04, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, 0x05, 0x00,
                0x06, 0x00,
            ],
        ));
    }
    all
}

/// The vendor events the crate decodes, and one it doesn't.
const VENDOR_CODES: [u16; 25] = [
    vendor::BLUE_INITIALIZED,
    vendor::HAL_EVENTS_LOST,
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

/// A vendor event of any code above, its handle mostly valid and its element length, format or
/// count mostly one the layout takes.
fn vendor_events() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(VENDOR_CODES.to_vec()),
        prop::collection::vec(any::<u8>(), 0..48),
        any::<bool>(),
    )
        .prop_map(|(code, mut params, valid)| {
            if valid && params.len() > 3 {
                params[1] &= 0x0E;
                params[2] = [1, 2, 6, 7, 20, 21][usize::from(params[2] % 6)];
            }
            vendor_event(code, &params)
        })
}

/// The packet with its length field set to `n` parameters and the bytes after them dropped.
fn shortened(p: &[u8], n: usize) -> Vec<u8> {
    let mut q = p[..3 + n].to_vec();
    q[2] = u8::try_from(n).unwrap();
    q
}

#[test]
fn where_the_reference_takes_what_the_layout_does_not_allow() {
    let peer =
        Address { kind: AddrKind::PUBLIC, addr: BdAddr([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]) };
    let connection = le_event(0x01, &le_full(0x01));
    let mut reserved_role = connection.clone();
    reserved_role[7] = 0x02;
    let mut reserved_accuracy = connection.clone();
    reserved_accuracy[21] = 0x08;
    assert!(matches!(
        reference::parse(&reserved_role),
        Ev::LeConnectionComplete { role: LeConnRole::Peripheral, .. }
    ));
    assert_eq!(parse(&reserved_role), Ev::Malformed(&reserved_role));
    for p in [shortened(&connection, 18), reserved_accuracy] {
        assert!(
            matches!(reference::parse(&p), Ev::LeConnectionComplete { handle, timing: Some(Timing { conn_interval, .. }), .. } if handle == ConnHandle::new(0x0801) && conn_interval.as_u16() == 24)
        );
        assert_eq!(
            parse(&p),
            Ev::LeConnectionComplete {
                handle: ConnHandle::new(0x0801),
                role: LeConnRole::Peripheral,
                peer,
                timing: None
            }
        );
    }
    let encryption = hci_event(0x08, &[0x00, 0x01, 0x08, 0x03]);
    assert_eq!(parse(&encryption), Ev::Malformed(&encryption));
    let update = shortened(&le_event(0x03, &le_full(0x03)), 8);
    assert!(matches!(reference::parse(&update), Ev::LeConnectionUpdateComplete { .. }));
    assert_eq!(parse(&update), Ev::Malformed(&update));
    let reports = le_event(0x02, &[]);
    assert_eq!(reference::parse(&reports), Ev::LeAdvertisingReport { data: &[] });
    assert_eq!(parse(&reports), Ev::Malformed(&reports));
}

#[test]
fn every_packet_the_tests_use_cut_and_changed_decodes_the_same() {
    for p in corpus() {
        for cut in 0..=p.len() {
            check(&p[..cut]);
        }
        if p.len() >= 3 && p[0] == 0x04 {
            for n in 0..=usize::from(p[2]).min(p.len() - 3) {
                check(&shortened(&p, n));
            }
        }
        for at in 0..p.len().min(48) {
            let mut q = p.clone();
            for b in 0..=u8::MAX {
                q[at] = b;
                check(&q);
            }
        }
    }
}

/// The full parameters (after the subevent) of the LE events with a status first.
fn le_full(sub: u8) -> Vec<u8> {
    match sub {
        0x01 => vec![
            0x00, 0x01, 0x08, 0x01, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 24, 0, 0, 0, 0xF4,
            0x01, 0x00,
        ],
        0x0A => {
            let mut v = vec![0x00, 0x01, 0x08, 0x00, 0x03];
            v.extend([0x11; 6]);
            v.extend([0x22; 12]);
            v.extend([39, 0, 4, 0, 0xF4, 0x01, 0x07]);
            v
        }
        _ => vec![0x00, 0x01, 0x08, 12, 0, 0, 0, 0xF4, 0x01],
    }
}

#[test]
fn every_status_of_every_event_with_one_decodes_the_same_whole_and_cut() {
    for sub in [0x01, 0x03, 0x0A] {
        let full = le_full(sub);
        for status in 0..=u8::MAX {
            let mut params = full.clone();
            params[0] = status;
            params.push(0xEE);
            for n in 0..=params.len() {
                check(&le_event(sub, &params[..n]));
            }
        }
    }
    for code in [0x05, 0x08, 0x0F] {
        for status in 0..=u8::MAX {
            let params = [status, 0x01, 0x08, 0x01, 0xEE];
            for n in 0..=params.len() {
                check(&hci_event(code, &params[..n]));
            }
        }
    }
}

/// An HCI event of a code the decoder reads, with its length field right.
fn event() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![0x05u8, 0x08, 0x0E, 0x0F, 0x3E, 0xFF, 0x10, 0x13, 0x00]),
        prop::collection::vec(any::<u8>(), 0..40),
    )
        .prop_map(|(code, params)| hci_event(code, &params))
}

/// An LE Meta event with a status first, whole or cut, its role, address type, clock accuracy
/// and encryption state mostly valid, sometimes reserved.
fn le_meta() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![0x01u8, 0x02, 0x03, 0x07, 0x0A, 0x04, 0x0C, 0xFF]),
        any::<u8>(),
        prop::collection::vec(any::<u8>(), 40),
        any::<bool>(),
        0usize..42,
    )
        .prop_map(|(sub, status, mut body, valid, cut)| {
            // Mostly a success, for the paths past the status.
            body[0] = if status < 0xC0 { 0 } else { status };
            if valid {
                body[3] %= 2;
                body[4] %= 4;
                if let Some(ca) = body.get_mut(if sub == 0x0A { 29 } else { 17 }) {
                    *ca %= 8;
                }
            }
            body.truncate(cut);
            le_event(sub, &body)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn any_bytes_decode_the_same(p in prop::collection::vec(any::<u8>(), 0..64)) {
        check(&p);
    }

    #[test]
    fn any_event_decodes_the_same(p in event(), extra in prop::collection::vec(any::<u8>(), 0..4)) {
        check(&p);
        let mut q = p.clone();
        q.extend(extra);
        check(&q);
    }

    #[test]
    fn any_vendor_event_decodes_the_same(p in vendor_events()) {
        check(&p);
        for n in 0..p.len() - 3 {
            check(&shortened(&p, n));
        }
    }

    #[test]
    fn any_le_meta_event_decodes_the_same(p in le_meta()) {
        check(&p);
        for n in 0..p.len() - 3 {
            check(&shortened(&p, n));
        }
    }
}
