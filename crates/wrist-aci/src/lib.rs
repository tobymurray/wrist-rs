//! The BlueNRG-2's packets as bytes, without the SPI bus or the link's state.
//!
//! ST's network-coprocessor firmware speaks HCI with ST's vendor commands
//! (OGF 0x3F, the ACI) and vendor events (event code 0xFF with a 16-bit
//! code), over SPI with a five-byte header each way. This crate holds the
//! numbers (from ST's public BlueNRG-1/2 ACI documentation), builds command
//! packets, measures packets in a read, and decodes the events the image
//! uses into types ([`parse`], then the vendor payload decoders). The image's
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
    pub const ACI_GAP_TERMINATE: u16 = 0xFC93;
    pub const ACI_GAP_CLEAR_SECURITY_DB: u16 = 0xFC94;
    pub const ACI_GAP_ALLOW_REBOND: u16 = 0xFC95;
    pub const ACI_GAP_GET_BONDED_DEVICES: u16 = 0xFCA3;
    pub const ACI_GAP_NUMERIC_COMPARISON_CONFIRM: u16 = 0xFCA5;
    pub const ACI_GATT_INIT: u16 = 0xFD01;
    pub const ACI_GATT_ADD_SERVICE: u16 = 0xFD02;
    pub const ACI_GATT_ADD_CHAR: u16 = 0xFD04;
    pub const ACI_GATT_UPDATE_CHAR_VALUE: u16 = 0xFD06;
    pub const ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ: u16 = 0xFD81;
}

/// `ACI_GATT_ADD_CHAR`'s security permissions (ST's `ble_defs.h`). ST's stack applies them to
/// writes with response only; a write without response arrives whatever the link, so the
/// application checks the link for those itself.
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
    pub const GAP_BOND_LOST: u16 = 0x0405;
    pub const GAP_NUMERIC_COMPARISON_VALUE: u16 = 0x0409;
    pub const GATT_ATTRIBUTE_MODIFIED: u16 = 0x0C01;
    pub const GATT_PROC_TIMEOUT: u16 = 0x0C02;
    pub const ATT_EXCHANGE_MTU_RESP: u16 = 0x0C03;
    pub const GATT_PROC_COMPLETE: u16 = 0x0C10;
    pub const GATT_TX_POOL_AVAILABLE: u16 = 0x0C16;
}

/// ST's command status codes (`ble_status.h`).
pub mod status {
    /// No TX buffers for now; `GATT_TX_POOL_AVAILABLE` follows when there are.
    pub const INSUFFICIENT_RESOURCES: u8 = 0x64;
}

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

/// The events the image acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// Command Complete: the opcode answered, its status, the return parameters.
    CommandComplete { opcode: u16, status: u8, params: &'a [u8] },
    /// Command Status: a command accepted (0) or refused, results to follow.
    CommandStatus { opcode: u16, status: u8 },
    /// LE (Enhanced) Connection Complete with success: the handle, and the
    /// interval (1.25 ms units) and latency when the packet carries them.
    Connected { handle: u16, interval: Option<(u16, u16)> },
    /// LE Connection Update Complete with success: interval (1.25 ms units), latency.
    ConnectionUpdate { interval: u16, latency: u16 },
    /// Disconnection Complete, with its reason (0x13: the peer hung up).
    Disconnected { reason: u8 },
    /// Encryption Change: 1 on, 0 off.
    Encryption { on: u8 },
    /// One of ST's events: its code and the bytes after it.
    Vendor { code: u16, params: &'a [u8] },
}

/// The event in one HCI packet, if it is one the image acts on.
#[must_use]
pub fn parse(p: &[u8]) -> Option<Event<'_>> {
    match p {
        [0x04, 0x0E, _, _num, lo, hi, status, params @ ..] => {
            Some(Event::CommandComplete { opcode: u16::from_le_bytes([*lo, *hi]), status: *status, params })
        }
        [0x04, 0x0F, _, status, _num, lo, hi, ..] => {
            Some(Event::CommandStatus { opcode: u16::from_le_bytes([*lo, *hi]), status: *status })
        }
        // Status, handle, role, peer address type and address, then (Enhanced)
        // two resolvable addresses, then interval and latency.
        [0x04, 0x3E, _, sub @ (0x01 | 0x0A), 0x00, lo, hi, ..] => {
            let at = if *sub == 0x01 { 15 } else { 27 };
            Some(Event::Connected {
                handle: u16::from_le_bytes([*lo, *hi]),
                interval: p.get(at..at + 4).map(|v| (u16::from_le_bytes([v[0], v[1]]), u16::from_le_bytes([v[2], v[3]]))),
            })
        }
        [0x04, 0x3E, _, 0x03, 0x00, _, _, iv_lo, iv_hi, lat_lo, lat_hi, ..] => Some(Event::ConnectionUpdate {
            interval: u16::from_le_bytes([*iv_lo, *iv_hi]),
            latency: u16::from_le_bytes([*lat_lo, *lat_hi]),
        }),
        [0x04, 0x05, _, _status, _lo, _hi, reason] => Some(Event::Disconnected { reason: *reason }),
        [0x04, 0x08, _, _status, _lo, _hi, on] => Some(Event::Encryption { on: *on }),
        [0x04, 0xFF, _, lo, hi, params @ ..] => Some(Event::Vendor { code: u16::from_le_bytes([*lo, *hi]), params }),
        _ => None,
    }
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
