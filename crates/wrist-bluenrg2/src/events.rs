//! What each packet from the BlueNRG-2 tells the core.

use bluenrg2::{boot_reason, vendor, BleStatus, Event};
use wrist_ble::{
    BdAddr, ChipInfo, ConnHandle, Failure, FailureKind, Interval, LeConnRole, LinkParams, PairingEvent, PairingStep, PeerAddr, RadioEvent,
    ResetCause, Status, Timeout,
};

/// The chip's packets as the core's events. ST's bond-lost event names no link, so the
/// translator keeps the link a central holds.
#[derive(Clone, Copy, Debug)]
pub struct Translator {
    pub info: ChipInfo,
    phone: Option<ConnHandle>,
}

impl Translator {
    #[must_use]
    pub const fn new(info: ChipInfo) -> Self {
        Self { info, phone: None }
    }

    /// `None` for a command's reply, which belongs to the operation that sent it, and for the
    /// events the core has no use for.
    pub fn event(&mut self, packet: &[u8]) -> Option<RadioEvent> {
        let e = match bluenrg2::parse(packet) {
            Event::CommandComplete { .. } | Event::CommandStatus { .. } | Event::LeAdvertisingReport { .. } => return None,
            Event::LeConnectionComplete { handle, role, peer, conn_interval: Some((interval, latency)), supervision_timeout: Some(timeout) } => {
                let link = ConnHandle::new(handle);
                if role == LeConnRole::Peripheral {
                    self.phone = Some(link);
                }
                RadioEvent::LinkUp { link, role, peer: PeerAddr { kind: peer.kind, addr: BdAddr::new(peer.bytes) }, params: params(interval, latency, timeout) }
            }
            Event::LeConnectionUpdateComplete { handle, conn_interval, peripheral_latency, supervision_timeout } => {
                RadioEvent::Params { link: ConnHandle::new(handle), result: Ok(params(conn_interval, peripheral_latency, supervision_timeout)) }
            }
            Event::LeConnectionUpdateFailed { handle, status } => {
                RadioEvent::Params { link: ConnHandle::new(handle), result: Err(failure(FailureKind::PeerRejected, status.into_inner())) }
            }
            Event::DisconnectionComplete { handle, status: BleStatus::SUCCESS, reason } => {
                let link = ConnHandle::new(handle);
                if self.phone == Some(link) {
                    self.phone = None;
                }
                RadioEvent::LinkDown { link, reason: Status::new(reason.into_inner()) }
            }
            e @ (Event::EncryptionChange { .. } | Event::EncryptionKeyRefresh { .. }) => {
                let (handle, on) = e.encryption()?;
                RadioEvent::Encryption { link: ConnHandle::new(handle), on }
            }
            Event::LeDataLengthChange { handle, max_tx_octets, max_rx_octets, .. } => {
                RadioEvent::DataLength { link: ConnHandle::new(handle), tx: max_tx_octets, rx: max_rx_octets }
            }
            Event::Vendor { code, params } => self.vendor(code, params)?,
            Event::LeUnknown { subevent_code } => RadioEvent::Unknown { raw_code: u16::from_be_bytes([0x3E, subevent_code]) },
            // Another event, a link without its timing, a connection or disconnection that failed, a malformed packet.
            _ => RadioEvent::Unknown { raw_code: packet.get(1).map_or(0, |&c| u16::from(c)) },
        };
        Some(e)
    }

    fn vendor(&mut self, code: u16, params: &[u8]) -> Option<RadioEvent> {
        let link = bluenrg2::connection_handle(params).map(ConnHandle::new);
        Some(match code {
            // Whatever the reason, the chip's links are gone. A start's own boot never comes
            // here: the driver's `start()` consumes it (`wrist_ble::Radio::start`).
            vendor::BLUE_INITIALIZED => {
                self.phone = None;
                RadioEvent::ChipReset(match bluenrg2::blue_initialized(params) {
                    boot_reason::STARTED => return Some(RadioEvent::Ready(self.info)),
                    boot_reason::WATCHDOG => ResetCause::Watchdog,
                    boot_reason::LOCKUP => ResetCause::Lockup,
                    boot_reason::BROWNOUT => ResetCause::Brownout,
                    boot_reason::CRASH => ResetCause::Crash,
                    r if boot_reason::updater(r) => ResetCause::Updater,
                    _ => ResetCause::Other,
                })
            }
            vendor::HAL_EVENTS_LOST => RadioEvent::EventsLost,
            vendor::GAP_NUMERIC_COMPARISON_VALUE => {
                let (conn, value) = bluenrg2::numeric_comparison(params)?;
                RadioEvent::Pairing(PairingEvent { link: ConnHandle::new(conn), step: PairingStep::Numeric(value) })
            }
            vendor::GAP_PASS_KEY_REQ => RadioEvent::Pairing(PairingEvent { link: link?, step: PairingStep::PasskeyWanted }),
            vendor::GAP_PAIRING_COMPLETE => {
                let (status, _) = bluenrg2::pairing_complete(params)?;
                let step = if status == 0 { PairingStep::Complete } else { PairingStep::Failed };
                RadioEvent::Pairing(PairingEvent { link: link?, step })
            }
            vendor::GAP_BOND_LOST => RadioEvent::BondLost(self.phone?),
            vendor::ATT_EXCHANGE_MTU_RESP => RadioEvent::Mtu(link?, bluenrg2::att_mtu(params)?),
            vendor::L2CAP_CONNECTION_UPDATE_RESP => {
                let r = bluenrg2::L2capUpdateResponse::decode(params)?;
                if r.accepted() {
                    return None;
                }
                RadioEvent::Params { link: link?, result: Err(failure(FailureKind::PeerRejected, 0)) }
            }
            vendor::GAP_SLAVE_SECURITY_INITIATED
            | vendor::GAP_PROC_COMPLETE
            | vendor::GATT_ATTRIBUTE_MODIFIED
            | vendor::GATT_TX_POOL_AVAILABLE
            | vendor::GATT_SERVER_CONFIRMATION => return None,
            _ => RadioEvent::Unknown { raw_code: code },
        })
    }
}

fn params(interval: u16, latency: u16, timeout: u16) -> LinkParams {
    LinkParams { interval: Interval::from_u16(interval), latency, timeout: Timeout::from_u16(timeout) }
}

fn failure(kind: FailureKind, native: u8) -> Failure {
    Failure { kind, native: u16::from(native) }
}
