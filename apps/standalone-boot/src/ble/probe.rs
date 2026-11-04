//! `ble probe`: the chip's firmware and updater versions, and its anchor period (PM0257 §4)
//! idle and beside advertising, or beside a central's link.

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use wrist_aci::opcode::{ACI_GAP_SET_NON_DISCOVERABLE, ACI_HAL_GET_ANCHOR_PERIOD};
use wrist_aci::probe::{Beside, Line, Probe};

use super::{Link, INTERVAL, LATENCY, STATE, STATE_ADVERTISING, STATE_CONNECTED, STATE_IDLE};
use crate::uart::Uart;

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
