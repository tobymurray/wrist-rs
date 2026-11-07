//! `ble probe`: the chip's firmware and updater versions, and its anchor period (PM0257 §4)
//! idle and beside advertising, or beside a central's link. `ble restart [updater]`: the chip
//! restarted under the driver, to check that the driver finds it and brings the chip back.

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use bluenrg2::opcode::{ACI_GAP_SET_NON_DISCOVERABLE, ACI_HAL_GET_ANCHOR_PERIOD};
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

impl Link<'_> {
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
