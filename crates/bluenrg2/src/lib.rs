//! The BlueNRG-2's packets as bytes, without the SPI bus or the link's state.
//!
//! ST's network-coprocessor firmware speaks HCI with ST's vendor commands (OGF 0x3F, the ACI) and
//! vendor events (event code 0xFF with a 16-bit code). This crate holds the numbers from ST's
//! BlueNRG-1/2 ACI documentation, builds command packets ([`command`]), measures packets in a read
//! ([`packet_len`]), and decodes each into an [`Event`] ([`parse`]).
//!
//! # The SPI exchange
//!
//! The bus is yours to drive; ST's X-CUBE-BLE2 `hci_tl_interface.c` (`HCI_TL_SPI_Send`,
//! `HCI_TL_SPI_Receive`) is the reference. With CS low, the host clocks a five-byte header out
//! while the chip clocks its answer in ([`header_sizes`]).
//!
//! - **Write**: CS low, then wait for IRQ to rise; exchange [`HEADER_WRITE`]; unless the room the
//!   chip answers holds the whole packet, CS high and start again; clock the packet out; CS high;
//!   wait for IRQ to fall.
//! - **Read**, when IRQ is high: CS low; exchange [`HEADER_READ`]; clock in as many bytes as the
//!   chip answers waiting; wait for IRQ to fall; CS high. Those bytes are one or more packets back
//!   to back.

#![no_std]

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

pub mod hal;
mod vendor_event;

pub use bt_hci;
pub use bt_hci::data::{AclBroadcastFlag, AclPacketBoundary};
pub use bt_hci::param::{
    AddrKind, BdAddr, ConnHandle, Duration, EncryptionEnabledLevel, LeAdvEventKind, LeConnRole,
};

pub use vendor_event::{Characteristics, Descriptors, HandleRanges, Services, VendorEvent};

use bt_hci::data::AclPacket;
use bt_hci::param::Status;
use bt_hci::FromHciBytes;

/// The SPI header the host sends to write a packet; the chip answers in the same five bytes.
pub const HEADER_WRITE: [u8; 5] = [0x0A, 0, 0, 0, 0];
/// The SPI header the host sends to read; the chip answers in the same five bytes.
pub const HEADER_READ: [u8; 5] = [0x0B, 0, 0, 0, 0];

/// The chip's answer to a header, in bytes: the room in its write buffer, and what waits to be
/// read. ST's driver reads nothing else from the answer.
#[must_use]
pub fn header_sizes(header: &[u8; 5]) -> (usize, usize) {
    (
        usize::from(u16::from_le_bytes([header[1], header[2]])),
        usize::from(u16::from_le_bytes([header[3], header[4]])),
    )
}

/// HCI opcodes and a subset of the ACI; [`command`] builds any opcode.
pub mod opcode {
    pub const HCI_READ_LOCAL_VERSION: u16 = 0x1001;
    pub const HCI_SET_EVENT_MASK: u16 = 0x0C01;
    pub const HCI_LE_RAND: u16 = 0x2018;
    pub const ACI_HAL_GET_FW_BUILD_NUMBER: u16 = 0xFC00;
    pub const ACI_HAL_GET_FIRMWARE_DETAILS: u16 = 0xFC01;
    pub const ACI_HAL_WRITE_CONFIG_DATA: u16 = 0xFC0C;
    pub const ACI_HAL_SET_TX_POWER_LEVEL: u16 = 0xFC0F;
    pub const ACI_HAL_GET_LINK_STATUS: u16 = 0xFC17;
    pub const ACI_HAL_GET_ANCHOR_PERIOD: u16 = 0xFC19;
    /// Reads only; [`ACI_UPDATER_START`] reboots the chip into the updater.
    pub const ACI_HAL_GET_UPDATER_VERSION: u16 = 0xFC22;
    /// Reboots the chip into its updater: status 0, then `BLUE_INITIALIZED` with
    /// [`super::BootReason::UPDATER_BY_COMMAND`].
    pub const ACI_UPDATER_START: u16 = 0xFC20;
    pub const ACI_GAP_SET_NON_DISCOVERABLE: u16 = 0xFC81;
    pub const ACI_GAP_SET_DISCOVERABLE: u16 = 0xFC83;
    pub const ACI_GAP_SET_IO_CAPABILITY: u16 = 0xFC85;
    pub const ACI_GAP_SET_AUTHENTICATION_REQUIREMENT: u16 = 0xFC86;
    pub const ACI_GAP_PASS_KEY_RESP: u16 = 0xFC88;
    pub const ACI_GAP_INIT: u16 = 0xFC8A;
    pub const ACI_GAP_SLAVE_SECURITY_REQ: u16 = 0xFC8D;
    /// A connection handle.
    pub const ACI_GAP_GET_SECURITY_LEVEL: u16 = 0xFC90;
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
    /// Removes one bonded device: identity address type, then the address (ST's `bluenrg1_types.h`,
    /// `aci_gap_remove_bonded_device_cp0`; PM0257 doesn't list it).
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
    /// As a central, the answer to a peripheral's request ([`super::VendorEvent::L2capUpdateRequest`]).
    pub const ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_RESP: u16 = 0xFD82;
}

/// `ACI_GAP_INIT`'s roles, a bitmask.
pub mod gap_role {
    pub const PERIPHERAL: u8 = 0x01;
    pub const BROADCASTER: u8 = 0x02;
    pub const CENTRAL: u8 = 0x04;
    pub const OBSERVER: u8 = 0x08;
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

/// A static random address (Core Vol 6 Part B 1.3.2.1) from the low 46 of 64 bits, nudged off
/// the two values the specification forbids.
#[must_use]
pub const fn static_random(bits: u64) -> BdAddr {
    let b = bits.to_le_bytes();
    let mut a = [b[0], b[1], b[2], b[3], b[4], b[5] | 0xC0];
    let rest = bits & 0x3FFF_FFFF_FFFF;
    if rest == 0 || rest == 0x3FFF_FFFF_FFFF {
        a[0] ^= 1;
    }
    BdAddr(a)
}

/// `ACI_GATT_ADD_CHAR`'s security permissions (ST's `ble_defs.h`).
pub mod permission {
    pub const NONE: u8 = 0x00;
    /// Reads only over a link encrypted with authenticated (MITM-protected) keys.
    pub const AUTHEN_READ: u8 = 0x01;
    /// Reads only once the application authorizes each.
    pub const AUTHOR_READ: u8 = 0x02;
    /// Reads only over an encrypted link.
    pub const ENCRY_READ: u8 = 0x04;
    /// Writes only over a link encrypted with authenticated (MITM-protected) keys.
    pub const AUTHEN_WRITE: u8 = 0x08;
    /// Writes only once the application authorizes each.
    pub const AUTHOR_WRITE: u8 = 0x10;
    /// Writes only over an encrypted link.
    pub const ENCRY_WRITE: u8 = 0x20;
    /// [`AUTHEN_WRITE`] and [`ENCRY_WRITE`] together: writes only over a link encrypted with
    /// authenticated keys.
    pub const AUTHENTICATED_WRITE: u8 = 0x28;
}

/// ST's vendor event codes (event 0xFF).
pub mod vendor {
    pub const BLUE_INITIALIZED: u16 = 0x0001;
    /// The chip dropped events because the host read too slowly.
    pub const BLUE_EVENTS_LOST: u16 = 0x0002;
    pub const GAP_PAIRING_COMPLETE: u16 = 0x0401;
    pub const GAP_PASS_KEY_REQ: u16 = 0x0402;
    /// A security request reached the central.
    pub const GAP_SLAVE_SECURITY_INITIATED: u16 = 0x0404;
    pub const GAP_BOND_LOST: u16 = 0x0405;
    /// A GAP procedure ended ([`super::VendorEvent::GapProcComplete`]).
    pub const GAP_PROC_COMPLETE: u16 = 0x0407;
    pub const GAP_NUMERIC_COMPARISON_VALUE: u16 = 0x0409;
    /// The central's answer to the local device's parameter request ([`super::VendorEvent::L2capUpdateResponse`]).
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

/// Why the chip says it started: `ACI_BLUE_INITIALIZED`'s reason code (ST's `bluenrg1_events.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BootReason(u8);

impl BootReason {
    /// The stack started.
    pub const STARTED: Self = Self(0x01);
    /// The updater runs, started by [`opcode::ACI_UPDATER_START`].
    pub const UPDATER_BY_COMMAND: Self = Self(0x02);
    /// The updater runs because the stack's image is not marked valid.
    pub const UPDATER_BAD_FLAG: Self = Self(0x03);
    /// The updater runs because the IRQ pin was high at reset.
    pub const UPDATER_BY_PIN: Self = Self(0x04);
    pub const WATCHDOG: Self = Self(0x05);
    pub const LOCKUP: Self = Self(0x06);
    pub const BROWNOUT: Self = Self(0x07);
    pub const CRASH: Self = Self(0x08);
    pub const ECC_ERROR: Self = Self(0x09);

    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn into_inner(self) -> u8 {
        self.0
    }

    /// The updater runs instead of the stack.
    #[must_use]
    pub const fn is_updater(self) -> bool {
        matches!(self.0, 0x02..=0x04)
    }
}

/// A status or reason as the chip gives it (ST's `tBleStatus`): HCI's error codes below 0x40,
/// ST's own (`ble_status.h`) from 0x40, which bt-hci's `Status` would misname.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BleStatus(u8);

impl BleStatus {
    pub const SUCCESS: Self = Self(0x00);
    pub const UNKNOWN_HCI_COMMAND: Self = Self(0x01);
    pub const UNKNOWN_CONNECTION_ID: Self = Self(0x02);
    pub const HARDWARE_FAILURE: Self = Self(0x03);
    pub const AUTHENTICATION_FAILURE: Self = Self(0x05);
    pub const KEY_MISSING: Self = Self(0x06);
    pub const MEMORY_CAPACITY_EXCEEDED: Self = Self(0x07);
    pub const CONNECTION_TIMEOUT: Self = Self(0x08);
    pub const COMMAND_DISALLOWED: Self = Self(0x0C);
    pub const UNSUPPORTED_FEATURE: Self = Self(0x11);
    pub const INVALID_HCI_CMD_PARAMS: Self = Self(0x12);
    pub const TERMINATED_REMOTE_USER: Self = Self(0x13);
    pub const TERMINATED_LOCAL_HOST: Self = Self(0x16);
    pub const UNSUPP_RMT_FEATURE: Self = Self(0x1A);
    pub const UNSPECIFIED: Self = Self(0x1F);
    pub const PROCEDURE_TIMEOUT: Self = Self(0x22);
    pub const INSTANT_PASSED: Self = Self(0x28);
    pub const PARAMETER_OUT_OF_RANGE: Self = Self(0x30);
    pub const HOST_BUSY_PAIRING: Self = Self(0x38);
    pub const CONTROLLER_BUSY: Self = Self(0x3A);
    pub const DIRECTED_ADVERTISING_TIMEOUT: Self = Self(0x3C);
    pub const CONNECTION_END_WITH_MIC_FAILURE: Self = Self(0x3D);
    pub const CONNECTION_FAILED_TO_ESTABLISH: Self = Self(0x3E);
    pub const STATUS_UNKNOWN_CONNECTION_ID: Self = Self(0x40);
    pub const FAILED: Self = Self(0x41);
    pub const INVALID_PARAMS: Self = Self(0x42);
    pub const BUSY: Self = Self(0x43);
    pub const PENDING: Self = Self(0x45);
    pub const NOT_ALLOWED: Self = Self(0x46);
    pub const ERROR: Self = Self(0x47);
    pub const OUT_OF_MEMORY: Self = Self(0x48);
    pub const INVALID_CID: Self = Self(0x50);
    pub const DEV_IN_BLACKLIST: Self = Self(0x59);
    pub const CSRK_NOT_FOUND: Self = Self(0x5A);
    pub const IRK_NOT_FOUND: Self = Self(0x5B);
    pub const DEV_NOT_FOUND: Self = Self(0x5C);
    pub const SEC_DB_FULL: Self = Self(0x5D);
    pub const DEV_NOT_BONDED: Self = Self(0x5E);
    pub const INSUFFICIENT_ENC_KEYSIZE: Self = Self(0x5F);
    pub const INVALID_HANDLE: Self = Self(0x60);
    pub const OUT_OF_HANDLE: Self = Self(0x61);
    pub const INVALID_OPERATION: Self = Self(0x62);
    pub const CHARAC_ALREADY_EXISTS: Self = Self(0x63);
    /// No TX buffers for now; `GATT_TX_POOL_AVAILABLE` follows when there are.
    pub const INSUFFICIENT_RESOURCES: Self = Self(0x64);
    pub const SEC_PERMISSION_ERROR: Self = Self(0x65);
    pub const ADDRESS_NOT_RESOLVED: Self = Self(0x70);
    pub const NO_VALID_SLOT: Self = Self(0x82);
    pub const SCAN_WINDOW_SHORT: Self = Self(0x83);
    pub const NEW_INTERVAL_FAILED: Self = Self(0x84);
    pub const INTERVAL_TOO_LARGE: Self = Self(0x85);
    pub const LENGTH_FAILED: Self = Self(0x86);
    pub const FLASH_READ_FAILED: Self = Self(0xFA);
    pub const FLASH_WRITE_FAILED: Self = Self(0xFB);
    pub const FLASH_ERASE_FAILED: Self = Self(0xFC);

    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn into_inner(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 == Self::SUCCESS.0
    }
}

/// ST's name for one of its own codes, from 0x40.
const fn st_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x40 => "STATUS_UNKNOWN_CONNECTION_ID",
        0x41 => "FAILED",
        0x42 => "INVALID_PARAMS",
        0x43 => "BUSY",
        0x45 => "PENDING",
        0x46 => "NOT_ALLOWED",
        0x47 => "ERROR",
        0x48 => "OUT_OF_MEMORY",
        0x50 => "INVALID_CID",
        0x59 => "DEV_IN_BLACKLIST",
        0x5A => "CSRK_NOT_FOUND",
        0x5B => "IRK_NOT_FOUND",
        0x5C => "DEV_NOT_FOUND",
        0x5D => "SEC_DB_FULL",
        0x5E => "DEV_NOT_BONDED",
        0x5F => "INSUFFICIENT_ENC_KEYSIZE",
        0x60 => "INVALID_HANDLE",
        0x61 => "OUT_OF_HANDLE",
        0x62 => "INVALID_OPERATION",
        0x63 => "CHARAC_ALREADY_EXISTS",
        0x64 => "INSUFFICIENT_RESOURCES",
        0x65 => "SEC_PERMISSION_ERROR",
        0x70 => "ADDRESS_NOT_RESOLVED",
        0x82 => "NO_VALID_SLOT",
        0x83 => "SCAN_WINDOW_SHORT",
        0x84 => "NEW_INTERVAL_FAILED",
        0x85 => "INTERVAL_TOO_LARGE",
        0x86 => "LENGTH_FAILED",
        0xFA => "FLASH_READ_FAILED",
        0xFB => "FLASH_WRITE_FAILED",
        0xFC => "FLASH_ERASE_FAILED",
        _ => return None,
    })
}

/// HCI's name below 0x40, ST's from it, else the code.
impl core::fmt::Debug for BleStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 < 0x40 {
            core::fmt::Debug::fmt(&Status::new(self.0), f)
        } else if let Some(name) = st_name(self.0) {
            f.write_str(name)
        } else {
            write!(f, "BleStatus(0x{:02x})", self.0)
        }
    }
}

/// HCI's name below 0x40, ST's from it, then the code: `OUT_OF_MEMORY (0x48)`.
impl core::fmt::Display for BleStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 < 0x40 {
            write!(f, "{:?} (0x{:02x})", Status::new(self.0), self.0)
        } else if let Some(name) = st_name(self.0) {
            write!(f, "{name} (0x{:02x})", self.0)
        } else {
            write!(f, "ST status 0x{:02x}", self.0)
        }
    }
}

impl core::error::Error for BleStatus {}

/// `ACI_GATT_ADD_SERVICE`'s and `ACI_GATT_ADD_CHAR`'s UUID types.
pub mod uuid_type {
    pub const U16: u8 = 0x01;
    pub const U128: u8 = 0x02;
}

/// `ACI_GATT_ADD_SERVICE`'s service type.
pub mod service_type {
    pub const PRIMARY: u8 = 0x01;
    pub const SECONDARY: u8 = 0x02;
}

/// `ACI_GATT_ADD_CHAR`'s `GATT_Evt_Mask`.
pub mod evt_mask {
    pub const NONE: u8 = 0x00;
    pub const ATTRIBUTE_WRITE: u8 = 0x01;
}

/// Encryption key size bounds, in bytes (Core Vol 3 Part H 2.3.4).
pub mod enc_key_size {
    pub const MIN: u8 = 7;
    pub const MAX: u8 = 16;
}

/// `ACI_GATT_ADD_CHAR`'s `Is_Variable`.
pub mod value_length {
    pub const CONSTANT: u8 = 0x00;
    pub const VARIABLE: u8 = 0x01;
}

/// `ACI_GAP_INIT`'s privacy flag.
pub mod privacy {
    pub const DISABLED: u8 = 0x00;
}

/// `ACI_GAP_SET_DISCOVERABLE`'s advertising type.
pub mod adv_type {
    /// Connectable undirected.
    pub const ADV_IND: u8 = 0x00;
}

/// `ACI_GAP_SET_DISCOVERABLE`'s advertising filter policy.
pub mod adv_filter_policy {
    pub const NO_FILTER: u8 = 0x00;
}

/// `ACI_GAP_START_GENERAL_DISCOVERY_PROC`'s `Filter_Duplicates`.
pub mod filter_duplicates {
    pub const DISABLED: u8 = 0x00;
    pub const ENABLED: u8 = 0x01;
}

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
    pub secure_connections: SecureConnections,
    pub keypress_notifications: bool,
    pub key_size: KeySizes,
    pub fixed_pin: FixedPin,
    pub identity_address: IdentityAddress,
}

impl AuthenticationRequirement {
    /// The parameters to give [`command`] with
    /// [`opcode::ACI_GAP_SET_AUTHENTICATION_REQUIREMENT`].
    #[must_use]
    pub fn params(&self) -> [u8; 12] {
        const USE_FIXED_PIN: u8 = 0x00;
        const ASK_FOR_PIN: u8 = 0x01;
        let (use_fixed, pin) = match self.fixed_pin {
            FixedPin::Pin(pin) => (USE_FIXED_PIN, pin.get()),
            FixedPin::Ask => (ASK_FOR_PIN, 0),
        };
        let [p0, p1, p2, p3] = pin.to_le_bytes();
        let identity = match self.identity_address {
            IdentityAddress::Public => AddrKind::PUBLIC,
            IdentityAddress::StaticRandom => AddrKind::RANDOM,
        };
        [
            u8::from(self.bonding),
            u8::from(self.mitm),
            self.secure_connections as u8,
            u8::from(self.keypress_notifications),
            self.key_size.min,
            self.key_size.max,
            use_fixed,
            p0,
            p1,
            p2,
            p3,
            identity.as_raw(),
        ]
    }
}

/// Whether pairing uses LE Secure Connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecureConnections {
    NotSupported = 0x00,
    Optional = 0x01,
    Only = 0x02,
}

/// The encryption key sizes a pairing accepts, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeySizes {
    min: u8,
    max: u8,
}

impl KeySizes {
    /// `None` unless `min <= max`, both within [`enc_key_size`].
    #[must_use]
    pub const fn new(min: u8, max: u8) -> Option<Self> {
        if enc_key_size::MIN <= min && min <= max && max <= enc_key_size::MAX {
            Some(Self { min, max })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn min(self) -> u8 {
        self.min
    }

    #[must_use]
    pub const fn max(self) -> u8 {
        self.max
    }
}

/// Whether pairing uses a fixed passkey or asks the host for one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixedPin {
    /// The host is asked for a passkey (`GAP_PASS_KEY_REQ`).
    Ask,
    Pin(Passkey),
}

/// A six-digit passkey, 0 to 999999 (Core Vol 3 Part H 2.3.5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Passkey(u32);

impl Passkey {
    /// `None` past 999999.
    #[must_use]
    pub const fn new(key: u32) -> Option<Self> {
        if key <= 999_999 {
            Some(Self(key))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Which of the chip's addresses identifies it to a bonded peer; [`config::STATIC_RANDOM_ADDRESS`]
/// sets the static random one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdentityAddress {
    Public,
    StaticRandom,
}

/// An HCI command packet (`01`, opcode, length, then the parts of `params` in order) at the start
/// of `out`, and its length; `None`, with `out` untouched, when the parameters pass 255 bytes or the
/// packet passes `out`.
#[must_use]
pub fn command(opcode: u16, params: &[&[u8]], out: &mut [u8]) -> Option<usize> {
    let len: usize = params.iter().map(|p| p.len()).sum();
    let len_byte = u8::try_from(len).ok()?;
    let packet = out.get_mut(..COMMAND_HEADER_LEN + len)?;
    let (header, mut rest) = packet.split_at_mut(COMMAND_HEADER_LEN);
    let [lo, hi] = opcode.to_le_bytes();
    header.copy_from_slice(&[packet::COMMAND, lo, hi, len_byte]);
    for part in params {
        let (here, after) = rest.split_at_mut(part.len());
        here.copy_from_slice(part);
        rest = after;
    }
    Some(COMMAND_HEADER_LEN + len)
}

/// The indicator, opcode and length before a command's parameters.
const COMMAND_HEADER_LEN: usize = 4;
/// The longest packet [`command`] writes.
pub const MAX_COMMAND: usize = COMMAND_HEADER_LEN + u8::MAX as usize;

/// The length the packet at the start of `p` gives in its header, which may run past the end of
/// `p`; `None` unless `p` starts with a whole event or ACL header.
#[must_use]
pub fn packet_len(p: &[u8]) -> Option<usize> {
    match p {
        [packet::EVENT, _, len, ..] => Some(3 + usize::from(*len)),
        [packet::ACL, _, _, lo, hi, ..] => Some(5 + usize::from(u16::from_le_bytes([*lo, *hi]))),
        _ => None,
    }
}

/// Whether `p` starts with a whole packet: its header, and as many bytes as the header says.
#[must_use]
pub fn packet_whole(p: &[u8]) -> bool {
    packet_len(p).is_some_and(|len| len <= p.len())
}

/// A device address, public or random.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Address {
    /// [`AddrKind::PUBLIC`] or [`AddrKind::RANDOM`]; a resolved identity's type is given as one of these.
    pub kind: AddrKind,
    /// Little-endian, as the air carries it.
    pub addr: BdAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnTiming {
    pub conn_interval: Duration<1_250>,
    /// Connection events the peripheral may skip.
    pub peripheral_latency: u16,
    pub supervision_timeout: Duration<10_000>,
}

/// The event in one HCI packet, by section of the Core Specification, Vol 4 Part E, whose
/// layouts ST's `bluenrg1_events.h` follows. A connection handle above 0x0EFF makes it `Malformed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// Command Complete (7.7.14): the opcode answered, its status, the return parameters after it;
    /// every opcode but 0x0000, which only grants credits, carries a status, else the event is `Malformed`.
    #[non_exhaustive]
    CommandComplete { cmd_opcode: u16, status: Option<BleStatus>, return_param_bytes: &'a [u8] },
    /// Command Status (7.7.15): a command accepted or refused, results to follow.
    #[non_exhaustive]
    CommandStatus { cmd_opcode: u16, status: BleStatus },
    /// LE (Enhanced) Connection Complete (7.7.65.1, 7.7.65.10) with success: the handle, the
    /// local device's role, the peer, and the timing when the packet carries it.
    #[non_exhaustive]
    LeConnectionComplete {
        handle: ConnHandle,
        role: LeConnRole,
        peer: Address,
        timing: Option<ConnTiming>,
    },
    /// LE (Enhanced) Connection Complete with an error: no connection formed.
    #[non_exhaustive]
    LeConnectionFailed { status: BleStatus },
    /// LE Connection Update Complete (7.7.65.3) with success: the link's new timing.
    #[non_exhaustive]
    LeConnectionUpdateComplete {
        handle: ConnHandle,
        conn_interval: Duration<1_250>,
        peripheral_latency: u16,
        supervision_timeout: Duration<10_000>,
    },
    /// LE Connection Update Complete with an error: the link keeps the parameters it had.
    #[non_exhaustive]
    LeConnectionUpdateFailed { handle: ConnHandle, status: BleStatus },
    /// Disconnection Complete (7.7.5): a status other than success is a disconnection that
    /// failed, the link still up; else the reason.
    #[non_exhaustive]
    DisconnectionComplete { handle: ConnHandle, status: BleStatus, reason: BleStatus },
    /// Encryption Change (7.7.8): a status other than success is a change that failed.
    #[non_exhaustive]
    EncryptionChange { handle: ConnHandle, status: BleStatus, enabled: EncryptionEnabledLevel },
    /// Encryption Key Refresh Complete (7.7.39): an encrypted link encrypted again, a status
    /// other than success a refresh that failed.
    #[non_exhaustive]
    EncryptionKeyRefresh { handle: ConnHandle, status: BleStatus },
    /// LE Data Length Change (7.7.65.7): the largest payload each way, in octets and in microseconds on air.
    #[non_exhaustive]
    LeDataLengthChange {
        handle: ConnHandle,
        max_tx_octets: u16,
        max_tx_time: u16,
        max_rx_octets: u16,
        max_rx_time: u16,
    },
    /// LE Advertising Report (7.7.65.2): one or more reports ([`reports`]).
    #[non_exhaustive]
    LeAdvertisingReport { data: &'a [u8] },
    /// One of ST's events (event code 0xFF), by the code after the event code.
    Vendor(VendorEvent<'a>),
    /// Another LE meta event, by subevent code.
    #[non_exhaustive]
    LeUnknown { subevent_code: u8 },
    /// Another HCI event, by event code.
    #[non_exhaustive]
    Unknown { code: u8 },
    /// ACL data (Core Vol 4 Part E 5.4.2), as the chip sends it in its link-layer-only
    /// configuration (ST's `bluenrg1_hal_aci.h`, `LL_WITHOUT_HOST`).
    #[non_exhaustive]
    Acl {
        handle: ConnHandle,
        boundary: AclPacketBoundary,
        broadcast: AclBroadcastFlag,
        data: &'a [u8],
    },
    /// Not an HCI event, cut short by the end of the read, or not fitting its layout.
    Malformed(&'a [u8]),
}

impl Event<'_> {
    /// For an Encryption Change or Key Refresh Complete, the link's handle and whether it is
    /// encrypted now: only on success, with encryption on.
    #[must_use]
    pub fn encryption(&self) -> Option<(ConnHandle, bool)> {
        match *self {
            Event::EncryptionChange { handle, status, enabled } => {
                Some((handle, status.is_success() && enabled != EncryptionEnabledLevel::Off))
            }
            Event::EncryptionKeyRefresh { handle, status } => Some((handle, status.is_success())),
            _ => None,
        }
    }
}

/// The event in one HCI packet, read no further than the packet's own length.
#[must_use]
pub fn parse(p: &[u8]) -> Event<'_> {
    let Some(p) = packet_len(p).and_then(|len| p.get(..len)) else {
        return Event::Malformed(p);
    };
    let event = match p {
        [packet::EVENT, code, _, params @ ..] => standard(*code, params),
        [packet::ACL, rest @ ..] => acl(rest),
        _ => None,
    };
    event.unwrap_or(Event::Malformed(p))
}

fn acl(p: &[u8]) -> Option<Event<'_>> {
    let acl: AclPacket = first(p)?;
    let handle = link(acl.header().handle & 0x0FFF)?;
    Some(Event::Acl {
        handle,
        boundary: acl.boundary_flag(),
        broadcast: acl.broadcast_flag(),
        data: acl.data(),
    })
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
            let rest = &params[params.len() - e.bytes.len()..];
            let (status, return_param_bytes) = if e.has_status() {
                let (&status, rest) = rest.split_first()?;
                (Some(BleStatus(status)), rest)
            } else {
                (None, rest)
            };
            Event::CommandComplete { cmd_opcode: e.cmd_opcode.to_raw(), status, return_param_bytes }
        }
        ev::COMMAND_STATUS => {
            let e: hci::CommandStatus = first(params)?;
            Event::CommandStatus { cmd_opcode: e.cmd_opcode.to_raw(), status: ble(e.status) }
        }
        ev::DISCONNECTION_COMPLETE => {
            let e: hci::DisconnectionComplete = first(params)?;
            Event::DisconnectionComplete {
                handle: link(e.handle.raw())?,
                status: ble(e.status),
                reason: ble(e.reason),
            }
        }
        ev::ENCRYPTION_CHANGE => {
            let e: hci::EncryptionChangeV1 = first(params)?;
            Event::EncryptionChange {
                handle: link(e.handle.raw())?,
                status: ble(e.status),
                enabled: e.enabled,
            }
        }
        ev::ENCRYPTION_KEY_REFRESH_COMPLETE => {
            let e: hci::EncryptionKeyRefreshComplete = first(params)?;
            Event::EncryptionKeyRefresh { handle: link(e.handle.raw())?, status: ble(e.status) }
        }
        ev::LE_META => le_meta(params)?,
        ev::VENDOR => {
            let (code, params) = u16::from_hci_bytes(params).ok()?;
            Event::Vendor(vendor_event::decode(code, params)?)
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
        sub::CONNECTION_COMPLETE | sub::ENHANCED_CONNECTION_COMPLETE => {
            connection_complete(code, params)?
        }
        sub::CONNECTION_UPDATE_COMPLETE => {
            let (status, rest) = Status::from_hci_bytes(params).ok()?;
            if status == Status::SUCCESS {
                let e: le::LeConnectionUpdateComplete = first(params)?;
                Event::LeConnectionUpdateComplete {
                    handle: link(e.handle.raw())?,
                    conn_interval: e.conn_interval,
                    peripheral_latency: e.peripheral_latency,
                    supervision_timeout: e.supervision_timeout,
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
/// both layouts share (a reserved role is malformed), and the timing when the whole event decodes.
fn connection_complete(code: u8, params: &[u8]) -> Option<Event<'_>> {
    use bt_hci::event::le;
    let (status, rest) = Status::from_hci_bytes(params).ok()?;
    if status != Status::SUCCESS {
        // No connection formed, so the other fields mean nothing (Core Vol 4 Part E §7.7.65.1).
        return Some(Event::LeConnectionFailed { status: ble(status) });
    }
    // A short tail or a reserved clock accuracy still leaves a link on the chip, so it is taken.
    let (handle, rest) = u16::from_hci_bytes(rest).ok()?;
    let (role, rest) = LeConnRole::from_hci_bytes(rest).ok()?;
    let (kind, rest) = AddrKind::from_hci_bytes(rest).ok()?;
    let peer: BdAddr = first(rest)?;
    let timing = if code == le_subevent::CONNECTION_COMPLETE {
        first::<le::LeConnectionComplete>(params)
            .map(|e| (e.conn_interval, e.peripheral_latency, e.supervision_timeout))
    } else {
        first::<le::LeEnhancedConnectionComplete>(params)
            .map(|e| (e.conn_interval, e.peripheral_latency, e.supervision_timeout))
    };
    Some(Event::LeConnectionComplete {
        handle: link(handle)?,
        role,
        // An Enhanced event says 2 or 3 for an identity it resolved: still public or random.
        peer: Address { kind: AddrKind::new(kind.as_raw() & 0x01), addr: peer },
        timing: timing.map(|(conn_interval, peripheral_latency, supervision_timeout)| ConnTiming {
            conn_interval,
            peripheral_latency,
            supervision_timeout,
        }),
    })
}

/// One advertising report: what kind, who, the advertising data and the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Report<'a> {
    pub kind: LeAdvEventKind,
    pub address: Address,
    /// The advertising data ([`ad_structures`]).
    pub data: &'a [u8],
    /// In dBm; 127 when the controller could not measure it.
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
            address: Address {
                kind: AddrKind::new(head[1] & 0x01),
                addr: BdAddr([head[2], head[3], head[4], head[5], head[6], head[7]]),
            },
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
    ad_structures(data)
        .find(|(t, _)| *t == ad::NAME_COMPLETE || *t == ad::NAME_SHORT)
        .map(|(_, v)| v)
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

fn ble(status: Status) -> BleStatus {
    BleStatus(status.into_inner())
}

/// A connection handle, which is at most 0x0EFF (Core Vol 4 Part E 5.4.2).
fn link(handle: u16) -> Option<ConnHandle> {
    (handle <= 0x0EFF).then(|| ConnHandle::new(handle))
}

/// The most a Command Complete carries after its status: an event's 255 parameter bytes less the
/// packet count, the opcode and the status (Core Vol 4 Part E 5.4.4, 7.7.14).
pub const RETURN_PARAMS_MAX: usize = 255 - 4;

/// The identity addresses in an `ACI_GAP_GET_BONDED_DEVICES` reply, never past its end.
pub fn bonded_devices(params: &[u8]) -> impl Iterator<Item = Address> + '_ {
    let count = usize::from(params.first().copied().unwrap_or(0));
    params.get(1..).unwrap_or(&[]).as_chunks::<7>().0.iter().take(count).map(|e| {
        let [kind, a0, a1, a2, a3, a4, a5] = *e;
        Address { kind: AddrKind::new(kind & 0x01), addr: BdAddr([a0, a1, a2, a3, a4, a5]) }
    })
}

/// `ACI_GAP_GET_SECURITY_LEVEL`'s return parameters (ST's `bluenrg1_types.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SecurityLevel {
    /// LE security mode 1 (encryption) or 2 (data signing).
    pub security_mode: u8,
    /// 1 to 4 (Core Vol 3 Part C 10.2).
    pub security_level: u8,
}

impl SecurityLevel {
    /// From a Command Complete's return parameters, the bytes after its status.
    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let [security_mode, security_level, ..] = *p else {
            return None;
        };
        Some(Self { security_mode, security_level })
    }

    /// Whether the link is encrypted: LE security mode 1 at level 2 or above (Core Vol 3 Part C
    /// 10.2.1).
    #[must_use]
    pub const fn encrypted(&self) -> bool {
        self.security_mode == 1 && self.security_level >= 2
    }
}

/// `0A:1B:2C:3D:4E:5F`, the most significant byte first; the kind is not written.
impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [b0, b1, b2, b3, b4, b5] = self.addr.into_inner();
        write!(f, "{b5:02X}:{b4:02X}:{b3:02X}:{b2:02X}:{b1:02X}:{b0:02X}")
    }
}

/// `GAP_PAIRING_COMPLETE`'s status: ST's own codes (`bluenrg1_events.h`), not HCI's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PairingStatus(u8);

impl PairingStatus {
    pub const SUCCESS: Self = Self(0x00);
    pub const TIMEOUT: Self = Self(0x01);
    pub const FAILED: Self = Self(0x02);
    /// Encryption failed: the local device no longer holds the key.
    pub const LTK_MISSING_LOCAL: Self = Self(0x03);
    /// Encryption failed: the peer no longer holds the key.
    pub const LTK_MISSING_PEER: Self = Self(0x04);
    /// The peer does not support encryption.
    pub const ENCRYPTION_NOT_SUPPORTED: Self = Self(0x05);

    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn into_inner(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        self.0 == Self::SUCCESS.0
    }
}

/// Why a pairing failed: `GAP_PAIRING_COMPLETE`'s reason, ST's names (`bluenrg1_events.h`) for
/// SMP's codes (Core Vol 3 Part H 3.5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PairingReason(u8);

impl PairingReason {
    pub const PASSKEY_ENTRY_FAILED: Self = Self(0x01);
    pub const OOB_NOT_AVAILABLE: Self = Self(0x02);
    pub const AUTH_REQ_CANNOT_BE_MET: Self = Self(0x03);
    pub const CONFIRM_VALUE_FAILED: Self = Self(0x04);
    pub const PAIRING_NOT_SUPPORTED: Self = Self(0x05);
    pub const INSUFF_ENCRYPTION_KEY_SIZE: Self = Self(0x06);
    pub const CMD_NOT_SUPPORTED: Self = Self(0x07);
    pub const UNSPECIFIED_REASON: Self = Self(0x08);
    pub const VERY_EARLY_NEXT_ATTEMPT: Self = Self(0x09);
    pub const SM_INVALID_PARAMS: Self = Self(0x0A);
    pub const SMP_SC_DHKEY_CHECK_FAILED: Self = Self(0x0B);
    pub const SMP_SC_NUMCOMPARISON_FAILED: Self = Self(0x0C);

    #[must_use]
    pub const fn new(code: u8) -> Self {
        Self(code)
    }

    #[must_use]
    pub const fn into_inner(self) -> u8 {
        self.0
    }
}

/// The LE `ATT_MTU` before an exchange (Core Vol 3 Part G 5.2.1).
pub const ATT_DEFAULT_MTU: usize = 23;
/// A write's or a notification's opcode and handle, ahead of its value.
pub const ATT_HEADER_LEN: usize = 3;

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

/// A primary service a discovery found: its handle range and UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Service {
    /// The first attribute handle in the service, its declaration.
    pub start: u16,
    /// The last attribute handle in the service.
    pub end: u16,
    pub uuid: Uuid,
}

/// A characteristic declaration a discovery found: its handle, properties ([`props`]), value handle and UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Characteristic {
    /// The declaration's attribute handle.
    pub declaration: u16,
    /// A bitmask of [`props`].
    pub properties: u8,
    /// The value's attribute handle.
    pub value: u16,
    pub uuid: Uuid,
}

/// Characteristic properties (Core Vol 3 Part G 3.3.1.1).
pub mod props {
    pub const BROADCAST: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;
    pub const AUTHENTICATED_SIGNED_WRITES: u8 = 0x40;
    pub const EXTENDED_PROPERTIES: u8 = 0x80;
}
