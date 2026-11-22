//! `ble probe`: the chip's firmware and updater versions, and its anchor period (PM0257 §4)
//! idle and beside advertising, or beside a central's link. `ble restart [updater]`: the chip
//! restarted under the driver, to check that the driver finds it and brings the chip back.
//! `ble probe snapshot`: the driver's snapshot, to check it against what the watch is doing.
//! `ble probe matrix`: advertising beside a central's link at each interval, in both orders.
//! `ble probe ll`: one boot of the chip without its own host.

use core::fmt::{self, Write};
use core::sync::atomic::Ordering;

use bluenrg2::opcode::{
    ACI_GAP_SET_NON_DISCOVERABLE, ACI_GAP_TERMINATE, ACI_GATT_ADD_CHAR, ACI_GATT_ADD_SERVICE, ACI_HAL_GET_ANCHOR_PERIOD,
    ACI_GATT_INIT, ACI_HAL_WRITE_CONFIG_DATA, ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ, ACI_UPDATER_START, HCI_SET_EVENT_MASK,
};
use bluenrg2::{enc_key_size, evt_mask, permission, props, service_type, uuid_type, value_length, Event, VendorEvent};
use wrist_bluenrg2::driver::{Got, Reply, RESET_MS};
use wrist_bluenrg2::probe::{Beside, Cell, Line, Matrix, Order, Probe, Taken, Update, MATRIX_TIMEOUT_MS};
use wrist_bluenrg2::snapshot::{Snap, Snapping};
use wrist_bluenrg2::transport;

use embassy_futures::select::{select, Either};
use embassy_time::{Instant, Timer};

use wrist_bluenrg2::wake::ble::timing;

use super::{power, trace_command, Link, CONN_HANDLE, DEVICE_NAME, INTERVAL, LATENCY, STATE, STATE_ADVERTISING, STATE_CONNECTED};
use crate::uart::Uart;

/// `ble probe adv`'s steps: the interval (0.625 ms units) each `ACI_GAP_SET_DISCOVERABLE` asks
/// for, and whether `ACI_GAP_SET_NON_DISCOVERABLE` goes first.
const ADVERTISING_STEPS: [(u16, bool); 4] = [(160, true), (1636, false), (160, false), (160, false)];

/// How long each step of `ble probe adv` advertises, for a host to time the advertisements.
const STEP_MS: u64 = 6_000;

/// The cells of `ble probe matrix`.
pub(super) const CELLS: u8 = Cell::COUNT;

/// How long a cell waits for the central's answer and the update it makes: six events at the
/// longest interval, with room.
const UPDATE_WITHIN_MS: u64 = 15_000;

/// How long a cell holds both changes before its last reading, for at least one event of the
/// longest advertising interval.
const HOLD_MS: u64 = 3_000;

/// Core 5.2 Vol 4 Part E §7.8: the HCI commands `ble probe ll` advertises with.
const HCI_LE_SET_RANDOM_ADDRESS: u16 = 0x2005;
const HCI_LE_SET_ADVERTISING_PARAMETERS: u16 = 0x2006;
const HCI_LE_SET_ADVERTISING_DATA: u16 = 0x2008;
const HCI_LE_SET_ADVERTISING_ENABLE: u16 = 0x200A;

/// `ACI_HAL_WRITE_CONFIG_DATA`'s link-layer-only switch (ST's `bluenrg1_hal_aci.h`, `CONFIG_DATA_LL_WITHOUT_HOST`).
const LL_WITHOUT_HOST: u8 = 0x2C;

/// `ble probe ll`'s static random address, not the watch's, so that no host's bond meets it.
const LL_ADDRESS: [u8; 6] = [0x01, 0x00, 0x00, 0x00, 0x2C, 0xC0];

/// `ble probe ll` waits this long for a host to connect, then this long on the link.
const LL_CONNECT_WITHIN_MS: u64 = 60_000;
const LL_LINK_MS: u64 = 30_000;

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
            Gatt::Records => self.probe_records().await,
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
                        Ok(_) => {
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

    /// The largest service the table still takes, then one-byte characteristics in it until full.
    async fn probe_records(&mut self) {
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
        let (mut n, mut last) = (0u32, service);
        let status = loop {
            match self.probe_char(service, PROBE_UUID + 1 + u16::try_from(n).unwrap_or(0), 1).await {
                Ok(h) if n < 200 => {
                    n += 1;
                    last = h;
                }
                Ok(_) => break 0,
                Err(s) => break s,
            }
        };
        self.uart.write_str("[ble] probe gatt records: ");
        self.uart.write_u32(n);
        self.uart.write_str(" one-byte characteristics (two records each) in it, then status ");
        self.uart.write_hex8(status);
        self.uart.write_line("");
        // A characteristic's value follows its declaration's handle.
        let mut t = Text(self.uart);
        let _ = write!(t, "[ble] probe gatt records: service handle=0x{service:04x} last value handle=0x{:04x}", last.saturating_add(1));
        self.uart.write_line("");
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

    /// A readable fixed-length characteristic of `len` bytes: its declaration's handle.
    async fn probe_char(&mut self, service: u16, uuid: u16, len: u16) -> Result<u16, u8> {
        let sv = service.to_le_bytes();
        let u = uuid.to_le_bytes();
        let l = len.to_le_bytes();
        let p = [
            sv[0], sv[1], uuid_type::U16, u[0], u[1], l[0], l[1], props::READ, permission::NONE, evt_mask::NONE, enc_key_size::MIN,
            value_length::CONSTANT,
        ];
        match self.command(ACI_GATT_ADD_CHAR, &p).await {
            Ok(r) if r.status == 0 && r.len >= 2 => Ok(u16::from_le_bytes([r.params[0], r.params[1]])),
            Ok(r) if r.status == 0 => Err(0xFE),
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

/// A reply's status and parameters, as a probe line takes them.
fn params(r: Option<&Reply>) -> Option<(u8, &[u8])> {
    r.map(|r| (r.status, &r.params[..r.len]))
}

fn connected() -> bool {
    STATE.load(Ordering::Relaxed) == STATE_CONNECTED
}

impl Link<'_> {
    /// Each cell from `from`, one line per cell, until the link goes.
    pub(super) async fn probe_matrix(&mut self, from: u8) {
        if !connected() || self.scanner.until().is_some() {
            return self.uart.write_line("[ble] probe matrix: needs a central's link and no scan (tools/ble-probe-host.py holds one)");
        }
        let _ = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await;
        let mut index = from;
        while let Some(cell) = Cell::nth(index) {
            if !self.matrix_cell(cell).await {
                let mut t = Text(self.uart);
                let _ = write!(t, "[ble] probe matrix: the link went in cell {index}; connect again and `ble probe matrix from {index}`");
                self.uart.write_line("");
                break;
            }
            index += 1;
        }
        if index == CELLS {
            self.uart.write_line("[ble] probe matrix: done");
        }
        // The core's record holds none of the probe's commands: it takes a snapshot and plans again.
        self.lost_read();
    }

    /// One cell; whether the link was still up at its end.
    async fn matrix_cell(&mut self, cell: Cell) -> bool {
        let (update, advertising, first) = match cell.order {
            Order::LinkFirst => {
                let update = self.matrix_update(cell).await;
                let first = self.anchor_reply().await;
                (update, self.discoverable(cell.adv_units()).await.ok(), first)
            }
            Order::AdvertisingFirst => {
                let advertising = self.discoverable(cell.adv_units()).await.ok();
                let first = self.anchor_reply().await;
                (self.matrix_update(cell).await, advertising, first)
            }
        };
        self.events_for(HOLD_MS, |_| false).await;
        let second = self.anchor_reply().await;
        let link_up = connected();
        write(self.uart, Matrix { cell, update, advertising, anchors: [params(first.as_ref()), params(second.as_ref())], link_up });
        if advertising == Some(0) {
            let _ = self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await;
        }
        link_up
    }

    /// The link's interval asked of the central, and what came of it.
    async fn matrix_update(&mut self, cell: Cell) -> Update {
        self.update_seen = Update::default();
        let h = CONN_HANDLE.load(Ordering::Relaxed).to_le_bytes();
        let i = cell.link_units().to_le_bytes();
        let to = (MATRIX_TIMEOUT_MS / 10).to_le_bytes();
        let status = self.command(ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ, &[h[0], h[1], i[0], i[1], i[0], i[1], 0, 0, to[0], to[1]]).await;
        self.update_seen.status = status.ok().map(|r| r.status);
        if self.update_seen.status == Some(0) {
            self.events_for(UPDATE_WITHIN_MS, |l| l.update_seen.done.is_some() || l.update_seen.accepted == Some(false)).await;
        }
        self.update_seen
    }

    async fn anchor_reply(&mut self) -> Option<Reply> {
        self.command(ACI_HAL_GET_ANCHOR_PERIOD, &[]).await.ok()
    }

    /// The chip's events handled for up to `ms`, until `done` or the link goes.
    async fn events_for(&mut self, ms: u64, done: fn(&Self) -> bool) {
        let end = Instant::now() + embassy_time::Duration::from_millis(ms);
        while connected() && !done(self) {
            match select(self.chip.wait_pending(), Timer::at(end)).await {
                Either::First(()) => self.drain().await,
                Either::Second(()) => break,
            }
        }
    }

    /// The link's update as the chip reports it, for `ble probe matrix`.
    pub(super) fn saw_update(&mut self, p: &[u8]) {
        match bluenrg2::parse(p) {
            Event::LeConnectionUpdateComplete { conn_interval, peripheral_latency, .. } => {
                self.update_seen.done = Some((0, conn_interval.as_u16(), peripheral_latency));
            }
            Event::LeConnectionUpdateFailed { status, .. } => self.update_seen.done = Some((status.into_inner(), 0, 0)),
            Event::Vendor(VendorEvent::L2capUpdateResponse { accepted, .. }) => self.update_seen.accepted = Some(accepted),
            _ => {}
        }
    }

    /// One boot with `0x2C` written (0 for `control`), advertising by HCI commands as `UNA-LL` and
    /// printing every packet from the chip, then the chip reset for the driver to bring back.
    pub(super) async fn probe_link_layer(&mut self, control: bool) {
        if connected() || self.scanner.until().is_some() {
            return self.uart.write_line("[ble] probe ll: end the link and any scan first");
        }
        self.uart.write_line("[ble] probe ll: the chip reset for one boot without its host");
        self.chip.reset().await;
        let mut packet = [0u8; transport::MAX_READ];
        if !self.chip.boot().await {
            self.uart.write_line("[ble] probe ll: no boot after the reset");
            return self.end_link_layer().await;
        }
        // The boot, and the ends of operations the reset cut short: none of it the core's now.
        while self.chip.next(&mut packet).await.is_some() {}
        let mut data = [0u8; 32];
        let ad = [2, 0x01, 0x06, 7, 0x09, b'U', b'N', b'A', b'-', b'L', b'L'];
        data[0] = 31;
        data[1..=ad.len()].copy_from_slice(&ad);
        let interval = 160u16.to_le_bytes();
        let steps: [(&str, u16, &[u8]); 7] = [
            ("config", ACI_HAL_WRITE_CONFIG_DATA, &[LL_WITHOUT_HOST, 1, u8::from(!control)]),
            ("mask", HCI_SET_EVENT_MASK, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x1F, 0x00, 0x20]),
            ("gatt", ACI_GATT_INIT, &[]),
            ("address", HCI_LE_SET_RANDOM_ADDRESS, &LL_ADDRESS),
            ("params", HCI_LE_SET_ADVERTISING_PARAMETERS, &[interval[0], interval[1], interval[0], interval[1], 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x07, 0]),
            ("data", HCI_LE_SET_ADVERTISING_DATA, &data),
            ("enable", HCI_LE_SET_ADVERTISING_ENABLE, &[1]),
        ];
        let mut t = Text(self.uart);
        let _ = write!(t, "[ble] probe ll{}:", if control { " control" } else { "" });
        for (name, opcode, params) in steps {
            let status = self.ll_command(opcode, params, &mut packet).await;
            let mut t = Text(self.uart);
            let _ = match status {
                Some(s) => write!(t, " {name}=0x{s:02x}"),
                None => write!(t, " {name}=none"),
            };
        }
        self.uart.write_line("");
        self.uart.write_line("[ble] probe ll: advertising as UNA-LL from C0:2C:00:00:00:01 (random) for 60 s");
        let (mut acl, mut events, mut other, mut linked) = (0u32, 0u32, 0u32, false);
        let mut end = Instant::now() + embassy_time::Duration::from_millis(LL_CONNECT_WITHIN_MS);
        while let Either::First(()) = select(self.chip.wait_pending(), Timer::at(end)).await {
            self.chip.drain();
            while let Some(got) = self.chip.next(&mut packet).await {
                let Got::Packet { len, .. } = got else {
                    other += 1;
                    self.uart.write_line(match got {
                        Got::Event(wrist_ble::RadioEvent::EventsLost) => "[ble] probe ll: a read lost",
                        Got::Unreadable { .. } => "[ble] probe ll: a header unreadable",
                        _ => "[ble] probe ll: something not a packet",
                    });
                    continue;
                };
                let p = &packet[..len];
                let is_acl = p.first() == Some(&bluenrg2::packet::ACL);
                if is_acl {
                    acl += 1;
                } else {
                    events += 1;
                }
                if !linked && matches!(bluenrg2::parse(p), Event::LeConnectionComplete { .. }) {
                    linked = true;
                    end = Instant::now() + embassy_time::Duration::from_millis(LL_LINK_MS);
                }
                self.uart.write_str(if is_acl { "[ble] probe ll: acl " } else { "[ble] probe ll: event " });
                super::write_hex(self.uart, &p[..p.len().min(40)]);
                self.uart.write_line("");
            }
        }
        let mut t = Text(self.uart);
        let _ = write!(t, "[ble] probe ll: done linked={} acl={acl} events={events} other={other}", u8::from(linked));
        self.uart.write_line("");
        self.end_link_layer().await;
    }

    /// A command to the chip in `ble probe ll`, outside the core: its status.
    async fn ll_command(&mut self, opcode: u16, params: &[u8], packet: &mut [u8; transport::MAX_READ]) -> Option<u8> {
        self.chip.send(opcode, params).await.ok()?;
        while self.chip.awaiting().await {
            while self.chip.next(packet).await.is_some() {}
        }
        self.chip.reply().ok().map(|r| r.status)
    }

    /// The chip reset again through its pin, unannounced, and its boot read: the driver brings
    /// it back as from a restart of its own.
    async fn end_link_layer(&mut self) {
        super::trace::packet(super::trace::Dir::Reset, &[]);
        self.chip.hold();
        Timer::after_millis(u64::from(RESET_MS)).await;
        self.chip.release();
        if let Either::First(()) = select(self.chip.wait_pending(), Timer::after_millis(1_500)).await {
            self.drain().await;
        }
        self.uart.write_line("[ble] probe ll: the chip reset; the driver brings it back");
    }
}
