//! [`Event`] field by field in types a test can build, since the crate's own can't be built outside it.

#![allow(dead_code)]

use bluenrg2::{
    AclBroadcastFlag, AclPacketBoundary, Address, BleStatus, BootReason, ConnHandle, ConnTiming,
    Duration, EncryptionEnabledLevel, Event, LeConnRole, PairingStatus, Uuid, VendorEvent,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub conn_interval: Duration<1_250>,
    pub peripheral_latency: u16,
    pub supervision_timeout: Duration<10_000>,
}

impl From<ConnTiming> for Timing {
    fn from(t: ConnTiming) -> Self {
        let ConnTiming { conn_interval, peripheral_latency, supervision_timeout, .. } = t;
        Self { conn_interval, peripheral_latency, supervision_timeout }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev<'a> {
    CommandComplete {
        cmd_opcode: u16,
        status: Option<BleStatus>,
        return_param_bytes: &'a [u8],
    },
    CommandStatus {
        cmd_opcode: u16,
        status: BleStatus,
    },
    LeConnectionComplete {
        handle: ConnHandle,
        role: LeConnRole,
        peer: Address,
        timing: Option<Timing>,
    },
    LeConnectionFailed {
        status: BleStatus,
    },
    LeConnectionUpdateComplete {
        handle: ConnHandle,
        conn_interval: Duration<1_250>,
        peripheral_latency: u16,
        supervision_timeout: Duration<10_000>,
    },
    LeConnectionUpdateFailed {
        handle: ConnHandle,
        status: BleStatus,
    },
    DisconnectionComplete {
        handle: ConnHandle,
        status: BleStatus,
        reason: BleStatus,
    },
    EncryptionChange {
        handle: ConnHandle,
        status: BleStatus,
        enabled: EncryptionEnabledLevel,
    },
    EncryptionKeyRefresh {
        handle: ConnHandle,
        status: BleStatus,
    },
    LeDataLengthChange {
        handle: ConnHandle,
        max_tx_octets: u16,
        max_tx_time: u16,
        max_rx_octets: u16,
        max_rx_time: u16,
    },
    LeAdvertisingReport {
        data: &'a [u8],
    },
    Vendor(Vn<'a>),
    LeUnknown {
        subevent_code: u8,
    },
    Unknown {
        code: u8,
    },
    Acl {
        handle: ConnHandle,
        boundary: AclPacketBoundary,
        broadcast: AclBroadcastFlag,
        data: &'a [u8],
    },
    Malformed(&'a [u8]),
}

/// Every field of `e`.
pub fn ev(e: Event<'_>) -> Ev<'_> {
    match e {
        Event::CommandComplete { cmd_opcode, status, return_param_bytes, .. } => {
            Ev::CommandComplete { cmd_opcode, status, return_param_bytes }
        }
        Event::CommandStatus { cmd_opcode, status, .. } => Ev::CommandStatus { cmd_opcode, status },
        Event::LeConnectionComplete { handle, role, peer, timing, .. } => {
            Ev::LeConnectionComplete { handle, role, peer, timing: timing.map(Timing::from) }
        }
        Event::LeConnectionFailed { status, .. } => Ev::LeConnectionFailed { status },
        Event::LeConnectionUpdateComplete {
            handle,
            conn_interval,
            peripheral_latency,
            supervision_timeout,
            ..
        } => Ev::LeConnectionUpdateComplete {
            handle,
            conn_interval,
            peripheral_latency,
            supervision_timeout,
        },
        Event::LeConnectionUpdateFailed { handle, status, .. } => {
            Ev::LeConnectionUpdateFailed { handle, status }
        }
        Event::DisconnectionComplete { handle, status, reason, .. } => {
            Ev::DisconnectionComplete { handle, status, reason }
        }
        Event::EncryptionChange { handle, status, enabled, .. } => {
            Ev::EncryptionChange { handle, status, enabled }
        }
        Event::EncryptionKeyRefresh { handle, status, .. } => {
            Ev::EncryptionKeyRefresh { handle, status }
        }
        Event::LeDataLengthChange {
            handle,
            max_tx_octets,
            max_tx_time,
            max_rx_octets,
            max_rx_time,
            ..
        } => Ev::LeDataLengthChange {
            handle,
            max_tx_octets,
            max_tx_time,
            max_rx_octets,
            max_rx_time,
        },
        Event::LeAdvertisingReport { data, .. } => Ev::LeAdvertisingReport { data },
        Event::Vendor(e) => Ev::Vendor(vn(e)),
        Event::LeUnknown { subevent_code, .. } => Ev::LeUnknown { subevent_code },
        Event::Unknown { code, .. } => Ev::Unknown { code },
        Event::Acl { handle, boundary, broadcast, data, .. } => {
            Ev::Acl { handle, boundary, broadcast, data }
        }
        Event::Malformed(p) => Ev::Malformed(p),
        e => panic!("an event the tests don't know: {e:?}"),
    }
}

/// The event in `p`, field by field.
pub fn parse(p: &[u8]) -> Ev<'_> {
    ev(bluenrg2::parse(p))
}

/// [`VendorEvent`] field by field, each list collected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Vn<'a> {
    BlueInitialized {
        reason: BootReason,
    },
    EventsLost {
        lost: u64,
    },
    PairingComplete {
        handle: ConnHandle,
        status: PairingStatus,
        reason: u8,
    },
    PassKeyRequest {
        handle: ConnHandle,
    },
    SlaveSecurityInitiated,
    BondLost,
    GapProcComplete {
        procedure: u8,
        status: BleStatus,
    },
    NumericComparison {
        handle: ConnHandle,
        value: u32,
    },
    L2capUpdateResponse {
        handle: ConnHandle,
        accepted: bool,
    },
    L2capUpdateRequest {
        handle: ConnHandle,
        identifier: u8,
        interval_min: Duration<1_250>,
        interval_max: Duration<1_250>,
        latency: u16,
        timeout: Duration<10_000>,
    },
    AttributeModified {
        handle: ConnHandle,
        attr: u16,
        offset: u16,
        more: bool,
        data: &'a [u8],
    },
    GattProcTimeout {
        handle: ConnHandle,
    },
    ExchangeMtuResponse {
        handle: ConnHandle,
        server_mtu: u16,
    },
    FindInfoResponse {
        handle: ConnHandle,
        descriptors: Vec<(u16, Uuid)>,
    },
    FindByTypeValueResponse {
        handle: ConnHandle,
        ranges: Vec<(u16, u16)>,
    },
    /// (declaration, properties, value, UUID) each.
    ReadByTypeResponse {
        handle: ConnHandle,
        characteristics: Vec<(u16, u8, u16, Uuid)>,
    },
    ReadResponse {
        handle: ConnHandle,
        value: &'a [u8],
    },
    /// (start, end, UUID) each.
    ReadByGroupTypeResponse {
        handle: ConnHandle,
        services: Vec<(u16, u16, Uuid)>,
    },
    Indication {
        handle: ConnHandle,
        attr: u16,
        value: &'a [u8],
    },
    Notification {
        handle: ConnHandle,
        attr: u16,
        value: &'a [u8],
    },
    GattProcComplete {
        handle: ConnHandle,
        status: BleStatus,
    },
    GattErrorResponse {
        handle: ConnHandle,
        request_opcode: u8,
        attr: u16,
        error: u8,
    },
    TxPoolAvailable {
        handle: ConnHandle,
        available: u16,
    },
    ServerConfirmation {
        handle: ConnHandle,
    },
    Unknown {
        code: u16,
        params: &'a [u8],
    },
}

/// Every field of `e`, each list collected.
pub fn vn(e: VendorEvent<'_>) -> Vn<'_> {
    use VendorEvent as V;
    match e {
        V::BlueInitialized { reason, .. } => Vn::BlueInitialized { reason },
        V::EventsLost { lost, .. } => Vn::EventsLost { lost },
        V::PairingComplete { handle, status, reason, .. } => {
            Vn::PairingComplete { handle, status, reason }
        }
        V::PassKeyRequest { handle, .. } => Vn::PassKeyRequest { handle },
        V::SlaveSecurityInitiated => Vn::SlaveSecurityInitiated,
        V::BondLost => Vn::BondLost,
        V::GapProcComplete { procedure, status, .. } => Vn::GapProcComplete { procedure, status },
        V::NumericComparison { handle, value, .. } => Vn::NumericComparison { handle, value },
        V::L2capUpdateResponse { handle, accepted, .. } => {
            Vn::L2capUpdateResponse { handle, accepted }
        }
        V::L2capUpdateRequest {
            handle,
            identifier,
            interval_min,
            interval_max,
            latency,
            timeout,
            ..
        } => Vn::L2capUpdateRequest {
            handle,
            identifier,
            interval_min,
            interval_max,
            latency,
            timeout,
        },
        V::AttributeModified { handle, attr, offset, more, data, .. } => {
            Vn::AttributeModified { handle, attr, offset, more, data }
        }
        V::GattProcTimeout { handle, .. } => Vn::GattProcTimeout { handle },
        V::ExchangeMtuResponse { handle, server_mtu, .. } => {
            Vn::ExchangeMtuResponse { handle, server_mtu }
        }
        V::FindInfoResponse { handle, descriptors, .. } => {
            Vn::FindInfoResponse { handle, descriptors: descriptors.iter().collect() }
        }
        V::FindByTypeValueResponse { handle, ranges, .. } => {
            Vn::FindByTypeValueResponse { handle, ranges: ranges.iter().collect() }
        }
        V::ReadByTypeResponse { handle, characteristics, .. } => Vn::ReadByTypeResponse {
            handle,
            characteristics: characteristics
                .iter()
                .map(|c| (c.declaration, c.properties, c.value, c.uuid))
                .collect(),
        },
        V::ReadResponse { handle, value, .. } => Vn::ReadResponse { handle, value },
        V::ReadByGroupTypeResponse { handle, services, .. } => Vn::ReadByGroupTypeResponse {
            handle,
            services: services.iter().map(|s| (s.start, s.end, s.uuid)).collect(),
        },
        V::Indication { handle, attr, value, .. } => Vn::Indication { handle, attr, value },
        V::Notification { handle, attr, value, .. } => Vn::Notification { handle, attr, value },
        V::GattProcComplete { handle, status, .. } => Vn::GattProcComplete { handle, status },
        V::GattErrorResponse { handle, request_opcode, attr, error, .. } => {
            Vn::GattErrorResponse { handle, request_opcode, attr, error }
        }
        V::TxPoolAvailable { handle, available, .. } => Vn::TxPoolAvailable { handle, available },
        V::ServerConfirmation { handle, .. } => Vn::ServerConfirmation { handle },
        V::Unknown { code, params, .. } => Vn::Unknown { code, params },
        e => panic!("a vendor event the tests don't know: {e:?}"),
    }
}
