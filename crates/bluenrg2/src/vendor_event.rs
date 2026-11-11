//! ST's vendor events (event code 0xFF), each decoded by the code it carries. Layouts from ST's
//! `bluenrg1_events.h`.

use crate::{
    link, vendor, BleStatus, BootReason, Characteristic, ConnHandle, Duration, PairingReason,
    PairingStatus, Service, Uuid,
};

/// One of ST's vendor events, by its code. Each starts with its connection handle where it has
/// one; a value longer than its event is cut to what the event holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VendorEvent<'a> {
    /// `ACI_BLUE_INITIALIZED`: the chip started, after a reset asked for or not.
    #[non_exhaustive]
    BlueInitialized { reason: Option<BootReason> },
    /// `ACI_BLUE_EVENTS_LOST`: the chip dropped events because the host read too slowly; a bit for
    /// each kind it dropped, in ST's order.
    #[non_exhaustive]
    EventsLost { lost: u64 },
    /// `ACI_GAP_PAIRING_COMPLETE`; the reason only when the status is [`PairingStatus::FAILED`].
    #[non_exhaustive]
    PairingComplete { handle: ConnHandle, status: PairingStatus, reason: Option<PairingReason> },
    /// `ACI_GAP_PASS_KEY_REQ`: the stack wants a passkey (`ACI_GAP_PASS_KEY_RESP`).
    #[non_exhaustive]
    PassKeyRequest { handle: ConnHandle },
    /// `ACI_GAP_SLAVE_SECURITY_INITIATED`: the security request reached the central.
    SlaveSecurityInitiated,
    /// `ACI_GAP_BOND_LOST`: a bonded central no longer has its keys (`ACI_GAP_ALLOW_REBOND`).
    BondLost,
    /// `ACI_GAP_PROC_COMPLETE`: a GAP procedure ([`crate::gap_proc`]) ended.
    #[non_exhaustive]
    GapProcComplete { procedure: u8, status: BleStatus },
    /// `ACI_GAP_NUMERIC_COMPARISON_VALUE`: the six-digit number to compare.
    #[non_exhaustive]
    NumericComparison { handle: ConnHandle, value: u32 },
    /// `ACI_L2CAP_CONNECTION_UPDATE_RESP`: the central's answer to the local device's parameter
    /// request (Core Vol 3 Part A 4.21); a result other than 0 is a refusal.
    #[non_exhaustive]
    L2capUpdateResponse { handle: ConnHandle, accepted: bool },
    /// `ACI_L2CAP_CONNECTION_UPDATE_REQ`: a peripheral asks the central for new parameters; the
    /// answer echoes the identifier.
    #[non_exhaustive]
    L2capUpdateRequest {
        handle: ConnHandle,
        identifier: u8,
        interval_min: Duration<1_250>,
        interval_max: Duration<1_250>,
        /// Connection events the peripheral may skip.
        latency: u16,
        timeout: Duration<10_000>,
    },
    /// `ACI_GATT_ATTRIBUTE_MODIFIED`: a peer wrote an attribute of the local GATT server; `more`
    /// when a long write has pieces still to come.
    #[non_exhaustive]
    AttributeModified { handle: ConnHandle, attr: u16, offset: u16, more: bool, data: &'a [u8] },
    /// `ACI_GATT_PROC_TIMEOUT`: a GATT procedure got no answer within ATT's timeout (Core Vol 3
    /// Part F 3.3.3).
    #[non_exhaustive]
    GattProcTimeout { handle: ConnHandle },
    /// `ACI_ATT_EXCHANGE_MTU_RESP`: the server's receive MTU, in bytes.
    #[non_exhaustive]
    ExchangeMtuResponse { handle: ConnHandle, server_mtu: u16 },
    /// `ACI_ATT_FIND_INFO_RESP`: descriptors found.
    #[non_exhaustive]
    FindInfoResponse { handle: ConnHandle, descriptors: Descriptors<'a> },
    /// `ACI_ATT_FIND_BY_TYPE_VALUE_RESP`: the handle ranges of what matched.
    #[non_exhaustive]
    FindByTypeValueResponse { handle: ConnHandle, ranges: HandleRanges<'a> },
    /// `ACI_ATT_READ_BY_TYPE_RESP`, as a characteristic discovery reads it.
    #[non_exhaustive]
    ReadByTypeResponse { handle: ConnHandle, characteristics: Characteristics<'a> },
    /// `ACI_ATT_READ_RESP`: the value read.
    #[non_exhaustive]
    ReadResponse { handle: ConnHandle, value: &'a [u8] },
    /// `ACI_ATT_READ_BY_GROUP_TYPE_RESP`, as a primary service discovery reads it.
    #[non_exhaustive]
    ReadByGroupTypeResponse { handle: ConnHandle, services: Services<'a> },
    /// `ACI_GATT_INDICATION`; the stack waits for `ACI_GATT_CONFIRM_INDICATION`.
    #[non_exhaustive]
    Indication { handle: ConnHandle, attr: u16, value: &'a [u8] },
    /// `ACI_GATT_NOTIFICATION`.
    #[non_exhaustive]
    Notification { handle: ConnHandle, attr: u16, value: &'a [u8] },
    /// `ACI_GATT_PROC_COMPLETE`: a GATT procedure ended, in success or with its error.
    #[non_exhaustive]
    GattProcComplete { handle: ConnHandle, status: BleStatus },
    /// `ACI_GATT_ERROR_RESP`: the server refused a request, with its ATT error code (Core Vol 3
    /// Part F 3.4.1.1).
    #[non_exhaustive]
    GattErrorResponse { handle: ConnHandle, request_opcode: u8, attr: u16, error: u8 },
    /// `ACI_GATT_TX_POOL_AVAILABLE`: TX buffers free again after
    /// [`BleStatus::INSUFFICIENT_RESOURCES`].
    #[non_exhaustive]
    TxPoolAvailable { handle: ConnHandle, available: u16 },
    /// `ACI_GATT_SERVER_CONFIRMATION`: the client confirmed an indication.
    #[non_exhaustive]
    ServerConfirmation { handle: ConnHandle },
    /// A vendor event this crate does not decode, by code ([`vendor`]).
    #[non_exhaustive]
    Unknown { code: u16, params: &'a [u8] },
}

impl VendorEvent<'_> {
    /// The vendor event code ([`vendor`]) the event came with.
    #[must_use]
    pub fn code(&self) -> u16 {
        use vendor as v;
        match self {
            Self::BlueInitialized { .. } => v::BLUE_INITIALIZED,
            Self::EventsLost { .. } => v::BLUE_EVENTS_LOST,
            Self::PairingComplete { .. } => v::GAP_PAIRING_COMPLETE,
            Self::PassKeyRequest { .. } => v::GAP_PASS_KEY_REQ,
            Self::SlaveSecurityInitiated => v::GAP_SLAVE_SECURITY_INITIATED,
            Self::BondLost => v::GAP_BOND_LOST,
            Self::GapProcComplete { .. } => v::GAP_PROC_COMPLETE,
            Self::NumericComparison { .. } => v::GAP_NUMERIC_COMPARISON_VALUE,
            Self::L2capUpdateResponse { .. } => v::L2CAP_CONNECTION_UPDATE_RESP,
            Self::L2capUpdateRequest { .. } => v::L2CAP_CONNECTION_UPDATE_REQ,
            Self::AttributeModified { .. } => v::GATT_ATTRIBUTE_MODIFIED,
            Self::GattProcTimeout { .. } => v::GATT_PROC_TIMEOUT,
            Self::ExchangeMtuResponse { .. } => v::ATT_EXCHANGE_MTU_RESP,
            Self::FindInfoResponse { .. } => v::ATT_FIND_INFO_RESP,
            Self::FindByTypeValueResponse { .. } => v::ATT_FIND_BY_TYPE_VALUE_RESP,
            Self::ReadByTypeResponse { .. } => v::ATT_READ_BY_TYPE_RESP,
            Self::ReadResponse { .. } => v::ATT_READ_RESP,
            Self::ReadByGroupTypeResponse { .. } => v::ATT_READ_BY_GROUP_TYPE_RESP,
            Self::Indication { .. } => v::GATT_INDICATION,
            Self::Notification { .. } => v::GATT_NOTIFICATION,
            Self::GattProcComplete { .. } => v::GATT_PROC_COMPLETE,
            Self::GattErrorResponse { .. } => v::GATT_ERROR_RESP,
            Self::TxPoolAvailable { .. } => v::GATT_TX_POOL_AVAILABLE,
            Self::ServerConfirmation { .. } => v::GATT_SERVER_CONFIRMATION,
            Self::Unknown { code, .. } => *code,
        }
    }
}

/// The vendor event `code` names; `None` when the parameters do not fit its layout.
pub(crate) fn decode(code: u16, p: &[u8]) -> Option<VendorEvent<'_>> {
    use vendor as v;
    use VendorEvent as E;
    let byte = |i: usize| p.get(i).copied();
    let u16_at = |i: usize| Some(u16::from_le_bytes([byte(i)?, byte(i + 1)?]));
    let handle = || link(u16_at(0)?);
    Some(match code {
        v::BLUE_INITIALIZED => E::BlueInitialized { reason: byte(0).map(BootReason::new) },
        v::BLUE_EVENTS_LOST => E::EventsLost { lost: u64::from_le_bytes(*p.first_chunk()?) },
        v::GAP_PAIRING_COMPLETE => {
            let status = PairingStatus::new(byte(2)?);
            let reason = PairingReason::new(byte(3)?);
            E::PairingComplete {
                handle: handle()?,
                status,
                reason: (status == PairingStatus::FAILED).then_some(reason),
            }
        }
        v::GAP_PASS_KEY_REQ => E::PassKeyRequest { handle: handle()? },
        v::GAP_SLAVE_SECURITY_INITIATED => E::SlaveSecurityInitiated,
        v::GAP_BOND_LOST => E::BondLost,
        v::GAP_PROC_COMPLETE => {
            E::GapProcComplete { procedure: byte(0)?, status: BleStatus::new(byte(1)?) }
        }
        v::GAP_NUMERIC_COMPARISON_VALUE => E::NumericComparison {
            handle: handle()?,
            value: u32::from_le_bytes(*p.get(2..)?.first_chunk()?),
        },
        v::L2CAP_CONNECTION_UPDATE_RESP => {
            E::L2capUpdateResponse { handle: handle()?, accepted: u16_at(2)? == 0 }
        }
        // The L2CAP length at 3 says nothing the event's own length doesn't.
        v::L2CAP_CONNECTION_UPDATE_REQ => E::L2capUpdateRequest {
            handle: handle()?,
            identifier: byte(2)?,
            interval_min: Duration::from_u16(u16_at(5)?),
            interval_max: Duration::from_u16(u16_at(7)?),
            latency: u16_at(9)?,
            timeout: Duration::from_u16(u16_at(11)?),
        },
        v::GATT_ATTRIBUTE_MODIFIED => {
            let offset = u16_at(4)?;
            let data = p.get(8..)?;
            let len = usize::from(u16_at(6)?).min(data.len());
            E::AttributeModified {
                handle: handle()?,
                attr: u16_at(2)?,
                offset: offset & 0x7FFF,
                more: offset & 0x8000 != 0,
                data: &data[..len],
            }
        }
        v::GATT_PROC_TIMEOUT => E::GattProcTimeout { handle: handle()? },
        v::ATT_EXCHANGE_MTU_RESP => {
            E::ExchangeMtuResponse { handle: handle()?, server_mtu: u16_at(2)? }
        }
        v::ATT_FIND_INFO_RESP => E::FindInfoResponse {
            handle: handle()?,
            descriptors: Descriptors { format: byte(2)?, data: value(p, 3)? },
        },
        v::ATT_FIND_BY_TYPE_VALUE_RESP => E::FindByTypeValueResponse {
            handle: handle()?,
            ranges: HandleRanges { count: byte(2)?, data: p.get(3..)? },
        },
        v::ATT_READ_BY_TYPE_RESP => E::ReadByTypeResponse {
            handle: handle()?,
            characteristics: Characteristics { each: byte(2)?, data: value(p, 3)? },
        },
        v::ATT_READ_RESP => E::ReadResponse { handle: handle()?, value: value(p, 2)? },
        v::ATT_READ_BY_GROUP_TYPE_RESP => E::ReadByGroupTypeResponse {
            handle: handle()?,
            services: Services { each: byte(2)?, data: value(p, 3)? },
        },
        v::GATT_INDICATION => {
            E::Indication { handle: handle()?, attr: u16_at(2)?, value: value(p, 4)? }
        }
        v::GATT_NOTIFICATION => {
            E::Notification { handle: handle()?, attr: u16_at(2)?, value: value(p, 4)? }
        }
        v::GATT_PROC_COMPLETE => {
            E::GattProcComplete { handle: handle()?, status: BleStatus::new(byte(2)?) }
        }
        v::GATT_ERROR_RESP => E::GattErrorResponse {
            handle: handle()?,
            request_opcode: byte(2)?,
            attr: u16_at(3)?,
            error: byte(5)?,
        },
        v::GATT_TX_POOL_AVAILABLE => {
            E::TxPoolAvailable { handle: handle()?, available: u16_at(2)? }
        }
        v::GATT_SERVER_CONFIRMATION => E::ServerConfirmation { handle: handle()? },
        code => E::Unknown { code, params: p },
    })
}

/// The bytes after a one-byte length at `at`, as many as it says, cut to what `p` holds.
fn value(p: &[u8], at: usize) -> Option<&[u8]> {
    let n = usize::from(*p.get(at)?);
    let rest = p.get(at + 1..)?;
    Some(&rest[..n.min(rest.len())])
}

/// The primary services in an `ACI_ATT_READ_BY_GROUP_TYPE_RESP`: elements of 6 or 20 bytes;
/// another length yields none, an element cut short is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Services<'a> {
    each: u8,
    data: &'a [u8],
}

impl<'a> Services<'a> {
    pub fn iter(&self) -> impl Iterator<Item = Service> + 'a {
        let each = usize::from(self.each);
        self.data.chunks_exact(each.max(1)).filter(move |_| each == 6 || each == 20).filter_map(
            |e| {
                Some(Service {
                    start: u16::from_le_bytes([e[0], e[1]]),
                    end: u16::from_le_bytes([e[2], e[3]]),
                    uuid: Uuid::from_le(&e[4..])?,
                })
            },
        )
    }
}

/// The characteristic declarations in an `ACI_ATT_READ_BY_TYPE_RESP`: elements of 7 or 21
/// bytes; another length yields none, an element cut short is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Characteristics<'a> {
    each: u8,
    data: &'a [u8],
}

impl<'a> Characteristics<'a> {
    pub fn iter(&self) -> impl Iterator<Item = Characteristic> + 'a {
        let each = usize::from(self.each);
        self.data.chunks_exact(each.max(1)).filter(move |_| each == 7 || each == 21).filter_map(
            |e| {
                Some(Characteristic {
                    declaration: u16::from_le_bytes([e[0], e[1]]),
                    properties: e[2],
                    value: u16::from_le_bytes([e[3], e[4]]),
                    uuid: Uuid::from_le(&e[5..])?,
                })
            },
        )
    }
}

/// The (handle, UUID) pairs in an `ACI_ATT_FIND_INFO_RESP`: format 1 for 16-bit UUIDs, 2 for
/// 128-bit; another format yields none, a pair cut short is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptors<'a> {
    format: u8,
    data: &'a [u8],
}

impl<'a> Descriptors<'a> {
    pub fn iter(&self) -> impl Iterator<Item = (u16, Uuid)> + 'a {
        let each = match self.format {
            1 => 4,
            2 => 18,
            _ => 0,
        };
        let data = if each == 0 { &[][..] } else { self.data };
        data.chunks_exact(each.max(1))
            .filter_map(|e| Some((u16::from_le_bytes([e[0], e[1]]), Uuid::from_le(&e[2..])?)))
    }
}

/// The (found, group end) handle pairs in an `ACI_ATT_FIND_BY_TYPE_VALUE_RESP`, no more than
/// its count says or it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleRanges<'a> {
    count: u8,
    data: &'a [u8],
}

impl<'a> HandleRanges<'a> {
    pub fn iter(&self) -> impl Iterator<Item = (u16, u16)> + 'a {
        self.data
            .as_chunks::<4>()
            .0
            .iter()
            .take(usize::from(self.count))
            .map(|e| (u16::from_le_bytes([e[0], e[1]]), u16::from_le_bytes([e[2], e[3]])))
    }
}
