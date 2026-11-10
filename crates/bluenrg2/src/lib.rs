//! The BlueNRG-2's packets as bytes, without the SPI bus or the link's state.
//!
//! ST's network-coprocessor firmware speaks HCI with ST's vendor commands
//! (OGF 0x3F, the ACI) and vendor events (event code 0xFF with a 16-bit
//! code), over SPI with a five-byte header each way. This crate holds the
//! numbers (from ST's public BlueNRG-1/2 ACI documentation), builds command
//! packets, measures packets in a read, and decodes every packet into an
//! event ([`parse`], then the vendor payload decoders).

#![no_std]

pub mod probe;

pub use bt_hci;
pub use bt_hci::param::{AddrKind, ConnHandle, EncryptionEnabledLevel, LeAdvEventKind, LeConnRole};

use bt_hci::param::{BdAddr, Status};
use bt_hci::FromHciBytes;

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

/// HCI commands and ST's vendor commands: the ones in use so far, not the whole ACI.
pub mod opcode {
    pub const HCI_READ_LOCAL_VERSION: u16 = 0x1001;
    pub const HCI_SET_EVENT_MASK: u16 = 0x0C01;
    pub const HCI_LE_RAND: u16 = 0x2018;
    pub const ACI_HAL_GET_FW_BUILD_NUMBER: u16 = 0xFC00;
    pub const ACI_HAL_GET_FIRMWARE_DETAILS: u16 = 0xFC01;
    pub const ACI_HAL_WRITE_CONFIG_DATA: u16 = 0xFC0C;
    pub const ACI_HAL_SET_TX_POWER_LEVEL: u16 = 0xFC0F;
    pub const ACI_HAL_GET_ANCHOR_PERIOD: u16 = 0xFC19;
    /// Reads only; the updater's start command reboots the chip into it.
    pub const ACI_HAL_GET_UPDATER_VERSION: u16 = 0xFC22;
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
    /// From the central: handle, interval min and max, latency, timeout, CE length min and max.
    pub const ACI_GAP_START_CONNECTION_UPDATE: u16 = 0xFC9E;
    pub const ACI_GAP_GET_BONDED_DEVICES: u16 = 0xFCA3;
    pub const ACI_GAP_NUMERIC_COMPARISON_CONFIRM: u16 = 0xFCA5;
    /// Removes one bonded device (identity address type, then the address); PM0257 doesn't list
    /// it, but stack build 2353 answers it with status 0 and drops only that bond.
    pub const ACI_GAP_REMOVE_BONDED_DEVICE: u16 = 0xFCAA;
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

/// Address types as command parameters carry them: own, identity and peer.
pub mod address {
    pub const PUBLIC: u8 = 0x00;
    /// The local device's own static random address ([`super::config::STATIC_RANDOM_ADDRESS`]),
    /// or a peer's random address, static or private.
    pub const RANDOM: u8 = 0x01;
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
    /// The central's answer to the local device's parameter request ([`super::L2capUpdateResponse`]).
    pub const L2CAP_CONNECTION_UPDATE_RESP: u16 = 0x0800;
    /// To the central: a peripheral asks for new connection parameters.
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

/// Why the chip says it started: `ACI_BLUE_INITIALIZED`'s reason code (ST's
/// `bluenrg1_events.h`), its one parameter. The chip sends the event after every reset, asked for
/// or not.
pub mod boot_reason {
    /// The stack started.
    pub const STARTED: u8 = 0x01;
    /// The updater runs, started by its command. On stack build 2353 that command is `0xFC20`,
    /// the opcode STM32WB uses for setting slave latency: the chip answered it with status 0, then
    /// this reason.
    pub const UPDATER_BY_COMMAND: u8 = 0x02;
    /// The updater runs because the stack's image is not marked valid.
    pub const UPDATER_BAD_FLAG: u8 = 0x03;
    /// The updater runs because the IRQ pin was high at reset.
    pub const UPDATER_BY_PIN: u8 = 0x04;
    pub const WATCHDOG: u8 = 0x05;
    pub const LOCKUP: u8 = 0x06;
    pub const BROWNOUT: u8 = 0x07;
    pub const CRASH: u8 = 0x08;
    pub const ECC_ERROR: u8 = 0x09;

    /// The updater runs instead of the stack: it answers normal commands with status `0x01`,
    /// and only a reset brings the stack back.
    #[must_use]
    pub const fn updater(reason: u8) -> bool {
        matches!(reason, UPDATER_BY_COMMAND | UPDATER_BAD_FLAG | UPDATER_BY_PIN)
    }

    /// A few words for the console.
    #[must_use]
    pub const fn describe(reason: u8) -> &'static str {
        match reason {
            STARTED => "the stack started",
            UPDATER_BY_COMMAND => "the updater, by command",
            UPDATER_BAD_FLAG => "the updater, the stack not marked valid",
            UPDATER_BY_PIN => "the updater, by the IRQ pin",
            WATCHDOG => "a watchdog reset",
            LOCKUP => "a lockup reset",
            BROWNOUT => "a brownout reset",
            CRASH => "a crash reset",
            ECC_ERROR => "an ECC error reset",
            _ => "a reason unknown",
        }
    }
}

/// `ACI_BLUE_INITIALIZED`'s reason ([`boot_reason`]); 0 if the event is empty.
#[must_use]
pub fn blue_initialized(params: &[u8]) -> u8 {
    params.first().copied().unwrap_or(0)
}

/// A status or reason as the chip gives it (ST's `tBleStatus`): HCI's error codes below 0x40,
/// ST's own (`ble_status.h`) from 0x40, which bt-hci's `Status` would misname.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BleStatus(u8);

impl BleStatus {
    pub const SUCCESS: Self = Self(0);

    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn into_inner(self) -> u8 {
        self.0
    }
}

impl core::fmt::Debug for BleStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 < 0x40 {
            core::fmt::Debug::fmt(&Status::new(self.0), f)
        } else {
            write!(f, "BleStatus(0x{:02x})", self.0)
        }
    }
}

/// ST's command status codes (`ble_status.h`), HCI's among them.
pub mod status {
    pub const COMMAND_DISALLOWED: u8 = 0x0C;
    pub const REMOTE_USER_TERMINATED: u8 = 0x13;
    /// No TX buffers for now; `GATT_TX_POOL_AVAILABLE` follows when there are.
    pub const INSUFFICIENT_RESOURCES: u8 = 0x64;
}

/// `ACI_GATT_ADD_SERVICE`'s and `ACI_GATT_ADD_CHAR`'s UUID types.
pub mod uuid_type {
    pub const U16: u8 = 0x01;
    pub const U128: u8 = 0x02;
}

/// `ACI_GATT_ADD_SERVICE`'s service type.
pub const PRIMARY_SERVICE: u8 = 0x01;

/// `ACI_GATT_ADD_CHAR`'s `GATT_Evt_Mask`.
pub mod evt_mask {
    pub const NONE: u8 = 0x00;
    pub const ATTRIBUTE_WRITE: u8 = 0x01;
}

/// `ACI_GATT_ADD_CHAR`'s smallest `Enc_Key_Size`, and its `Is_Variable`.
pub const MIN_ENC_KEY_SIZE: u8 = 7;
pub const VARIABLE_LENGTH: u8 = 0x01;

/// `ACI_GAP_INIT`'s privacy flag.
pub const PRIVACY_DISABLED: u8 = 0x00;

/// `ACI_GAP_SET_DISCOVERABLE`'s connectable undirected advertising, and its filter policy.
pub const ADV_IND: u8 = 0x00;
pub const NO_FILTER: u8 = 0x00;

/// `ACI_GAP_START_GENERAL_DISCOVERY_PROC`'s `Filter_Duplicates`.
pub const FILTER_DUPLICATES: u8 = 0x01;

/// HCI packet indicators (Core Vol 4 Part A 2).
pub mod packet {
    pub const COMMAND: u8 = 0x01;
    pub const ACL: u8 = 0x02;
    pub const EVENT: u8 = 0x04;
}

/// HCI event codes (Core Vol 4 Part E 7.7), and ST's vendor event.
pub mod event_code {
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    pub const ENCRYPTION_CHANGE: u8 = 0x08;
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    pub const COMMAND_STATUS: u8 = 0x0F;
    pub const ENCRYPTION_KEY_REFRESH_COMPLETE: u8 = 0x30;
    pub const LE_META: u8 = 0x3E;
    pub const VENDOR: u8 = 0xFF;
}

/// LE Meta subevent codes (Core Vol 4 Part E 7.7.65).
pub mod le_subevent {
    pub const CONNECTION_COMPLETE: u8 = 0x01;
    pub const ADVERTISING_REPORT: u8 = 0x02;
    pub const CONNECTION_UPDATE_COMPLETE: u8 = 0x03;
    pub const DATA_LENGTH_CHANGE: u8 = 0x07;
    pub const ENHANCED_CONNECTION_COMPLETE: u8 = 0x0A;
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
        const USE_FIXED_PIN: u8 = 0x00;
        const ASK_FOR_PIN: u8 = 0x01;
        let (use_fixed, pin) = match self.fixed_pin {
            Some(pin) => (USE_FIXED_PIN, pin),
            None => (ASK_FOR_PIN, 0),
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

/// An HCI command packet (`01`, opcode, length, parameters) into `out`;
/// returns its length. Parameters past 255 bytes, or past `out`, are cut.
pub fn command(opcode: u16, params: &[u8], out: &mut [u8]) -> usize {
    let n = params.len().min(MAX_COMMAND_PARAMS).min(out.len().saturating_sub(COMMAND_HEADER_LEN));
    if out.len() < COMMAND_HEADER_LEN {
        return 0;
    }
    out[0] = packet::COMMAND;
    out[1..3].copy_from_slice(&opcode.to_le_bytes());
    out[3] = u8::try_from(n).unwrap_or(0);
    out[COMMAND_HEADER_LEN..COMMAND_HEADER_LEN + n].copy_from_slice(&params[..n]);
    COMMAND_HEADER_LEN + n
}

/// The indicator, opcode and length before a command's parameters.
pub const COMMAND_HEADER_LEN: usize = 4;
pub const MAX_COMMAND_PARAMS: usize = u8::MAX as usize;
/// The longest packet [`command`] writes.
pub const MAX_COMMAND: usize = COMMAND_HEADER_LEN + MAX_COMMAND_PARAMS;

/// The length of the HCI packet at the start of `p`, clamped to `p`; 0 if
/// empty. A read may hold several packets back to back.
#[must_use]
pub fn packet_len(p: &[u8]) -> usize {
    match p {
        [packet::EVENT, _, len, ..] => (3 + usize::from(*len)).min(p.len()),
        [packet::ACL, _, _, lo, hi, ..] => (5 + usize::from(u16::from_le_bytes([*lo, *hi]))).min(p.len()),
        _ => p.len(),
    }
}

/// True when the packet's own length field fits inside what was read.
#[must_use]
pub fn packet_whole(p: &[u8]) -> bool {
    match p {
        [packet::EVENT, _, len, ..] => 3 + usize::from(*len) <= p.len(),
        [packet::ACL, _, _, lo, hi, ..] => 5 + usize::from(u16::from_le_bytes([*lo, *hi])) <= p.len(),
        _ => true,
    }
}

/// A device address as the air carries it: little-endian, and public or random.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Address {
    /// [`AddrKind::PUBLIC`] or [`AddrKind::RANDOM`]; a resolved identity's type is given as one of these.
    pub kind: AddrKind,
    pub bytes: [u8; 6],
}

/// A connection's interval (1.25 ms units), peripheral latency and supervision timeout (10 ms units).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnTiming {
    pub conn_interval: u16,
    pub peripheral_latency: u16,
    pub supervision_timeout: u16,
}

/// The event in one HCI packet, by section of the Core Specification, Vol 4 Part E, whose
/// layouts ST's `bluenrg1_events.h` follows. A connection handle above 0x0EFF makes it `Malformed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// Command Complete (7.7.14): the opcode answered, its status, the return parameters.
    CommandComplete { cmd_opcode: u16, status: BleStatus, return_param_bytes: &'a [u8] },
    /// Command Status (7.7.15): a command accepted or refused, results to follow.
    CommandStatus { cmd_opcode: u16, status: BleStatus },
    /// LE (Enhanced) Connection Complete (7.7.65.1, 7.7.65.10) with success: the handle, the
    /// local device's role, the peer, and the timing when the packet carries it.
    LeConnectionComplete { handle: ConnHandle, role: LeConnRole, peer: Address, timing: Option<ConnTiming> },
    /// LE (Enhanced) Connection Complete with an error: no connection formed.
    LeConnectionFailed { status: BleStatus },
    /// LE Connection Update Complete (7.7.65.3) with success: interval (1.25 ms units), latency,
    /// supervision timeout (10 ms units).
    LeConnectionUpdateComplete { handle: ConnHandle, conn_interval: u16, peripheral_latency: u16, supervision_timeout: u16 },
    /// LE Connection Update Complete with an error: the link keeps the parameters it had.
    LeConnectionUpdateFailed { handle: ConnHandle, status: BleStatus },
    /// Disconnection Complete (7.7.5): a status other than success is a disconnection that
    /// failed, the link still up; else the reason.
    DisconnectionComplete { handle: ConnHandle, status: BleStatus, reason: BleStatus },
    /// Encryption Change (7.7.8): a status other than success is a change that failed.
    EncryptionChange { handle: ConnHandle, status: BleStatus, enabled: EncryptionEnabledLevel },
    /// Encryption Key Refresh Complete (7.7.39): an encrypted link encrypted again, a status
    /// other than success a refresh that failed.
    EncryptionKeyRefresh { handle: ConnHandle, status: BleStatus },
    /// LE Data Length Change (7.7.65.7): the largest payload each way, octets and microseconds.
    LeDataLengthChange { handle: ConnHandle, max_tx_octets: u16, max_tx_time: u16, max_rx_octets: u16, max_rx_time: u16 },
    /// LE Advertising Report (7.7.65.2): one or more reports ([`reports`]).
    LeAdvertisingReport { data: &'a [u8] },
    /// One of ST's events (event code 0xFF): its code and the bytes after it.
    Vendor { code: u16, params: &'a [u8] },
    /// Another LE meta event, by subevent code.
    LeUnknown { subevent_code: u8 },
    /// Another HCI event, by event code.
    Unknown { code: u8 },
    /// Not an HCI event, cut short by the end of the read, or not fitting its layout.
    Malformed(&'a [u8]),
}

impl Event<'_> {
    /// For an Encryption Change or Key Refresh Complete, the link's handle and whether it is
    /// encrypted now: only on success, with encryption on.
    #[must_use]
    pub fn encryption(&self) -> Option<(ConnHandle, bool)> {
        match *self {
            Event::EncryptionChange { handle, status, enabled } => Some((handle, status == BleStatus::SUCCESS && enabled != EncryptionEnabledLevel::Off)),
            Event::EncryptionKeyRefresh { handle, status } => Some((handle, status == BleStatus::SUCCESS)),
            _ => None,
        }
    }
}

/// The event in one HCI packet, read no further than the packet's own length.
#[must_use]
pub fn parse(p: &[u8]) -> Event<'_> {
    if !packet_whole(p) {
        return Event::Malformed(p);
    }
    let p = &p[..packet_len(p)];
    let event = match p {
        [packet::EVENT, code, _, params @ ..] => standard(*code, params),
        _ => None,
    };
    event.unwrap_or(Event::Malformed(p))
}

/// The value at the start of `b`, the bytes after it ignored.
fn first<'a, T: FromHciBytes<'a>>(b: &'a [u8]) -> Option<T> {
    T::from_hci_bytes(b).ok().map(|(v, _)| v)
}

/// An event by its code and parameters; `None` when they do not fit its layout.
fn standard(code: u8, params: &[u8]) -> Option<Event<'_>> {
    use bt_hci::event as hci;
    use event_code as ev;
    Some(match code {
        ev::COMMAND_COMPLETE => {
            let e: hci::CommandComplete = first(params)?;
            let (status, return_param_bytes) = u8::from_hci_bytes(&params[params.len() - e.bytes.len()..]).ok()?;
            Event::CommandComplete { cmd_opcode: e.cmd_opcode.to_raw(), status: BleStatus(status), return_param_bytes }
        }
        ev::COMMAND_STATUS => {
            let e: hci::CommandStatus = first(params)?;
            Event::CommandStatus { cmd_opcode: e.cmd_opcode.to_raw(), status: ble(e.status) }
        }
        ev::DISCONNECTION_COMPLETE => {
            let e: hci::DisconnectionComplete = first(params)?;
            Event::DisconnectionComplete { handle: link(e.handle.raw())?, status: ble(e.status), reason: ble(e.reason) }
        }
        ev::ENCRYPTION_CHANGE => {
            let e: hci::EncryptionChangeV1 = first(params)?;
            Event::EncryptionChange { handle: link(e.handle.raw())?, status: ble(e.status), enabled: e.enabled }
        }
        ev::ENCRYPTION_KEY_REFRESH_COMPLETE => {
            let e: hci::EncryptionKeyRefreshComplete = first(params)?;
            Event::EncryptionKeyRefresh { handle: link(e.handle.raw())?, status: ble(e.status) }
        }
        ev::LE_META => le_meta(params)?,
        ev::VENDOR => {
            let (code, params) = u16::from_hci_bytes(params).ok()?;
            Event::Vendor { code, params }
        }
        code => Event::Unknown { code },
    })
}

/// An LE Meta event by its subevent code and parameters.
fn le_meta(params: &[u8]) -> Option<Event<'_>> {
    use bt_hci::event::le;
    use le_subevent as sub;
    let (&code, params) = params.split_first()?;
    Some(match code {
        sub::CONNECTION_COMPLETE | sub::ENHANCED_CONNECTION_COMPLETE => connection_complete(code, params)?,
        sub::CONNECTION_UPDATE_COMPLETE => {
            let (status, rest) = Status::from_hci_bytes(params).ok()?;
            if status == Status::SUCCESS {
                let e: le::LeConnectionUpdateComplete = first(params)?;
                Event::LeConnectionUpdateComplete {
                    handle: link(e.handle.raw())?,
                    conn_interval: e.conn_interval.as_u16(),
                    peripheral_latency: e.peripheral_latency,
                    supervision_timeout: e.supervision_timeout.as_u16(),
                }
            } else {
                // A failure's interval, latency and timeout mean nothing (Core Vol 4 Part E §7.7.65.3).
                Event::LeConnectionUpdateFailed { handle: link(first(rest)?)?, status: ble(status) }
            }
        }
        sub::DATA_LENGTH_CHANGE => {
            let e: le::LeDataLengthChange = first(params)?;
            Event::LeDataLengthChange {
                handle: link(e.handle.raw())?,
                max_tx_octets: e.max_tx_octets,
                max_tx_time: e.max_tx_time,
                max_rx_octets: e.max_rx_octets,
                max_rx_time: e.max_rx_time,
            }
        }
        sub::ADVERTISING_REPORT => {
            first::<le::LeAdvertisingReport>(params)?;
            Event::LeAdvertisingReport { data: params }
        }
        subevent_code => Event::LeUnknown { subevent_code },
    })
}

/// LE (Enhanced) Connection Complete: a failure by its status alone, a success by the fields
/// both layouts share, and the interval, latency and timeout when the whole event decodes.
fn connection_complete(code: u8, params: &[u8]) -> Option<Event<'_>> {
    use bt_hci::event::le;
    let (status, rest) = Status::from_hci_bytes(params).ok()?;
    if status != Status::SUCCESS {
        // No connection formed, so the other fields mean nothing (Core Vol 4 Part E §7.7.65.1).
        return Some(Event::LeConnectionFailed { status: ble(status) });
    }
    // The link exists on the chip even when the rest is short or reserved, so it is still taken.
    let (handle, rest) = u16::from_hci_bytes(rest).ok()?;
    let (role, rest) = u8::from_hci_bytes(rest).ok()?;
    let (kind, rest) = AddrKind::from_hci_bytes(rest).ok()?;
    let peer: BdAddr = first(rest)?;
    let timing = if code == le_subevent::CONNECTION_COMPLETE {
        first::<le::LeConnectionComplete>(params).map(|e| (e.conn_interval, e.peripheral_latency, e.supervision_timeout))
    } else {
        first::<le::LeEnhancedConnectionComplete>(params).map(|e| (e.conn_interval, e.peripheral_latency, e.supervision_timeout))
    };
    Some(Event::LeConnectionComplete {
        handle: link(handle)?,
        role: if role == 0 { LeConnRole::Central } else { LeConnRole::Peripheral },
        // An Enhanced event says 2 or 3 for an identity it resolved: still public or random.
        peer: Address { kind: AddrKind::new(kind.as_raw() & 0x01), bytes: peer.into_inner() },
        timing: timing.map(|(interval, latency, timeout)| ConnTiming {
            conn_interval: interval.as_u16(),
            peripheral_latency: latency,
            supervision_timeout: timeout.as_u16(),
        }),
    })
}

/// One advertising report: what kind, who, the advertising data and the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report<'a> {
    pub kind: LeAdvEventKind,
    pub address: Address,
    pub data: &'a [u8],
    pub rssi: i8,
}

/// The reports in an LE Advertising Report's parameters (after the subevent code): a count,
/// then each report whole (type, address type, address, length, data, RSSI), as ST's stack
/// packs them. A report cut short, or of a kind the specification doesn't define, ends the list.
pub fn reports(data: &[u8]) -> impl Iterator<Item = Report<'_>> + '_ {
    let count = data.first().copied().unwrap_or(0);
    let mut at = 1;
    (0..count).map_while(move |_| {
        let head = data.get(at..at + 9)?;
        let len = usize::from(head[8]);
        let body = data.get(at + 9..at + 9 + len)?;
        let rssi = *data.get(at + 9 + len)?;
        let kind = first(&head[..1])?;
        at += 10 + len;
        Some(Report {
            kind,
            address: Address { kind: AddrKind::new(head[1] & 0x01), bytes: [head[2], head[3], head[4], head[5], head[6], head[7]] },
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

/// The connection handle that starts most vendor events; `None` if short or above 0x0EFF.
#[must_use]
pub fn connection_handle(params: &[u8]) -> Option<ConnHandle> {
    link(first(params)?)
}

fn ble(status: Status) -> BleStatus {
    BleStatus(status.into_inner())
}

/// A connection handle, which is at most 0x0EFF (Core Vol 4 Part E 5.4.2).
fn link(handle: u16) -> Option<ConnHandle> {
    (handle <= 0x0EFF).then(|| ConnHandle::new(handle))
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
pub fn numeric_comparison(params: &[u8]) -> Option<(ConnHandle, u32)> {
    let v = params.get(2..6)?;
    Some((connection_handle(params)?, u32::from_le_bytes([v[0], v[1], v[2], v[3]])))
}

/// The most a Command Complete carries after its status: an event's 255 parameter bytes less the
/// packet count, the opcode and the status (Core Vol 4 Part E 5.4.4, 7.7.14).
pub const RETURN_PARAMS_MAX: usize = 255 - 4;

/// The identity addresses in an `ACI_GAP_GET_BONDED_DEVICES` reply, never past its end.
pub fn bonded_devices(params: &[u8]) -> impl Iterator<Item = Address> + '_ {
    let count = usize::from(params.first().copied().unwrap_or(0));
    params.get(1..).unwrap_or(&[]).as_chunks::<7>().0.iter().take(count).map(|e| {
        let mut bytes = [0u8; 6];
        bytes.copy_from_slice(&e[1..7]);
        Address { kind: AddrKind::new(e[0] & 0x01), bytes }
    })
}

/// The longest [`Address::text`].
pub const ADDRESS_TEXT_MAX: usize = 26;

impl Address {
    /// `0A:1B:2C:3D:4E:5F`, then ` (random)` for a random one.
    pub fn text(self, out: &mut [u8; ADDRESS_TEXT_MAX]) -> &str {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in self.bytes.iter().rev().enumerate() {
            out[3 * i] = HEX[usize::from(b >> 4)];
            out[3 * i + 1] = HEX[usize::from(b & 0x0F)];
            out[3 * i + 2] = b':';
        }
        let mut len = 17;
        if self.kind == AddrKind::RANDOM {
            out[len..].copy_from_slice(b" (random)");
            len = ADDRESS_TEXT_MAX;
        }
        core::str::from_utf8(&out[..len]).unwrap_or_default()
    }
}

/// An [`Address::text`] read back, colons or dashes, either case.
#[must_use]
pub fn parse_address(text: &str) -> Option<Address> {
    let text = text.trim();
    let (text, kind) = match text.strip_suffix("(random)") {
        Some(rest) => (rest.trim_end(), AddrKind::RANDOM),
        None => (text, AddrKind::PUBLIC),
    };
    let mut bytes = [0u8; 6];
    let mut parts = text.split([':', '-']);
    for i in (0..6).rev() {
        let part = parts.next()?;
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        bytes[i] = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(Address { kind, bytes })
}

/// `GAP_PAIRING_COMPLETE`'s status for a central that encrypted with a key the local device no longer holds.
pub const PAIRING_LTK_MISSING: u8 = 0x03;

/// `GAP_PAIRING_COMPLETE`: status (0 success) and reason.
#[must_use]
pub fn pairing_complete(params: &[u8]) -> Option<(u8, u8)> {
    Some((*params.get(2)?, *params.get(3)?))
}

/// The LE `ATT_MTU` before an exchange (Core Vol 3 Part G 5.2.1).
pub const ATT_DEFAULT_MTU: usize = 23;
/// A write's or a notification's opcode and handle, ahead of its value.
pub const ATT_HEADER_LEN: usize = 3;

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

/// `GATT_PROC_COMPLETE`: success, or the procedure's error.
#[must_use]
pub fn proc_complete(params: &[u8]) -> Option<BleStatus> {
    params.get(2).copied().map(BleStatus)
}

/// `GAP_PROC_COMPLETE`: the procedure's code and its status.
#[must_use]
pub fn gap_proc_complete(params: &[u8]) -> Option<(u8, BleStatus)> {
    Some((*params.first()?, BleStatus(*params.get(1)?)))
}

/// `L2CAP_CONNECTION_UPDATE_REQ`: a peripheral asks the central for an interval range, a
/// latency and a timeout; the answer echoes the identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L2capUpdateRequest {
    pub handle: ConnHandle,
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
        Some(Self { handle: connection_handle(p)?, identifier: p[2], interval_min: at(5), interval_max: at(7), latency: at(9), timeout: at(11) })
    }
}

/// `L2CAP_CONNECTION_UPDATE_RESP`: the central's answer to the local device's parameter request
/// (Core Specification, Vol 3, Part A, 4.21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L2capUpdateResponse {
    pub handle: ConnHandle,
    pub result: u16,
}

impl L2capUpdateResponse {
    /// Connection handle, result (ST's `bluenrg1_events.h`).
    #[must_use]
    pub fn decode(params: &[u8]) -> Option<Self> {
        let p = params.get(..4)?;
        Some(Self { handle: connection_handle(p)?, result: u16::from_le_bytes([p[2], p[3]]) })
    }

    /// Result 0; anything else is a refusal.
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.result == 0
    }
}
