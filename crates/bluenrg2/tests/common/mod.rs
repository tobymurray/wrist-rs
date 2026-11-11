//! [`Event`] field by field in types a test can build, since the crate's own can't be built outside it.

#![allow(dead_code)]

use bluenrg2::{
    AclBroadcastFlag, AclPacketBoundary, Address, BleStatus, ConnHandle, ConnTiming, Duration, EncryptionEnabledLevel, Event, LeConnRole,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev<'a> {
    CommandComplete { cmd_opcode: u16, status: Option<BleStatus>, return_param_bytes: &'a [u8] },
    CommandStatus { cmd_opcode: u16, status: BleStatus },
    LeConnectionComplete { handle: ConnHandle, role: LeConnRole, peer: Address, timing: Option<Timing> },
    LeConnectionFailed { status: BleStatus },
    LeConnectionUpdateComplete { handle: ConnHandle, conn_interval: Duration<1_250>, peripheral_latency: u16, supervision_timeout: Duration<10_000> },
    LeConnectionUpdateFailed { handle: ConnHandle, status: BleStatus },
    DisconnectionComplete { handle: ConnHandle, status: BleStatus, reason: BleStatus },
    EncryptionChange { handle: ConnHandle, status: BleStatus, enabled: EncryptionEnabledLevel },
    EncryptionKeyRefresh { handle: ConnHandle, status: BleStatus },
    LeDataLengthChange { handle: ConnHandle, max_tx_octets: u16, max_tx_time: u16, max_rx_octets: u16, max_rx_time: u16 },
    LeAdvertisingReport { data: &'a [u8] },
    Vendor { code: u16, params: &'a [u8] },
    LeUnknown { subevent_code: u8 },
    Unknown { code: u8 },
    Acl { handle: ConnHandle, boundary: AclPacketBoundary, broadcast: AclBroadcastFlag, data: &'a [u8] },
    Malformed(&'a [u8]),
}

/// Every field of `e`.
pub fn ev(e: Event<'_>) -> Ev<'_> {
    match e {
        Event::CommandComplete { cmd_opcode, status, return_param_bytes, .. } => Ev::CommandComplete { cmd_opcode, status, return_param_bytes },
        Event::CommandStatus { cmd_opcode, status, .. } => Ev::CommandStatus { cmd_opcode, status },
        Event::LeConnectionComplete { handle, role, peer, timing, .. } => Ev::LeConnectionComplete { handle, role, peer, timing: timing.map(Timing::from) },
        Event::LeConnectionFailed { status, .. } => Ev::LeConnectionFailed { status },
        Event::LeConnectionUpdateComplete { handle, conn_interval, peripheral_latency, supervision_timeout, .. } => {
            Ev::LeConnectionUpdateComplete { handle, conn_interval, peripheral_latency, supervision_timeout }
        }
        Event::LeConnectionUpdateFailed { handle, status, .. } => Ev::LeConnectionUpdateFailed { handle, status },
        Event::DisconnectionComplete { handle, status, reason, .. } => Ev::DisconnectionComplete { handle, status, reason },
        Event::EncryptionChange { handle, status, enabled, .. } => Ev::EncryptionChange { handle, status, enabled },
        Event::EncryptionKeyRefresh { handle, status, .. } => Ev::EncryptionKeyRefresh { handle, status },
        Event::LeDataLengthChange { handle, max_tx_octets, max_tx_time, max_rx_octets, max_rx_time, .. } => {
            Ev::LeDataLengthChange { handle, max_tx_octets, max_tx_time, max_rx_octets, max_rx_time }
        }
        Event::LeAdvertisingReport { data, .. } => Ev::LeAdvertisingReport { data },
        Event::Vendor { code, params, .. } => Ev::Vendor { code, params },
        Event::LeUnknown { subevent_code, .. } => Ev::LeUnknown { subevent_code },
        Event::Unknown { code, .. } => Ev::Unknown { code },
        Event::Acl { handle, boundary, broadcast, data, .. } => Ev::Acl { handle, boundary, broadcast, data },
        Event::Malformed(p) => Ev::Malformed(p),
        e => panic!("an event the tests don't know: {e:?}"),
    }
}

/// The event in `p`, field by field.
pub fn parse(p: &[u8]) -> Ev<'_> {
    ev(bluenrg2::parse(p))
}
