//! The BlueNRG-2's packets as bytes, without the SPI bus or the link's state.
//!
//! ST's network-coprocessor firmware speaks HCI with ST's vendor commands
//! (OGF 0x3F, the ACI) and vendor events (event code 0xFF with a 16-bit
//! code), over SPI with a five-byte header each way. This crate holds the
//! numbers (from ST's public BlueNRG-1/2 ACI documentation), builds command
//! packets, measures packets in a read, and decodes every packet into an
//! event ([`parse`], then the vendor payload decoders). The image's
//! `ble` module does the transfers and keeps the link's state around it.

#![no_std]

/// The SPI header the host sends to write (`0A`) or read (`0B`); the chip
/// answers in the same five bytes.
pub const HEADER_WRITE: [u8; 5] = [0x0A, 0, 0, 0, 0];
pub const HEADER_READ: [u8; 5] = [0x0B, 0, 0, 0, 0];

/// The chip's answer to a header: room in its write buffer, and bytes
/// waiting to be read. The BlueNRG-2 sends no ready byte, only these.
#[must_use]
pub fn header_sizes(header: &[u8; 5]) -> (usize, usize) {
    (
        usize::from(u16::from_le_bytes([header[1], header[2]])),
        usize::from(u16::from_le_bytes([header[3], header[4]])),
    )
}

/// HCI commands and ST's vendor commands.
pub mod opcode {
    pub const HCI_READ_LOCAL_VERSION: u16 = 0x1001;
    pub const HCI_SET_EVENT_MASK: u16 = 0x0C01;
    pub const HCI_LE_RAND: u16 = 0x2018;
    pub const ACI_HAL_GET_FW_BUILD_NUMBER: u16 = 0xFC00;
    pub const ACI_HAL_WRITE_CONFIG_DATA: u16 = 0xFC0C;
    pub const ACI_HAL_SET_TX_POWER_LEVEL: u16 = 0xFC0F;
    pub const ACI_GAP_SET_NON_DISCOVERABLE: u16 = 0xFC81;
    pub const ACI_GAP_SET_DISCOVERABLE: u16 = 0xFC83;
    pub const ACI_GAP_SET_IO_CAPABILITY: u16 = 0xFC85;
    pub const ACI_GAP_SET_AUTHENTICATION_REQUIREMENT: u16 = 0xFC86;
    pub const ACI_GAP_PASS_KEY_RESP: u16 = 0xFC88;
    pub const ACI_GAP_INIT: u16 = 0xFC8A;
    pub const ACI_GAP_SLAVE_SECURITY_REQ: u16 = 0xFC8D;
    pub const ACI_GAP_TERMINATE: u16 = 0xFC93;
    pub const ACI_GAP_CLEAR_SECURITY_DB: u16 = 0xFC94;
    pub const ACI_GAP_ALLOW_REBOND: u16 = 0xFC95;
    /// Scan interval and window (0.625 ms), own address type, filter duplicates.
    pub const ACI_GAP_START_GENERAL_DISCOVERY_PROC: u16 = 0xFC97;
    /// Scan interval and window, peer address type and address, own address type, interval
    /// min and max, latency, supervision timeout, CE length min and max.
    pub const ACI_GAP_CREATE_CONNECTION: u16 = 0xFC9C;
    /// Ends a GAP procedure by its code ([`super::gap_proc`]).
    pub const ACI_GAP_TERMINATE_GAP_PROC: u16 = 0xFC9D;
    /// As a central: handle, interval min and max, latency, timeout, CE length min and max.
    pub const ACI_GAP_START_CONNECTION_UPDATE: u16 = 0xFC9E;
    pub const ACI_GAP_GET_BONDED_DEVICES: u16 = 0xFCA3;
    pub const ACI_GAP_NUMERIC_COMPARISON_CONFIRM: u16 = 0xFCA5;
    pub const ACI_GATT_INIT: u16 = 0xFD01;
    pub const ACI_GATT_ADD_SERVICE: u16 = 0xFD02;
    pub const ACI_GATT_ADD_CHAR: u16 = 0xFD04;
    pub const ACI_GATT_UPDATE_CHAR_VALUE: u16 = 0xFD06;
    // GATT client procedures, on a connection in either role.
    pub const ACI_GATT_DISC_ALL_PRIMARY_SERVICES: u16 = 0xFD12;
    pub const ACI_GATT_DISC_PRIMARY_SERVICE_BY_UUID: u16 = 0xFD13;
    pub const ACI_GATT_DISC_ALL_CHAR_OF_SERVICE: u16 = 0xFD15;
    /// Handle, characteristic value handle, end handle: the descriptors after the value.
    pub const ACI_GATT_DISC_ALL_CHAR_DESC: u16 = 0xFD17;
    pub const ACI_GATT_READ_CHAR_VALUE: u16 = 0xFD18;
    /// Handle, attribute handle, length (one byte), value.
    pub const ACI_GATT_WRITE_CHAR_VALUE: u16 = 0xFD1C;
    pub const ACI_GATT_WRITE_CHAR_DESC: u16 = 0xFD21;
    /// An indication's confirmation.
    pub const ACI_GATT_CONFIRM_INDICATION: u16 = 0xFD25;
    pub const ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ: u16 = 0xFD81;
    /// As a central, the answer to a peripheral's request ([`super::L2capUpdateRequest`]).
    pub const ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_RESP: u16 = 0xFD82;
}

/// `ACI_GAP_INIT`'s roles, a bitmask.
pub mod role {
    pub const PERIPHERAL: u8 = 0x01;
    pub const CENTRAL: u8 = 0x04;
}

/// GAP procedure codes, for `ACI_GAP_TERMINATE_GAP_PROC` and `GAP_PROC_COMPLETE`.
pub mod gap_proc {
    pub const GENERAL_DISCOVERY: u8 = 0x02;
    pub const DIRECT_CONNECTION: u8 = 0x40;
}

/// `ACI_HAL_WRITE_CONFIG_DATA`'s offsets (ST's `bluenrg1_hal_aci.h`), written before any other command.
pub mod config {
    pub const PUBLIC_ADDRESS: u8 = 0x00;
    pub const STATIC_RANDOM_ADDRESS: u8 = 0x2E;
}

/// Address types, own and identity.
pub mod address {
    pub const PUBLIC: u8 = 0x00;
    pub const STATIC_RANDOM: u8 = 0x01;
}

/// A static random address (Core 6.B 1.3.2.1) from 64 bits, little-endian as the chip carries it.
#[must_use]
pub const fn static_random(bits: u64) -> [u8; 6] {
    let b = bits.to_le_bytes();
    let mut a = [b[0], b[1], b[2], b[3], b[4], b[5] | 0xC0];
    let rest = bits & 0x3FFF_FFFF_FFFF;
    if rest == 0 || rest == 0x3FFF_FFFF_FFFF {
        a[0] ^= 1;
    }
    a
}

/// The watch's static random address from the MCU's 96-bit unique id (FNV-1a).
#[must_use]
pub const fn static_random_address(uid: [u32; 3]) -> [u8; 6] {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut w = 0;
    while w < 3 {
        let bytes = uid[w].to_le_bytes();
        let mut i = 0;
        while i < 4 {
            h ^= bytes[i] as u64;
            h = h.wrapping_mul(0x0100_0000_01B3);
            i += 1;
        }
        w += 1;
    }
    static_random(h)
}

/// `ACI_GATT_ADD_CHAR`'s security permissions (ST's `ble_defs.h`).
pub mod permission {
    pub const NONE: u8 = 0x00;
    /// Writes only over a link encrypted with authenticated (MITM-protected) keys.
    pub const AUTHEN_WRITE: u8 = 0x08;
    /// Writes only over an encrypted link.
    pub const ENCRY_WRITE: u8 = 0x20;
}

/// ST's vendor event codes (event 0xFF).
pub mod vendor {
    pub const BLUE_INITIALIZED: u16 = 0x0001;
    /// The chip dropped events because the host read too slowly.
    pub const HAL_EVENTS_LOST: u16 = 0x0002;
    pub const GAP_PAIRING_COMPLETE: u16 = 0x0401;
    pub const GAP_PASS_KEY_REQ: u16 = 0x0402;
    /// A security request reached the central.
    pub const GAP_SLAVE_SECURITY_INITIATED: u16 = 0x0404;
    pub const GAP_BOND_LOST: u16 = 0x0405;
    /// A GAP procedure ended: its code, status, data ([`super::gap_proc_complete`]).
    pub const GAP_PROC_COMPLETE: u16 = 0x0407;
    pub const GAP_NUMERIC_COMPARISON_VALUE: u16 = 0x0409;
    /// As a central: a peripheral asks for new connection parameters.
    pub const L2CAP_CONNECTION_UPDATE_REQ: u16 = 0x0802;
    pub const GATT_ATTRIBUTE_MODIFIED: u16 = 0x0C01;
    pub const GATT_PROC_TIMEOUT: u16 = 0x0C02;
    pub const ATT_EXCHANGE_MTU_RESP: u16 = 0x0C03;
    pub const ATT_FIND_INFO_RESP: u16 = 0x0C04;
    pub const ATT_FIND_BY_TYPE_VALUE_RESP: u16 = 0x0C05;
    pub const ATT_READ_BY_TYPE_RESP: u16 = 0x0C06;
    pub const ATT_READ_RESP: u16 = 0x0C07;
    pub const ATT_READ_BY_GROUP_TYPE_RESP: u16 = 0x0C0A;
    pub const GATT_INDICATION: u16 = 0x0C0E;
    pub const GATT_NOTIFICATION: u16 = 0x0C0F;
    pub const GATT_PROC_COMPLETE: u16 = 0x0C10;
    pub const GATT_ERROR_RESP: u16 = 0x0C11;
    pub const GATT_TX_POOL_AVAILABLE: u16 = 0x0C16;
    pub const GATT_SERVER_CONFIRMATION: u16 = 0x0C17;
}

/// ST's command status codes (`ble_status.h`).
pub mod status {
    /// No TX buffers for now; `GATT_TX_POOL_AVAILABLE` follows when there are.
    pub const INSUFFICIENT_RESOURCES: u8 = 0x64;
}

/// `ACI_GAP_SET_AUTHENTICATION_REQUIREMENT`'s parameters, in ST's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct AuthenticationRequirement {
    pub bonding: bool,
    pub mitm: bool,
    /// 0 not supported, 1 optional, 2 Secure Connections only.
    pub secure_connections: u8,
    pub keypress_notifications: bool,
    pub min_key_size: u8,
    pub max_key_size: u8,
    /// `None`: the host is asked for a passkey (`GAP_PASS_KEY_REQ`).
    pub fixed_pin: Option<u32>,
    /// 0 public, 1 static random.
    pub identity_address_type: u8,
}

impl AuthenticationRequirement {
    #[must_use]
    pub const fn params(&self) -> [u8; 12] {
        let (use_fixed, pin) = match self.fixed_pin {
            Some(pin) => (0x00, pin),
            None => (0x01, 0),
        };
        let p = pin.to_le_bytes();
        [
            self.bonding as u8,
            self.mitm as u8,
            self.secure_connections,
            self.keypress_notifications as u8,
            self.min_key_size,
            self.max_key_size,
            use_fixed,
            p[0],
            p[1],
            p[2],
            p[3],
            self.identity_address_type,
        ]
    }
}

/// The watch's pairing.
pub const PAIRING: AuthenticationRequirement = AuthenticationRequirement {
    bonding: true,
    mitm: true,
    secure_connections: 2,
    keypress_notifications: false,
    min_key_size: 16,
    max_key_size: 16,
    fixed_pin: None,
    identity_address_type: address::STATIC_RANDOM,
};

/// An HCI command packet (`01`, opcode, length, parameters) into `out`;
/// returns its length. Parameters past 255 bytes, or past `out`, are cut.
pub fn command(opcode: u16, params: &[u8], out: &mut [u8]) -> usize {
    let n = params.len().min(255).min(out.len().saturating_sub(4));
    if out.len() < 4 {
        return 0;
    }
    out[0] = 0x01;
    out[1..3].copy_from_slice(&opcode.to_le_bytes());
    out[3] = u8::try_from(n).unwrap_or(0);
    out[4..4 + n].copy_from_slice(&params[..n]);
    4 + n
}

/// The length of the HCI packet at the start of `p`, clamped to `p`; 0 if
/// empty. A read may hold several packets back to back.
#[must_use]
pub fn packet_len(p: &[u8]) -> usize {
    match p {
        [0x04, _, len, ..] => (3 + usize::from(*len)).min(p.len()),
        [0x02, _, _, lo, hi, ..] => (5 + usize::from(u16::from_le_bytes([*lo, *hi]))).min(p.len()),
        _ => p.len(),
    }
}

/// True when the packet's own length field fits inside what was read.
#[must_use]
pub fn packet_whole(p: &[u8]) -> bool {
    match p {
        [0x04, _, len, ..] => 3 + usize::from(*len) <= p.len(),
        [0x02, _, _, lo, hi, ..] => 5 + usize::from(u16::from_le_bytes([*lo, *hi])) <= p.len(),
        _ => true,
    }
}

/// A device address as the air carries it: little-endian, and public (0) or random (1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Address {
    pub kind: u8,
    pub bytes: [u8; 6],
}

/// The event in one HCI packet, by section of the Core Specification, Vol 4 Part E, whose
/// layouts ST's `bluenrg1_events.h` follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// Command Complete (7.7.14): the opcode answered, its status, the return parameters.
    CommandComplete { cmd_opcode: u16, status: u8, return_param_bytes: &'a [u8] },
    /// Command Status (7.7.15): a command accepted (0) or refused, results to follow.
    CommandStatus { cmd_opcode: u16, status: u8 },
    /// LE (Enhanced) Connection Complete (7.7.65.1, 7.7.65.10) with success: the handle,
    /// whether the watch is the central, the peer, and the interval (1.25 ms units) and
    /// latency when the packet carries them.
    LeConnectionComplete { handle: u16, central: bool, peer: Address, conn_interval: Option<(u16, u16)> },
    /// LE (Enhanced) Connection Complete with an error: no connection formed.
    LeConnectionFailed { status: u8 },
    /// LE Connection Update Complete (7.7.65.3) with success: interval (1.25 ms units), latency.
    LeConnectionUpdateComplete { handle: u16, conn_interval: u16, peripheral_latency: u16 },
    /// LE Connection Update Complete with an error: the link keeps the parameters it had.
    LeConnectionUpdateFailed { handle: u16, status: u8 },
    /// Disconnection Complete (7.7.5): a nonzero status is a disconnection that failed, the
    /// link still up; else the reason (0x13: the peer hung up).
    DisconnectionComplete { handle: u16, status: u8, reason: u8 },
    /// Encryption Change (7.7.8): a nonzero status is a change that failed; 1 on, 0 off.
    EncryptionChange { handle: u16, status: u8, enabled: u8 },
    /// LE Advertising Report (7.7.65.2): one or more reports ([`reports`]).
    LeAdvertisingReport { data: &'a [u8] },
    /// One of ST's events (event code 0xFF): its code and the bytes after it.
    Vendor { code: u16, params: &'a [u8] },
    /// Another LE meta event, by subevent code.
    LeUnknown { subevent_code: u8 },
    /// Another HCI event, by event code.
    Unknown { code: u8 },
    /// Not an HCI event, cut short by the end of the read, or too short for its layout.
    Malformed(&'a [u8]),
}

/// The event in one HCI packet, read no further than the packet's own length.
#[must_use]
pub fn parse(p: &[u8]) -> Event<'_> {
    if !packet_whole(p) {
        return Event::Malformed(p);
    }
    let p = &p[..packet_len(p)];
    let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
    match p {
        [0x04, 0x0E, _, _num, lo, hi, status, return_param_bytes @ ..] => Event::CommandComplete {
            cmd_opcode: u16::from_le_bytes([*lo, *hi]),
            status: *status,
            return_param_bytes,
        },
        [0x04, 0x0F, _, status, _num, lo, hi, ..] => {
            Event::CommandStatus { cmd_opcode: u16::from_le_bytes([*lo, *hi]), status: *status }
        }
        [0x04, 0x05, _, status, _, _, reason, ..] => {
            Event::DisconnectionComplete { handle: u16_at(4), status: *status, reason: *reason }
        }
        [0x04, 0x08, _, status, _, _, enabled, ..] => {
            Event::EncryptionChange { handle: u16_at(4), status: *status, enabled: *enabled }
        }
        // Status, handle, role, peer address type and address, then (Enhanced) two resolvable
        // addresses, then interval and latency.
        [0x04, 0x3E, _, sub @ (0x01 | 0x0A), 0x00, _, _, role, kind, a0, a1, a2, a3, a4, a5, ..] => {
            let at = if *sub == 0x01 { 15 } else { 27 };
            Event::LeConnectionComplete {
                handle: u16_at(5),
                central: *role == 0,
                // An Enhanced event says 2 or 3 for an identity it resolved: still public or random.
                peer: Address { kind: kind & 0x01, bytes: [*a0, *a1, *a2, *a3, *a4, *a5] },
                conn_interval: p
                    .get(at..at + 4)
                    .map(|v| (u16::from_le_bytes([v[0], v[1]]), u16::from_le_bytes([v[2], v[3]]))),
            }
        }
        [0x04, 0x3E, _, 0x01 | 0x0A, status @ 0x01..=0xFF, ..] => Event::LeConnectionFailed { status: *status },
        [0x04, 0x3E, _, 0x03, 0x00, _, _, _, _, _, _, ..] => Event::LeConnectionUpdateComplete {
            handle: u16_at(5),
            conn_interval: u16_at(7),
            peripheral_latency: u16_at(9),
        },
        [0x04, 0x3E, _, 0x03, status @ 0x01..=0xFF, _, _, ..] => {
            Event::LeConnectionUpdateFailed { handle: u16_at(5), status: *status }
        }
        [0x04, 0x3E, _, 0x02, data @ ..] => Event::LeAdvertisingReport { data },
        [0x04, 0x3E, _, sub, ..] if !matches!(sub, 0x01 | 0x03 | 0x0A) => Event::LeUnknown { subevent_code: *sub },
        [0x04, 0xFF, _, lo, hi, params @ ..] => Event::Vendor { code: u16::from_le_bytes([*lo, *hi]), params },
        [0x04, code, _, ..] if !matches!(code, 0x05 | 0x08 | 0x0E | 0x0F | 0x3E | 0xFF) => {
            Event::Unknown { code: *code }
        }
        _ => Event::Malformed(p),
    }
}

/// One advertising report: what kind (0 connectable undirected, 4 a scan response, ...),
/// who, the advertising data and the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report<'a> {
    pub kind: u8,
    pub address: Address,
    pub data: &'a [u8],
    pub rssi: i8,
}

/// The reports in an LE Advertising Report's parameters (after the subevent code): a count,
/// then each report whole (type, address type, address, length, data, RSSI), as ST's stack
/// packs them. A report cut short ends the list.
pub fn reports(data: &[u8]) -> impl Iterator<Item = Report<'_>> + '_ {
    let count = data.first().copied().unwrap_or(0);
    let mut at = 1;
    (0..count).map_while(move |_| {
        let head = data.get(at..at + 9)?;
        let len = usize::from(head[8]);
        let body = data.get(at + 9..at + 9 + len)?;
        let rssi = *data.get(at + 9 + len)?;
        at += 10 + len;
        Some(Report {
            kind: head[0],
            address: Address { kind: head[1] & 0x01, bytes: [head[2], head[3], head[4], head[5], head[6], head[7]] },
            data: body,
            rssi: rssi.cast_signed(),
        })
    })
}

/// Advertising data's structures: (AD type, value). A structure cut short ends the list.
pub fn ad_structures(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> + '_ {
    let mut at = 0;
    core::iter::from_fn(move || {
        let len = usize::from(*data.get(at)?);
        if len == 0 {
            return None;
        }
        let s = data.get(at + 1..at + 1 + len)?;
        at += 1 + len;
        Some((s[0], &s[1..]))
    })
}

/// AD types (Bluetooth SIG assigned numbers).
pub mod ad {
    pub const UUID16_INCOMPLETE: u8 = 0x02;
    pub const UUID16_COMPLETE: u8 = 0x03;
    pub const UUID128_INCOMPLETE: u8 = 0x06;
    pub const UUID128_COMPLETE: u8 = 0x07;
    pub const NAME_SHORT: u8 = 0x08;
    pub const NAME_COMPLETE: u8 = 0x09;
}

/// The name in advertising data, complete or shortened.
#[must_use]
pub fn ad_name(data: &[u8]) -> Option<&[u8]> {
    ad_structures(data).find(|(t, _)| *t == ad::NAME_COMPLETE || *t == ad::NAME_SHORT).map(|(_, v)| v)
}

/// The 16-bit service UUIDs in advertising data.
pub fn ad_uuid16s(data: &[u8]) -> impl Iterator<Item = u16> + '_ {
    ad_structures(data)
        .filter(|(t, _)| *t == ad::UUID16_COMPLETE || *t == ad::UUID16_INCOMPLETE)
        .flat_map(|(_, v)| v.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)))
}

/// The 128-bit service UUIDs in advertising data, little-endian as carried.
pub fn ad_uuid128s(data: &[u8]) -> impl Iterator<Item = &[u8; 16]> + '_ {
    ad_structures(data)
        .filter(|(t, _)| *t == ad::UUID128_COMPLETE || *t == ad::UUID128_INCOMPLETE)
        .flat_map(|(_, v)| v.as_chunks::<16>().0.iter())
}

/// The connection handle that starts most vendor events (0 if short).
#[must_use]
pub fn connection_handle(params: &[u8]) -> u16 {
    u16::from_le_bytes([params.first().copied().unwrap_or(0), params.get(1).copied().unwrap_or(0)])
}

/// `GATT_ATTRIBUTE_MODIFIED`: a peer wrote one of our attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributeModified<'a> {
    pub attr: u16,
    /// Bits 0-14 the offset, bit 15 "more to come" (a long write in pieces).
    pub offset: u16,
    pub data: &'a [u8],
}

impl<'a> AttributeModified<'a> {
    /// Connection handle, attribute handle, offset, length, data; the data
    /// is cut to what the event holds.
    #[must_use]
    pub fn decode(params: &'a [u8]) -> Option<Self> {
        if params.len() < 8 {
            return None;
        }
        let len = usize::from(u16::from_le_bytes([params[6], params[7]])).min(params.len() - 8);
        Some(Self {
            attr: u16::from_le_bytes([params[2], params[3]]),
            offset: u16::from_le_bytes([params[4], params[5]]),
            data: &params[8..8 + len],
        })
    }
}

/// `GAP_NUMERIC_COMPARISON_VALUE`: the connection and the six-digit number.
#[must_use]
pub fn numeric_comparison(params: &[u8]) -> Option<(u16, u32)> {
    let v = params.get(2..6)?;
    Some((connection_handle(params), u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

/// `GAP_PAIRING_COMPLETE`: status (0 success) and reason.
#[must_use]
pub fn pairing_complete(params: &[u8]) -> Option<(u8, u8)> {
    Some((*params.get(2)?, *params.get(3)?))
}

/// `ATT_EXCHANGE_MTU_RESP`: the server's receive MTU.
#[must_use]
pub fn att_mtu(params: &[u8]) -> Option<u16> {
    let v = params.get(2..4)?;
    Some(u16::from_le_bytes([v[0], v[1]]))
}

/// A connection interval in 1.25 ms units as hundredths of a millisecond.
#[must_use]
pub fn interval_hundredths_ms(units: u16) -> u32 {
    u32::from(units) * 125
}

/// A UUID as an attribute carries it: 16-bit, or 128-bit little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uuid {
    U16(u16),
    U128([u8; 16]),
}

impl Uuid {
    /// Two or sixteen bytes; anything else is not a UUID.
    #[must_use]
    pub fn from_le(b: &[u8]) -> Option<Self> {
        match b {
            [lo, hi] => Some(Self::U16(u16::from_le_bytes([*lo, *hi]))),
            _ => b.try_into().ok().map(Self::U128),
        }
    }
}

/// The ATT response events of the GATT client, after the connection handle, share a shape: an
/// element length, a data length, then elements of that length. Elements cut short are dropped.
fn elements(params: &[u8], len_at: usize) -> &[u8] {
    let n = params.get(len_at).map_or(0, |&n| usize::from(n));
    params.get(len_at + 1..).map_or(&[], |rest| &rest[..n.min(rest.len())])
}

/// A primary service from `ATT_READ_BY_GROUP_TYPE_RESP`: its handle range and UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    pub start: u16,
    pub end: u16,
    pub uuid: Uuid,
}

/// `ATT_READ_BY_GROUP_TYPE_RESP` from a primary service discovery: connection handle, element
/// length, data length, then (start, end, UUID) each.
pub fn services(params: &[u8]) -> impl Iterator<Item = Service> + '_ {
    let each = params.get(2).map_or(0, |&n| usize::from(n));
    let data = elements(params, 3);
    data.chunks_exact(each.max(1)).filter(move |_| each == 6 || each == 20).filter_map(|e| {
        Some(Service {
            start: u16::from_le_bytes([e[0], e[1]]),
            end: u16::from_le_bytes([e[2], e[3]]),
            uuid: Uuid::from_le(&e[4..])?,
        })
    })
}

/// A characteristic declaration from `ATT_READ_BY_TYPE_RESP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Characteristic {
    pub declaration: u16,
    pub properties: u8,
    pub value: u16,
    pub uuid: Uuid,
}

/// Characteristic properties (Core Specification, Vol 3, Part G, 3.3.1.1).
pub mod props {
    pub const READ: u8 = 0x02;
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;
}

/// `ATT_READ_BY_TYPE_RESP` from a characteristic discovery: connection handle, element
/// length, data length, then (declaration handle, properties, value handle, UUID) each.
pub fn characteristics(params: &[u8]) -> impl Iterator<Item = Characteristic> + '_ {
    let each = params.get(2).map_or(0, |&n| usize::from(n));
    let data = elements(params, 3);
    data.chunks_exact(each.max(1)).filter(move |_| each == 7 || each == 21).filter_map(|e| {
        Some(Characteristic {
            declaration: u16::from_le_bytes([e[0], e[1]]),
            properties: e[2],
            value: u16::from_le_bytes([e[3], e[4]]),
            uuid: Uuid::from_le(&e[5..])?,
        })
    })
}

/// `ATT_FIND_INFO_RESP` from a descriptor discovery: connection handle, format (1: 16-bit
/// UUIDs, 2: 128-bit), data length, then (handle, UUID) each.
pub fn descriptors(params: &[u8]) -> impl Iterator<Item = (u16, Uuid)> + '_ {
    let each = match params.get(2) {
        Some(1) => 4,
        Some(2) => 18,
        _ => 0,
    };
    let data = if each == 0 { &[][..] } else { elements(params, 3) };
    data.chunks_exact(each.max(1))
        .filter(move |_| each != 0)
        .filter_map(|e| Some((u16::from_le_bytes([e[0], e[1]]), Uuid::from_le(&e[2..])?)))
}

/// `ATT_FIND_BY_TYPE_VALUE_RESP` from a discovery by UUID: the first (found, group end)
/// handle pair, the service's range.
#[must_use]
pub fn service_range(params: &[u8]) -> Option<(u16, u16)> {
    let v = params.get(3..7)?;
    Some((u16::from_le_bytes([v[0], v[1]]), u16::from_le_bytes([v[2], v[3]])))
}

/// `ATT_READ_RESP`: the value read, cut to what the event holds.
#[must_use]
pub fn read_value(params: &[u8]) -> Option<&[u8]> {
    let n = usize::from(*params.get(2)?);
    params.get(3..3 + n.min(params.len() - 3))
}

/// `GATT_NOTIFICATION` or `GATT_INDICATION`: connection handle, attribute handle, a one-byte
/// length, the value.
#[must_use]
pub fn notification(params: &[u8]) -> Option<(u16, &[u8])> {
    let attr = u16::from_le_bytes([*params.get(2)?, *params.get(3)?]);
    let n = usize::from(*params.get(4)?);
    Some((attr, params.get(5..5 + n.min(params.len() - 5))?))
}

/// `GATT_ERROR_RESP`: the request opcode refused, the attribute and the ATT error code.
#[must_use]
pub fn gatt_error(params: &[u8]) -> Option<(u8, u16, u8)> {
    Some((*params.get(2)?, u16::from_le_bytes([*params.get(3)?, *params.get(4)?]), *params.get(5)?))
}

/// `GATT_PROC_COMPLETE`: 0 when the procedure succeeded, else its error.
#[must_use]
pub fn proc_complete(params: &[u8]) -> Option<u8> {
    params.get(2).copied()
}

/// `GAP_PROC_COMPLETE`: the procedure's code and its status (0 success).
#[must_use]
pub fn gap_proc_complete(params: &[u8]) -> Option<(u8, u8)> {
    Some((*params.first()?, *params.get(1)?))
}

/// `L2CAP_CONNECTION_UPDATE_REQ`: a peripheral asks the central for an interval range, a
/// latency and a timeout; the answer echoes the identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L2capUpdateRequest {
    pub handle: u16,
    pub identifier: u8,
    pub interval_min: u16,
    pub interval_max: u16,
    pub latency: u16,
    pub timeout: u16,
}

impl L2capUpdateRequest {
    /// Connection handle, identifier, L2CAP length, interval min and max,
    /// latency, timeout multiplier.
    #[must_use]
    pub fn decode(params: &[u8]) -> Option<Self> {
        let p = params.get(..13)?;
        let at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        Some(Self { handle: at(0), identifier: p[2], interval_min: at(5), interval_max: at(7), latency: at(9), timeout: at(11) })
    }
}
