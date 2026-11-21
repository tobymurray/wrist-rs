//! `ble probe`: the chip's firmware and updater versions, and its anchor period (PM0257 §4)
//! idle and beside advertising, or beside a central's link. `ble restart [updater]`: the chip
//! restarted under the driver, to check that the driver finds it and brings the chip back.
//! `ble probe snapshot`: the driver's snapshot, to check it against what the watch is doing.

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use bluenrg2::opcode::{
    ACI_GAP_SET_NON_DISCOVERABLE, ACI_GAP_TERMINATE, ACI_GATT_ADD_CHAR, ACI_GATT_ADD_SERVICE, ACI_HAL_GET_ANCHOR_PERIOD,
    ACI_UPDATER_START,
};
use bluenrg2::{enc_key_size, evt_mask, permission, props, service_type, uuid_type, value_length};
use wrist_bluenrg2::driver::RESET_MS;
use wrist_bluenrg2::probe::{Beside, Line, Probe, Taken};
use wrist_bluenrg2::snapshot::{Snap, Snapping};
use wrist_bluenrg2::transport;

use embassy_time::Timer;

use wrist_bluenrg2::wake::ble::timing;

use super::{power, trace_command, Link, CONN_HANDLE, DEVICE_NAME, INTERVAL, LATENCY, STATE, STATE_ADVERTISING, STATE_CONNECTED};
use crate::uart::Uart;

/// `ble probe adv`'s steps: the interval (0.625 ms units) each `ACI_GAP_SET_DISCOVERABLE` asks
/// for, and whether `ACI_GAP_SET_NON_DISCOVERABLE` goes first.
const ADVERTISING_STEPS: [(u16, bool); 4] = [(160, true), (1636, false), (160, false), (160, false)];

/// How long each step of `ble probe adv` advertises, for a host to time the advertisements.
const STEP_MS: u64 = 6_000;

struct Text(Uart);

impl Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str(s);
        Ok(())
    }
}

/// What `ble probe gatt` fills until the chip refuses. Each leaves the table full: `ble off` then
/// `ble on` afterwards puts back the image's own services.
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
                let Some(service) = service else {
                    self.uart.write_str("[ble] probe gatt records: no service at all, status ");
                    self.uart.write_hex8(first.unwrap_or(0));
                    return self.uart.write_line("");
                };
                self.uart.write_str("[ble] probe gatt records: one service took ");
                self.uart.write_u32(u32::from(records));
                self.uart.write_str(" records, larger refused with status ");
                self.uart.write_hex8(first.unwrap_or(0));
                self.uart.write_line("");
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
        self.uart.write_line("[ble] probe gatt: the table is full; `ble off` then `ble on` puts ours back");
    }

    /// A primary service with a 16-bit UUID: its handle, or the chip's refusal (0xFF unanswered).
    async fn probe_service(&mut self, uuid: u16, records: u8) -> Result<u16, u8> {
        let u = uuid.to_le_bytes();
        match self.command(ACI_GATT_ADD_SERVICE, &[uuid_type::U16, u[0], u[1], service_type::PRIMARY, records]).await {
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
            sv[0], sv[1], uuid_type::U16, u[0], u[1], l[0], l[1], props::READ, permission::NONE, evt_mask::NONE, enc_key_size::MIN,
            value_length::CONSTANT,
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
        let advertising = Beside::Advertising { interval: self.advertising_interval() };
        match STATE.load(Ordering::Relaxed) {
            STATE_CONNECTED => {
                let link = Beside::Link { interval: INTERVAL.load(Ordering::Relaxed), latency: LATENCY.load(Ordering::Relaxed) };
                self.read_anchor(link).await;
            }
            STATE_ADVERTISING => {
                if self.stop_advertising().await {
                    self.read_anchor(Beside::Idle).await;
                    if self.restore_advertising().await {
                        self.read_anchor(advertising).await;
                    }
                }
            }
            _ => {
                self.read_anchor(Beside::Idle).await;
                if self.restore_advertising().await {
                    self.read_anchor(advertising).await;
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
            let status = self.command(ACI_UPDATER_START, &[]).await.map(|r| r.status);
            self.uart.write_str("[ble] probe: updater start sent, status ");
            match status {
                Ok(s) => self.uart.write_hex8(s),
                Err(why) => self.uart.write_str(why),
            }
            return self.uart.write_line("");
        }
        super::trace::packet(super::trace::Dir::Reset, &[]);
        self.chip.hold();
        Timer::after_millis(u64::from(RESET_MS)).await;
        self.chip.release();
        self.uart.write_line("[ble] probe: chip reset through its pin, unannounced");
    }

    async fn stop_advertising(&mut self) -> bool {
        let stopped = matches!(self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await, Ok(r) if r.status == 0);
        if !stopped {
            self.uart.write_line("[ble] probe: advertising would not stop");
        }
        stopped
    }

    /// The anchor period, labelled with what the probe left the radio doing.
    async fn read_anchor(&mut self, beside: Beside) {
        let reply = self.command(ACI_HAL_GET_ANCHOR_PERIOD, &[]).await.ok();
        write(self.uart, Line { probe: Probe::Anchor(beside), reply: reply.as_ref().map(|r| (r.status, &r.params[..r.len])) });
    }

    async fn read(&mut self, probe: Probe) {
        let reply = self.command(probe.opcode(), &[]).await.ok();
        write(self.uart, Line { probe, reply: reply.as_ref().map(|r| (r.status, &r.params[..r.len])) });
    }

    pub(super) async fn probe_snapshot(&mut self) {
        let mut snap = Snap::default();
        let mut packet = [0u8; transport::MAX_READ];
        while let Some(note) = self.chip.snapshotting(&mut snap, &mut packet).await {
            match note {
                Snapping::Got(got) => self.got(&packet, got),
                Snapping::Sending(c) => trace_command(c.opcode, c.params()),
                Snapping::Taken(r) => {
                    write(self.uart, Taken::Snapshot(&r));
                    for l in r.iter().flat_map(|s| s.links.iter().flatten()) {
                        write(self.uart, Taken::Link(l));
                    }
                }
            }
        }
    }
}

fn write(uart: Uart, line: impl fmt::Display) {
    let _ = write!(Text(uart), "[ble] {line}");
    uart.write_line("");
}

impl Link<'_> {
    /// Whether the chip takes `ACI_GAP_SET_DISCOVERABLE` while already advertising, one line per step.
    pub(super) async fn probe_advertising(&mut self) {
        if STATE.load(Ordering::Relaxed) == STATE_CONNECTED || self.scanner.until().is_some() {
            return self.uart.write_line("[ble] probe adv: connected or scanning; try while advertising");
        }
        for (step, (interval, stop_first)) in ADVERTISING_STEPS.into_iter().enumerate() {
            let stopped = if stop_first { self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await.ok().map(|r| r.status) } else { None };
            let status = self.discoverable(interval).await;
            let reply = self.command(ACI_HAL_GET_ANCHOR_PERIOD, &[]).await.ok();
            let mut t = Text(self.uart);
            let _ = write!(t, "[ble] probe adv step={step} interval={interval} stop_first={stop_first}");
            if let Some(s) = stopped {
                let _ = write!(t, " stop_status={s:02X}");
            }
            match status {
                Ok(s) => {
                    let _ = write!(t, " status={s:02X}");
                }
                Err(why) => {
                    let _ = write!(t, " error={why}");
                }
            }
            if let Some(r) = reply.filter(|r| r.status == 0 && r.len >= 8) {
                let p = &r.params;
                let anchor = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                let free = u32::from_le_bytes([p[4], p[5], p[6], p[7]]);
                let _ = write!(t, " anchor={anchor} free={free}");
            }
            self.uart.write_line("");
            Timer::after_millis(STEP_MS).await;
        }
        let _ = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await;
        self.restore_advertising().await;
        self.uart.write_line("[ble] probe adv: done");
    }

    /// The interval the core's record advertises at, else the power source's.
    fn advertising_interval(&self) -> u16 {
        self.shadow.state().radio.advertising.map_or(timing(power(), false).advertise, |a| a.interval_min.as_u16())
    }

    /// Advertising again after a probe stopped it, at [`Self::advertising_interval`]; whether it is.
    async fn restore_advertising(&mut self) -> bool {
        match self.discoverable(self.advertising_interval()).await {
            Ok(0) => true,
            Ok(status) => {
                self.uart.write_str("[ble] probe: advertising refused, status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
                false
            }
            Err(why) => {
                self.uart.write_str("[ble] probe: advertising: ");
                self.uart.write_line(why);
                false
            }
        }
    }

    /// Two `ACI_GAP_SET_NON_DISCOVERABLE` with nothing between, each status on the line.
    pub(super) async fn probe_stop(&mut self) {
        let state = STATE.load(Ordering::Relaxed);
        let first = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await.map(|r| r.status);
        let second = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await.map(|r| r.status);
        let mut t = Text(self.uart);
        let _ = write!(t, "[ble] probe stop state={state}");
        for status in [first, second] {
            let _ = match status {
                Ok(s) => write!(t, " status={s:02X}"),
                Err(why) => write!(t, " error={why}"),
            };
        }
        self.uart.write_line("");
    }

    /// `Op::Advertise`'s command at `interval`; the chip's status.
    async fn discoverable(&mut self, interval: u16) -> Result<u8, &'static str> {
        let slots = wrist_ble::Slots::from_u16(interval);
        let op = wrist_ble::Op::Advertise(Some(wrist_ble::AdvParams { interval_min: slots, interval_max: slots, connectable: true }));
        let Some(c) = wrist_bluenrg2::commands::command(&op, DEVICE_NAME, |_| None, None) else {
            return Err("not sent: no command carries it out");
        };
        self.command(c.opcode, c.params()).await.map(|r| r.status)
    }

    /// `ACI_GAP_TERMINATE` with `reason`, and the chip's status.
    pub(super) async fn probe_drop(&mut self, reason: u8) {
        if STATE.load(Ordering::Relaxed) != STATE_CONNECTED {
            return self.uart.write_line("[ble] probe drop: not connected");
        }
        let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
        super::trace::packet(super::trace::Dir::Disconnect, &[]);
        self.shadow.operator_disconnect(super::now_ms());
        let status = self.command(ACI_GAP_TERMINATE, &[h[0], h[1], reason]).await.map(|r| r.status);
        let mut t = Text(self.uart);
        let _ = match status {
            Ok(s) => write!(t, "[ble] probe drop reason={reason:02X} status={s:02X}"),
            Err(why) => write!(t, "[ble] probe drop reason={reason:02X} error={why}"),
        };
        self.uart.write_line("");
    }
}
