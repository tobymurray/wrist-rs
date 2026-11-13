//! What each packet from the BlueNRG-2 tells the core.

use bluenrg2::{BleStatus, BootReason, Event, VendorEvent};
use wrist_ble::{
    ChipInfo, ConnHandle, Failure, FailureKind, LeConnRole, LinkParams, Micros, PairingEvent, PairingStep, PeerAddr, RadioEvent,
    ResetCause, Status,
};

use crate::gatt::Handles;

/// The chip's packets as the core's events, keeping what ST's events leave out: the link a
/// central holds, for the bond-lost event, and the local server's handles, for a write.
#[derive(Clone, Copy, Debug, Default)]
pub struct Translator {
    /// The chip's identity once a start has read it; until then a boot is a reset from power-on.
    pub info: Option<ChipInfo>,
    phone: Option<ConnHandle>,
    server: Option<Handles>,
}

impl Translator {
    #[must_use]
    pub const fn new(info: ChipInfo) -> Self {
        Self { info: Some(info), phone: None, server: None }
    }

    /// The local server as the chip built it, until the chip next boots.
    pub fn serve(&mut self, server: Handles) {
        self.server = Some(server);
    }

    #[must_use]
    pub fn server(&self) -> Option<&Handles> {
        self.server.as_ref()
    }

    /// `None` for a command's reply, which belongs to the operation that sent it, for a write
    /// outside the local server, and for the events the core has no use for.
    pub fn event(&mut self, packet: &[u8]) -> Option<RadioEvent> {
        let e = match bluenrg2::parse(packet) {
            Event::CommandComplete { .. } | Event::CommandStatus { .. } | Event::LeAdvertisingReport { .. } => return None,
            Event::LeConnectionComplete { handle: link, role, peer, timing, .. } => {
                if role == LeConnRole::Peripheral {
                    self.phone = Some(link);
                }
                let params = timing.map(|t| LinkParams { interval: t.conn_interval, latency: t.peripheral_latency, timeout: t.supervision_timeout });
                RadioEvent::LinkUp { link, role, peer: PeerAddr { kind: peer.kind, addr: peer.addr }, params }
            }
            Event::LeConnectionUpdateComplete { handle, conn_interval, peripheral_latency, supervision_timeout, .. } => {
                RadioEvent::Params { link: handle, result: Ok(LinkParams { interval: conn_interval, latency: peripheral_latency, timeout: supervision_timeout }) }
            }
            Event::LeConnectionUpdateFailed { handle, status, .. } => {
                RadioEvent::Params { link: handle, result: Err(failure(FailureKind::PeerRejected, status.into_inner())) }
            }
            Event::DisconnectionComplete { handle: link, status: BleStatus::SUCCESS, reason, .. } => {
                if self.phone == Some(link) {
                    self.phone = None;
                }
                RadioEvent::LinkDown { link, reason: Status::new(reason.into_inner()) }
            }
            e @ (Event::EncryptionChange { .. } | Event::EncryptionKeyRefresh { .. }) => {
                let (link, on) = e.encryption()?;
                RadioEvent::Encryption { link, on }
            }
            Event::LeDataLengthChange { handle, max_tx_octets, max_tx_time, max_rx_octets, max_rx_time, .. } => RadioEvent::DataLength {
                link: handle,
                tx: max_tx_octets,
                rx: max_rx_octets,
                tx_time: Micros(u32::from(max_tx_time)),
                rx_time: Micros(u32::from(max_rx_time)),
            },
            Event::Vendor(e) => self.vendor(e)?,
            Event::LeUnknown { subevent_code, .. } => RadioEvent::Unknown { raw_code: u16::from_be_bytes([0x3E, subevent_code]) },
            // A packet no decoder can read is one the driver drops.
            Event::Malformed(_) => RadioEvent::EventsLost,
            // Another event, or a connection or disconnection that failed.
            _ => RadioEvent::Unknown { raw_code: packet.get(1).map_or(0, |&c| u16::from(c)) },
        };
        Some(e)
    }

    fn vendor(&mut self, e: VendorEvent) -> Option<RadioEvent> {
        Some(match e {
            // Whatever the reason, the chip's links are gone; `wrist_ble::Radio::start` consumes a start's own boot.
            VendorEvent::BlueInitialized { reason, .. } => {
                self.phone = None;
                self.server = None;
                RadioEvent::ChipReset(match reason {
                    Some(BootReason::STARTED) => return Some(self.info.map_or(RadioEvent::ChipReset(ResetCause::PowerOn), RadioEvent::Ready)),
                    Some(BootReason::WATCHDOG) => ResetCause::Watchdog,
                    Some(BootReason::LOCKUP) => ResetCause::Lockup,
                    Some(BootReason::BROWNOUT) => ResetCause::Brownout,
                    Some(BootReason::CRASH) => ResetCause::Crash,
                    Some(r) if r.is_updater() => ResetCause::Updater,
                    _ => ResetCause::Other,
                })
            }
            VendorEvent::EventsLost { .. } => RadioEvent::EventsLost,
            VendorEvent::NumericComparison { handle, value, .. } => RadioEvent::Pairing(PairingEvent { link: handle, step: PairingStep::Numeric(value) }),
            VendorEvent::PassKeyRequest { handle, .. } => RadioEvent::Pairing(PairingEvent { link: handle, step: PairingStep::PasskeyWanted }),
            VendorEvent::PairingComplete { handle, status, .. } => {
                let step = if status.is_success() { PairingStep::Complete } else { PairingStep::Failed };
                RadioEvent::Pairing(PairingEvent { link: handle, step })
            }
            VendorEvent::BondLost => RadioEvent::BondLost(self.phone?),
            VendorEvent::ExchangeMtuResponse { handle, server_mtu, .. } => RadioEvent::Mtu(handle, server_mtu),
            VendorEvent::L2capUpdateResponse { handle, accepted: false, .. } => {
                RadioEvent::Params { link: handle, result: Err(failure(FailureKind::PeerRejected, 0)) }
            }
            VendorEvent::AttributeModified { handle, attr, offset, more, data, .. } => self.server.as_ref()?.written(handle, attr, offset, more, data)?,
            VendorEvent::L2capUpdateResponse { accepted: true, .. }
            | VendorEvent::SlaveSecurityInitiated
            | VendorEvent::GapProcComplete { .. }
            | VendorEvent::TxPoolAvailable { .. }
            | VendorEvent::ServerConfirmation { .. } => return None,
            e => RadioEvent::Unknown { raw_code: e.code() },
        })
    }
}

fn failure(kind: FailureKind, native: u8) -> Failure {
    Failure { kind, native: u16::from(native) }
}
