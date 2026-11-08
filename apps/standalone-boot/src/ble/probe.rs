//! `ble probe`: the chip's firmware and updater versions, and its anchor period (PM0257 §4)
//! idle and beside advertising, or beside a central's link. `ble restart [updater]`: the chip
//! restarted under the driver, to check that the driver finds it and brings the chip back.

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use bluenrg2::opcode::{ACI_GAP_SET_NON_DISCOVERABLE, ACI_GATT_ADD_CHAR, ACI_GATT_ADD_SERVICE, ACI_HAL_GET_ANCHOR_PERIOD};
use bluenrg2::{evt_mask, permission, props, uuid_type, MIN_ENC_KEY_SIZE, PRIMARY_SERVICE};
use bluenrg2::probe::{Beside, Line, Probe};

use embassy_time::Timer;

use super::{Link, INTERVAL, LATENCY, STATE, STATE_ADVERTISING, STATE_CONNECTED, STATE_IDLE};
use crate::uart::Uart;

/// Starts the chip's updater on firmware 2353 (HARDWARE.md); only this debug command sends it.
const UPDATER_START: u16 = 0xFC20;

struct Text(Uart);

impl Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str(s);
        Ok(())
    }
}

/// What `ble probe gatt` fills until the chip refuses. Each leaves the table full: `ble restart`
/// afterwards puts back the image's own services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gatt {
    /// Services of one record each.
    Services,
    /// The largest record reservation one service is given, then 1-byte characteristics in it.
    Records,
    /// Fixed-length characteristics from 512 bytes down, halving at each refusal.
    Values,
}

/// Test UUIDs, not assigned to any service or characteristic.
const PROBE_UUID: u16 = 0xFF00;
const FIXED_LENGTH: u8 = 0x00;

impl Link<'_> {
    pub(super) async fn probe_gatt(&mut self, what: Gatt) {
        match what {
            Gatt::Services => {
                let mut n = 0u32;
                let status = loop {
                    match self.probe_service(PROBE_UUID + u16::try_from(n).unwrap_or(0), 1).await {
                        Ok(_) if n < 32 => n += 1,
                        Ok(_) => break 0,
                        Err(s) => break s,
                    }
                };
                self.uart.write_str("[ble] probe gatt services: ");
                self.uart.write_u32(n);
                self.uart.write_str(" added beside ours, then status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
            }
            Gatt::Records => {
                let mut records = 255u8;
                let mut first = None;
                let service = loop {
                    match self.probe_service(PROBE_UUID, records).await {
                        Ok(h) => break Some(h),
                        Err(s) => {
                            first.get_or_insert(s);
                            if records == 1 {
                                break None;
                            }
                            records -= 1;
                        }
                    }
                };
                self.uart.write_str("[ble] probe gatt records: one service took ");
                self.uart.write_u32(u32::from(records));
                self.uart.write_str(" records, larger refused with status ");
                self.uart.write_hex8(first.unwrap_or(0));
                self.uart.write_line("");
                let Some(service) = service else {
                    return self.uart.write_line("[ble] probe gatt records: no service at all");
                };
                let mut n = 0u32;
                let status = loop {
                    match self.probe_char(service, PROBE_UUID + 1 + u16::try_from(n).unwrap_or(0), 1).await {
                        Ok(()) if n < 200 => n += 1,
                        Ok(()) => break 0,
                        Err(s) => break s,
                    }
                };
                self.uart.write_str("[ble] probe gatt records: ");
                self.uart.write_u32(n);
                self.uart.write_str(" one-byte characteristics (two records each) in it, then status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
            }
            Gatt::Values => {
                let mut records = 64u8;
                let service = loop {
                    match self.probe_service(PROBE_UUID, records).await {
                        Ok(h) => break Some(h),
                        Err(_) if records > 3 => records -= 1,
                        Err(_) => break None,
                    }
                };
                let Some(service) = service else {
                    return self.uart.write_line("[ble] probe gatt values: no service at all");
                };
                self.uart.write_str("[ble] probe gatt values: a service of ");
                self.uart.write_u32(u32::from(records));
                self.uart.write_line(" records");
                let (mut len, mut total, mut n, mut uuid) = (512u16, 0u32, 0u32, PROBE_UUID + 1);
                while len > 0 && n < 64 {
                    match self.probe_char(service, uuid, len).await {
                        Ok(()) => {
                            total += u32::from(len);
                            n += 1;
                            uuid += 1;
                        }
                        Err(s) => {
                            self.uart.write_str("[ble] probe gatt values: ");
                            self.uart.write_u32(u32::from(len));
                            self.uart.write_str(" bytes refused, status ");
                            self.uart.write_hex8(s);
                            self.uart.write_str(", after ");
                            self.uart.write_u32(total);
                            self.uart.write_line(" bytes");
                            len /= 2;
                        }
                    }
                }
                self.uart.write_str("[ble] probe gatt values: ");
                self.uart.write_u32(total);
                self.uart.write_str(" bytes in ");
                self.uart.write_u32(n);
                self.uart.write_line(" fixed-length characteristics beside ours");
            }
        }
        self.uart.write_line("[ble] probe gatt: the table is full; `ble restart` puts ours back");
    }

    /// A primary service with a 16-bit UUID: its handle, or the chip's refusal (0xFF unanswered).
    async fn probe_service(&mut self, uuid: u16, records: u8) -> Result<u16, u8> {
        let u = uuid.to_le_bytes();
        match self.command(ACI_GATT_ADD_SERVICE, &[uuid_type::U16, u[0], u[1], PRIMARY_SERVICE, records]).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Ok(u16::from_le_bytes([r.params[0], r.params[1]])),
            Ok(r) => Err(if r.status == 0 { 0xFE } else { r.status }),
            Err(_) => Err(0xFF),
        }
    }

    /// A readable fixed-length characteristic of `len` bytes.
    async fn probe_char(&mut self, service: u16, uuid: u16, len: u16) -> Result<(), u8> {
        let sv = service.to_le_bytes();
        let u = uuid.to_le_bytes();
        let l = len.to_le_bytes();
        let p = [
            sv[0], sv[1], uuid_type::U16, u[0], u[1], l[0], l[1], props::READ, permission::NONE, evt_mask::NONE, MIN_ENC_KEY_SIZE,
            FIXED_LENGTH,
        ];
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 => Ok(()),
            Ok(r) => Err(r.status),
            Err(_) => Err(0xFF),
        }
    }

    pub(super) async fn probe(&mut self) {
        if self.scanner.until().is_some() {
            return self.uart.write_line("[ble] probe: a scan is running; try after it");
        }
        self.read(Probe::Firmware).await;
        self.read(Probe::Updater).await;
        match STATE.load(Ordering::Relaxed) {
            STATE_CONNECTED => self.read_anchor().await,
            STATE_ADVERTISING => {
                if self.stop_advertising().await {
                    self.read_anchor().await;
                    self.advertise().await;
                    self.read_anchor().await;
                }
            }
            _ => {
                self.read_anchor().await;
                self.advertise().await;
                self.read_anchor().await;
                if STATE.load(Ordering::Relaxed) == STATE_ADVERTISING {
                    self.stop_advertising().await;
                }
            }
        }
    }

    /// The chip reset through its pin with no start waiting for it, as a brownout would leave it;
    /// with `updater`, sent its updater's start, as Linux check 8 did by mistake. Either way the
    /// next `ACI_BLUE_INITIALIZED` is one the driver did not ask for.
    pub(super) async fn restart_chip(&mut self, updater: bool) {
        if updater {
            let status = self.command(UPDATER_START, &[]).await.map(|r| r.status);
            self.uart.write_str("[ble] probe: updater start sent, status ");
            match status {
                Ok(s) => self.uart.write_hex8(s),
                Err(why) => self.uart.write_str(why),
            }
            return self.uart.write_line("");
        }
        #[cfg(feature = "ble-trace")]
        super::trace::packet(super::trace::Dir::Reset, &[]);
        self.rst.set_low();
        Timer::after_millis(5).await;
        self.rst.set_high();
        self.uart.write_line("[ble] probe: chip reset through its pin, unannounced");
    }

    async fn stop_advertising(&mut self) -> bool {
        let stopped = matches!(self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await, Ok(r) if r.status == 0);
        if stopped {
            STATE.store(STATE_IDLE, Ordering::Relaxed);
            self.advertised = None;
        } else {
            self.uart.write_line("[ble] probe: advertising would not stop");
        }
        stopped
    }

    /// The anchor period, labelled with what the radio was doing once the reply came.
    async fn read_anchor(&mut self) {
        let reply = self.command(ACI_HAL_GET_ANCHOR_PERIOD, &[]).await.ok();
        let beside = match STATE.load(Ordering::Relaxed) {
            STATE_CONNECTED => {
                Beside::Link { interval: INTERVAL.load(Ordering::Relaxed), latency: LATENCY.load(Ordering::Relaxed) }
            }
            STATE_ADVERTISING => Beside::Advertising { interval: self.advertised.map_or(0, |t| t.advertise) },
            _ => Beside::Idle,
        };
        write(self.uart, Line { probe: Probe::Anchor(beside), reply: reply.as_ref().map(|r| (r.status, &r.params[..r.len])) });
    }

    async fn read(&mut self, probe: Probe) {
        let reply = self.command(probe.opcode(), &[]).await.ok();
        write(self.uart, Line { probe, reply: reply.as_ref().map(|r| (r.status, &r.params[..r.len])) });
    }
}

fn write(uart: Uart, line: Line<'_>) {
    let _ = write!(Text(uart), "[ble] {line}");
    uart.write_line("");
}
