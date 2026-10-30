//! The watch as a central: one outbound link (the "sensor link") beside the phone's, a GATT
//! client on it, and heart rate read from a strap.

use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU8, Ordering};

use embassy_time::{Duration, Instant};
use wrist_aci::opcode::{
    ACI_GAP_CREATE_CONNECTION, ACI_GAP_START_CONNECTION_UPDATE, ACI_GAP_TERMINATE, ACI_GAP_TERMINATE_GAP_PROC, ACI_GATT_CONFIRM_INDICATION,
    ACI_GATT_DISC_ALL_CHAR_DESC, ACI_GATT_DISC_ALL_CHAR_OF_SERVICE, ACI_GATT_DISC_ALL_PRIMARY_SERVICES,
    ACI_GATT_DISC_PRIMARY_SERVICE_BY_UUID, ACI_GATT_READ_CHAR_VALUE, ACI_GATT_WRITE_CHAR_DESC, ACI_GATT_WRITE_CHAR_VALUE,
    ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_RESP,
};
use wrist_aci::{address, gap_proc, vendor, Address, L2capUpdateRequest, Uuid};
use wrist_profiles::heart_rate::{self, rr_ms, Contact, Measurement};
use wrist_wake::ble::{connect_timeout_ms, readvertise_for, reconnect_after_ms, scan, sensor, sensor_procedure_ms, Timing, GIVE_UP_MS};

use super::scan::{write_address, write_uuid};
use super::{power, Link, Request, REQUESTS, STATE, STATE_ADVERTISING, STATE_IDLE};
use crate::uart::Uart;

/// A request for the sensor link's timing that the chip refused is tried again this much later.
const RETIME_AGAIN: Duration = Duration::from_secs(5);
/// A strap with no reading for this long is quiet.
const QUIET: Duration = Duration::from_secs(5);
/// A link this quiet is dropped and the strap looked for again, since a host that stops
/// serving keeps the link.
const STALE: Duration = Duration::from_secs(30);
/// `hr fake` notifies this often, as a strap does.
const FAKE_EVERY: Duration = Duration::from_secs(1);
/// Generic notifications are printed at most this often; the rest are counted.
const PRINT_PACE: Duration = Duration::from_millis(250);
/// The longest value `ble write` takes.
pub(super) const WRITE_MAX: usize = 20;
/// ATT error: insufficient authentication.
const ATT_INSUFFICIENT_AUTHENTICATION: u8 = 0x05;

/// Console requests for the central side.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Ask {
    Connect(Address),
    Drop,
    Services,
    Chars(u16, u16),
    Descs(u16, u16),
    Read(u16),
    Write(u16, [u8; WRITE_MAX], u8),
    /// The CCCD and its value: 0 off, 1 notifications, 2 indications.
    Subscribe(u16, u8),
    Strap(Address),
    HrOff,
    Fake(u16),
}

const CENTRAL_UNAVAILABLE: u8 = 0;
const CENTRAL_IDLE: u8 = 1;
const CENTRAL_CONNECTING: u8 = 3;
const CENTRAL_CONNECTED: u8 = 4;
static CENTRAL: AtomicU8 = AtomicU8::new(CENTRAL_UNAVAILABLE);
static PEER: [AtomicU8; 7] = [const { AtomicU8::new(0) }; 7];
static NOTIFICATIONS: AtomicU32 = AtomicU32::new(0);
static UNPRINTED: AtomicU32 = AtomicU32::new(0);

const HR_OFF: u8 = 0;
const HR_CONNECTING: u8 = 1;
const HR_DISCOVERING: u8 = 2;
const HR_LIVE: u8 = 3;
const HR_LOST: u8 = 4;
const HR_GAVE_UP: u8 = 5;
const HR_FAKE: u8 = 6;
const HR_NOT_STRAP: u8 = 7;
static HR: AtomicU8 = AtomicU8::new(HR_OFF);
static HR_BPM: AtomicU16 = AtomicU16::new(0);
/// 0 unsupported, 1 lost, 2 detected.
static HR_CONTACT: AtomicU8 = AtomicU8::new(0);
static HR_RR_MS: AtomicU32 = AtomicU32::new(0);
/// When the last reading came, in milliseconds since boot (0: none yet).
static HR_AT_MS: AtomicU32 = AtomicU32::new(0);
static HR_READINGS: AtomicU32 = AtomicU32::new(0);

/// What `ble <args>` meant to the central side.
pub(super) enum Parsed {
    /// None of its words.
    NotCentral,
    /// One of them mistyped; its usage is printed.
    Mistyped,
    Request(Request),
}

/// `ble connect|drop|services|chars|descs|read|write|subscribe ...`.
pub(super) fn ble_command(args: &[u8], uart: Uart) -> Parsed {
    let text = core::str::from_utf8(args).unwrap_or("");
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return Parsed::NotCentral;
    };
    let ask = match first {
        "connect" => match parse_address(words.next(), words.next()) {
            Some(a) => Ask::Connect(a),
            None => return usage(uart, "ble connect AA:BB:CC:DD:EE:FF [random]"),
        },
        "drop" => Ask::Drop,
        "services" => Ask::Services,
        w @ ("chars" | "descs") => match (parse_handle(words.next()), parse_handle(words.next())) {
            (Some(s), Some(e)) if s <= e => {
                if w == "chars" {
                    Ask::Chars(s, e)
                } else {
                    Ask::Descs(s, e)
                }
            }
            _ => return usage(uart, "ble chars|descs <start> <end> (handles, decimal or 0x)"),
        },
        "read" => match parse_handle(words.next()) {
            Some(h) => Ask::Read(h),
            None => return usage(uart, "ble read <handle>"),
        },
        "write" => {
            let h = parse_handle(words.next());
            let mut value = [0u8; WRITE_MAX];
            let n = words.next().and_then(|hex| parse_hex(hex, &mut value));
            match (h, n) {
                (Some(h), Some(n)) => Ask::Write(h, value, n),
                _ => return usage(uart, "ble write <handle> <hex bytes, up to 20>"),
            }
        }
        "subscribe" => match (parse_handle(words.next()), words.next()) {
            (Some(h), None) => Ask::Subscribe(h, 1),
            (Some(h), Some("indicate")) => Ask::Subscribe(h, 2),
            (Some(h), Some("off")) => Ask::Subscribe(h, 0),
            _ => return usage(uart, "ble subscribe <cccd handle> [indicate|off]"),
        },
        _ => return Parsed::NotCentral,
    };
    Parsed::Request(Request::Central(ask))
}

/// `hr`, `hr strap <addr> [random]`, `hr off`, `hr fake <bpm>`.
pub fn hr_command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("");
    let mut words = text.split_whitespace();
    let ask = match words.next() {
        None => return write_hr(uart),
        Some("strap") => match parse_address(words.next(), words.next()) {
            Some(a) => Ask::Strap(a),
            None => return uart.write_line("[hr] usage: hr strap AA:BB:CC:DD:EE:FF [random]"),
        },
        Some("off") => Ask::HrOff,
        Some("fake") => match words.next().map(str::parse::<u16>) {
            Some(Ok(bpm)) if (20..=250).contains(&bpm) => Ask::Fake(bpm),
            _ => return uart.write_line("[hr] usage: hr fake <bpm, 20 to 250> (`hr off` stops it)"),
        },
        Some(_) => return uart.write_line("[hr] usage: hr [strap <addr> [random]|off|fake <bpm>]"),
    };
    if REQUESTS.try_send(Request::Central(ask)).is_err() {
        uart.write_line("[hr] not queued: four already waiting");
    }
}

fn usage(uart: Uart, text: &str) -> Parsed {
    uart.write_str("[ble] usage: ");
    uart.write_line(text);
    Parsed::Mistyped
}

/// `AA:BB:CC:DD:EE:FF`, most significant first as people write it, then `random` or nothing.
fn parse_address(text: Option<&str>, kind: Option<&str>) -> Option<Address> {
    let mut bytes = [0u8; 6];
    let mut parts = text?.split(':');
    for slot in bytes.iter_mut().rev() {
        let p = parts.next()?;
        if p.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(p, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let kind = match kind {
        None | Some("public") => 0,
        Some("random") => 1,
        Some(_) => return None,
    };
    Some(Address { kind, bytes })
}

fn parse_handle(text: Option<&str>) -> Option<u16> {
    let t = text?;
    match t.strip_prefix("0x") {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

/// Hex digits, two per byte, into `out`; the byte count.
fn parse_hex(text: &str, out: &mut [u8; WRITE_MAX]) -> Option<u8> {
    let b = text.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(2) || b.len() / 2 > WRITE_MAX {
        return None;
    }
    for (slot, pair) in out.iter_mut().zip(b.as_chunks::<2>().0) {
        *slot = u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    u8::try_from(b.len() / 2).ok()
}

/// The central half of `ble`'s status line.
pub(super) fn write_status(uart: Uart) {
    uart.write_str("[ble] central: ");
    uart.write_str(match CENTRAL.load(Ordering::Relaxed) {
        CENTRAL_UNAVAILABLE => "not available (radio off, or the chip refused the role)",
        CENTRAL_IDLE if super::scan::scanning() => "scanning",
        CENTRAL_IDLE => "idle",
        CENTRAL_CONNECTING => "connecting to ",
        _ => "connected to ",
    });
    if matches!(CENTRAL.load(Ordering::Relaxed), CENTRAL_CONNECTING | CENTRAL_CONNECTED) {
        let mut a = Address { kind: PEER[6].load(Ordering::Relaxed), ..Address::default() };
        for (b, s) in a.bytes.iter_mut().zip(PEER.iter()) {
            *b = s.load(Ordering::Relaxed);
        }
        write_address(uart, a);
    }
    uart.write_str(", notifications ");
    uart.write_u32(NOTIFICATIONS.load(Ordering::Relaxed));
    uart.write_str(" (unprinted ");
    uart.write_u32(UNPRINTED.load(Ordering::Relaxed));
    uart.write_line(")");
}

/// `hr`: the strap's state, and the last reading and its age.
fn write_hr(uart: Uart) {
    uart.write_str("[hr] ");
    let state = HR.load(Ordering::Relaxed);
    uart.write_str(match state {
        HR_OFF => "off",
        HR_CONNECTING => "connecting to the strap",
        HR_DISCOVERING => "connected, finding heart rate",
        HR_LIVE => "strap live",
        HR_LOST => "strap lost, looking for it",
        HR_GAVE_UP => "strap lost for 5 minutes: gave up (`hr strap` again)",
        HR_NOT_STRAP => "not a heart-rate strap (`ble drop` ends the link)",
        _ => "fake readings",
    });
    let at = HR_AT_MS.load(Ordering::Relaxed);
    if at != 0 {
        uart.write_str("; last ");
        uart.write_u32(u32::from(HR_BPM.load(Ordering::Relaxed)));
        uart.write_str(" bpm, ");
        uart.write_str(match HR_CONTACT.load(Ordering::Relaxed) {
            0 => "contact not reported",
            1 => "NO skin contact",
            _ => "skin contact",
        });
        let rr = HR_RR_MS.load(Ordering::Relaxed);
        if rr != 0 {
            uart.write_str(", RR ");
            uart.write_u32(rr);
            uart.write_str(" ms");
        }
        let age = now_ms().wrapping_sub(at);
        uart.write_str(", ");
        uart.write_u32(age / 1000);
        uart.write_str(if u64::from(age) > QUIET.as_millis() { " s ago (QUIET)" } else { " s ago" });
        uart.write_str(", readings ");
        uart.write_u32(HR_READINGS.load(Ordering::Relaxed));
    }
    uart.write_line("");
}

// Wraps after 49 days; ages are taken with wrapping_sub.
#[allow(clippy::cast_possible_truncation)]
fn now_ms() -> u32 {
    Instant::now().as_millis() as u32
}

fn write_hex_bytes(uart: Uart, bytes: &[u8]) {
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            uart.write_str(" ");
        }
        uart.write_hex8(*b);
    }
}

fn write_handle(uart: Uart, h: u16) {
    uart.write_str("0x");
    uart.write_hex8((h >> 8) as u8);
    uart.write_hex8((h & 0xFF) as u8);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proc {
    Services,
    Chars,
    Descs,
    Read,
    Write,
    Subscribe,
    StrapService,
    StrapChars,
    StrapDescs,
    StrapSubscribe,
}

/// Where the strap's set-up has got to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// No link yet: connect at `retry_at`.
    Connect,
    Service,
    Chars,
    Descs,
    Subscribe,
    Live,
}

struct Strap {
    address: Address,
    stage: Stage,
    range: Option<(u16, u16)>,
    measurement: Option<u16>,
    cccd: Option<u16>,
    /// A characteristic declaration came after the measurement: descriptors stop there.
    after_measurement: bool,
    retry_at: Instant,
    lost_since: Option<Instant>,
    /// When the subscription last came up.
    live_at: Option<Instant>,
    /// The quiet state has been logged since the last reading.
    quiet_told: bool,
    /// A reading has been logged since the subscription.
    announced: bool,
    contact: Contact,
}

#[derive(Default)]
pub(super) struct Central {
    /// The chip took the central role at GAP init.
    pub available: bool,
    connecting: Option<(Address, Instant)>,
    link: Option<(u16, Address)>,
    /// The link was ended so that advertising could start: the strap is looked for at once.
    pub(super) reconnect_now: bool,
    /// The interval last asked for the sensor link.
    requested: Option<u16>,
    /// The sensor link's connection interval (1.25 ms units).
    link_interval: Option<u16>,
    proc: Option<(Proc, Instant)>,
    /// The sensor link's timing last asked.
    asked: Option<Timing>,
    /// The chip refused the last request for the sensor link's timing: it is tried again then.
    retime_at: Option<Instant>,
    strap: Option<Strap>,
    /// `hr fake`: the rate, and when the next reading goes.
    fake: Option<(u16, Instant)>,
    /// Work events left: an indication to confirm, a parameter request.
    confirm: bool,
    update_request: Option<L2capUpdateRequest>,
    printed_at: Option<Instant>,
}

impl Central {
    pub(super) fn connecting(&self) -> bool {
        self.connecting.is_some()
    }

    pub(super) fn handle(&self) -> Option<u16> {
        self.link.map(|(h, _)| h)
    }

    /// The sensor link's interval, while it is up.
    pub(super) fn interval(&self) -> Option<u16> {
        self.link.and(self.link_interval)
    }

    pub(super) fn set_interval(&mut self, iv: u16) {
        self.link_interval = Some(iv);
    }

    /// When the loop must next run for the central's sake; a strap's retry waits out a scan.
    pub(super) fn due(&self, scanning: bool) -> Option<Instant> {
        let now = Instant::now();
        let work = self.confirm
            || self.update_request.is_some()
            || (self.proc.is_none()
                && self.link.is_some()
                && self.strap.as_ref().is_some_and(|s| !matches!(s.stage, Stage::Connect | Stage::Live)));
        let strap = self.strap.as_ref().and_then(|s| match s.stage {
            Stage::Connect if self.link.is_none() && self.connecting.is_none() && !scanning => Some(s.retry_at),
            Stage::Live if self.proc.is_none() => {
                let at = HR_AT_MS.load(Ordering::Relaxed);
                let age = Duration::from_millis(u64::from(now_ms().wrapping_sub(at)));
                let limit = if s.quiet_told { STALE } else { QUIET };
                Some(now + limit.checked_sub(age).unwrap_or(Duration::from_ticks(0)) + Duration::from_millis(1))
            }
            _ => None,
        });
        [
            work.then_some(now),
            self.connecting.map(|(_, since)| since + connect_timeout()),
            self.proc.map(|(_, since)| since + self.procedure_timeout()),
            self.retime_at.filter(|_| self.link.is_some() && self.proc.is_none()),
            strap,
            self.fake.map(|(_, at)| at),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn procedure_timeout(&self) -> Duration {
        Duration::from_millis(sensor_procedure_ms(self.interval()))
    }

    fn set_peer(a: Address) {
        for (s, b) in PEER.iter().zip(a.bytes) {
            s.store(b, Ordering::Relaxed);
        }
        PEER[6].store(a.kind, Ordering::Relaxed);
    }

    /// The radio is up: whether the chip took the central role.
    pub(super) fn started(&mut self, available: bool) {
        *self = Self { available, ..Self::default() };
        CENTRAL.store(if available { CENTRAL_IDLE } else { CENTRAL_UNAVAILABLE }, Ordering::Relaxed);
        HR.store(HR_OFF, Ordering::Relaxed);
    }

    /// The radio is going off.
    pub(super) fn stopped() {
        CENTRAL.store(CENTRAL_UNAVAILABLE, Ordering::Relaxed);
        HR.store(HR_OFF, Ordering::Relaxed);
    }

    /// LE Connection Complete with the watch as the central.
    pub(super) fn connected(&mut self, handle: u16, peer: Address, interval: Option<(u16, u16)>, uart: Uart) {
        self.connecting = None;
        self.link = Some((handle, peer));
        self.requested = None;
        self.link_interval = interval.map(|(iv, _)| iv);
        self.retime_at = None;
        self.proc = None;
        Self::set_peer(peer);
        CENTRAL.store(CENTRAL_CONNECTED, Ordering::Relaxed);
        uart.write_str("[ble] connected to ");
        write_address(uart, peer);
        if let Some((iv, _)) = interval {
            uart.write_str(" as the central, interval ");
            super::write_interval(uart, iv);
        }
        uart.write_line("");
        if let Some(s) = self.strap.as_mut() {
            if s.address == peer {
                s.stage = Stage::Service;
                s.range = None;
                s.measurement = None;
                s.cccd = None;
                HR.store(HR_DISCOVERING, Ordering::Relaxed);
            }
        }
    }

    /// LE Connection Complete with an error, or the connection procedure ended without one.
    pub(super) fn connect_failed(&mut self, status: u8, uart: Uart) {
        if self.connecting.take().is_none() {
            return;
        }
        CENTRAL.store(CENTRAL_IDLE, Ordering::Relaxed);
        uart.write_str("[ble] connection failed, status ");
        uart.write_hex8(status);
        uart.write_line("");
        self.strap_retry();
    }

    /// The sensor link went down.
    pub(super) fn disconnected(&mut self, reason: u8, uart: Uart) {
        self.link = None;
        self.requested = None;
        let _ = HR.compare_exchange(HR_NOT_STRAP, HR_OFF, Ordering::Relaxed, Ordering::Relaxed);
        self.proc = None;
        self.confirm = false;
        self.update_request = None;
        CENTRAL.store(CENTRAL_IDLE, Ordering::Relaxed);
        uart.write_str("[ble] sensor link down, reason ");
        uart.write_hex8(reason);
        uart.write_line(" (08 timeout, 13 the peer hung up, 16 we did)");
        self.strap_retry();
    }

    /// A strap still wanted is looked for again after the power source's backoff; one whose
    /// link was ended so that advertising could start is not lost, and is looked for at once.
    fn strap_retry(&mut self) {
        let reorder = core::mem::take(&mut self.reconnect_now);
        let Some(s) = self.strap.as_mut() else {
            return;
        };
        s.stage = Stage::Connect;
        s.live_at = None;
        s.retry_at = Instant::now();
        if reorder {
            if s.announced {
                s.lost_since = None;
            }
            return HR.store(HR_CONNECTING, Ordering::Relaxed);
        }
        s.lost_since.get_or_insert(Instant::now());
        s.retry_at += Duration::from_millis(reconnect_after_ms(power()));
        HR.store(HR_LOST, Ordering::Relaxed);
    }

    /// `GAP_PROC_COMPLETE` for a connection attempt; success comes before the connection's own
    /// event, which ends the attempt.
    pub(super) fn gap_proc_complete(&mut self, code: u8, status: u8, uart: Uart) {
        if code == gap_proc::DIRECT_CONNECTION && status != 0 && self.connecting.is_some() && self.link.is_none() {
            self.connect_failed(status, uart);
        }
    }

    /// A GATT client event on the sensor link.
    pub(super) fn client_event(&mut self, code: u16, rest: &[u8], uart: Uart) {
        let proc = self.proc.map(|(p, _)| p);
        match code {
            vendor::ATT_READ_BY_GROUP_TYPE_RESP => {
                for s in wrist_aci::services(rest) {
                    uart.write_str("[ble] service ");
                    write_handle(uart, s.start);
                    uart.write_str("..");
                    write_handle(uart, s.end);
                    uart.write_str(" ");
                    write_uuid(uart, s.uuid);
                    uart.write_line("");
                }
            }
            vendor::ATT_READ_BY_TYPE_RESP => {
                for c in wrist_aci::characteristics(rest) {
                    if proc == Some(Proc::StrapChars) {
                        if let Some(s) = self.strap.as_mut() {
                            if c.uuid == Uuid::U16(heart_rate::MEASUREMENT) {
                                s.measurement = Some(c.value);
                            }
                        }
                        continue;
                    }
                    uart.write_str("[ble] characteristic ");
                    write_handle(uart, c.value);
                    uart.write_str(" ");
                    write_uuid(uart, c.uuid);
                    uart.write_str(" properties ");
                    uart.write_hex8(c.properties);
                    uart.write_line(" (02 read, 04/08 write, 10 notify, 20 indicate)");
                }
            }
            vendor::ATT_FIND_INFO_RESP => {
                for (h, u) in wrist_aci::descriptors(rest) {
                    if proc == Some(Proc::StrapDescs) {
                        if let Some(s) = self.strap.as_mut() {
                            match u {
                                Uuid::U16(0x2803) if s.measurement.is_some_and(|m| h > m) => s.after_measurement = true,
                                Uuid::U16(heart_rate::CCCD) if !s.after_measurement && s.cccd.is_none() => s.cccd = Some(h),
                                _ => {}
                            }
                        }
                        continue;
                    }
                    uart.write_str("[ble] descriptor ");
                    write_handle(uart, h);
                    uart.write_str(" ");
                    write_uuid(uart, u);
                    uart.write_line("");
                }
            }
            vendor::ATT_FIND_BY_TYPE_VALUE_RESP => {
                if let (Some(Proc::StrapService), Some(s)) = (proc, self.strap.as_mut()) {
                    s.range = s.range.or(wrist_aci::service_range(rest));
                }
            }
            vendor::ATT_READ_RESP => {
                uart.write_str("[ble] read: ");
                write_hex_bytes(uart, wrist_aci::read_value(rest).unwrap_or(&[]));
                uart.write_line("");
            }
            vendor::GATT_NOTIFICATION | vendor::GATT_INDICATION => {
                if code == vendor::GATT_INDICATION {
                    self.confirm = true;
                }
                if let Some((attr, value)) = wrist_aci::notification(rest) {
                    self.notification(attr, value, uart);
                }
            }
            vendor::GATT_ERROR_RESP => {
                if let Some((request, attr, error)) = wrist_aci::gatt_error(rest) {
                    // "Attribute not found" ends every discovery; it is not an error.
                    if error != 0x0A {
                        uart.write_str("[ble] ATT error ");
                        uart.write_hex8(error);
                        uart.write_str(" on request ");
                        uart.write_hex8(request);
                        uart.write_str(" at ");
                        write_handle(uart, attr);
                        uart.write_line(if error == ATT_INSUFFICIENT_AUTHENTICATION {
                            " (05: the device wants pairing first)"
                        } else {
                            ""
                        });
                    }
                }
            }
            vendor::GATT_PROC_COMPLETE => self.proc_complete(wrist_aci::proc_complete(rest).unwrap_or(0xFF), uart),
            vendor::GATT_PROC_TIMEOUT => {
                uart.write_line("[ble] the sensor's GATT procedure timed out (30 s); the link is unusable");
                self.proc = None;
            }
            vendor::L2CAP_CONNECTION_UPDATE_REQ => self.update_request = L2capUpdateRequest::decode(rest),
            _ => {}
        }
    }

    fn notification(&mut self, attr: u16, value: &[u8], uart: Uart) {
        NOTIFICATIONS.fetch_add(1, Ordering::Relaxed);
        if let Some(s) = self.strap.as_mut() {
            if s.stage == Stage::Live && s.measurement == Some(attr) {
                if let Some(m) = Measurement::decode(value) {
                    let first = !s.announced || s.quiet_told;
                    s.announced = true;
                    let contact_changed = m.contact != s.contact;
                    s.quiet_told = false;
                    s.contact = m.contact;
                    // A strap that hangs up soon after each subscription is still lost.
                    if s.live_at.is_some_and(|at| at.elapsed() >= STALE) {
                        s.lost_since = None;
                    }
                    reading(&m);
                    if first || contact_changed {
                        uart.write_str("[hr] ");
                        uart.write_u32(u32::from(m.bpm));
                        uart.write_str(" bpm, ");
                        uart.write_str(match m.contact {
                            Contact::Unsupported => "contact not reported",
                            Contact::Lost => "NO skin contact",
                            Contact::Detected => "skin contact",
                        });
                        uart.write_str(", RR intervals ");
                        uart.write_u32(u32::try_from(m.rr_len).unwrap_or(0));
                        uart.write_line(if first { " (readings arriving; `hr` shows the latest)" } else { "" });
                    }
                }
                return;
            }
        }
        let now = Instant::now();
        if self.printed_at.is_some_and(|at| now < at + PRINT_PACE) {
            UNPRINTED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.printed_at = Some(now);
        uart.write_str("[ble] notify ");
        write_handle(uart, attr);
        uart.write_str(": ");
        write_hex_bytes(uart, value);
        uart.write_line("");
    }

    fn proc_complete(&mut self, error: u8, uart: Uart) {
        let Some((proc, _)) = self.proc.take() else {
            return;
        };
        let ok = error == 0;
        match proc {
            Proc::Services | Proc::Chars | Proc::Descs | Proc::Read => {
                uart.write_line(if ok { "[ble] done" } else { "[ble] procedure ended with an error" });
            }
            Proc::Write | Proc::Subscribe => uart.write_line(if ok { "[ble] written" } else { "[ble] write refused" }),
            Proc::StrapService | Proc::StrapChars | Proc::StrapDescs | Proc::StrapSubscribe => {
                self.strap_step_done(proc, ok, uart);
            }
        }
    }

    fn strap_step_done(&mut self, proc: Proc, ok: bool, uart: Uart) {
        let Some(s) = self.strap.as_mut() else {
            return;
        };
        let next = match proc {
            Proc::StrapService if s.range.is_some() => Some(Stage::Chars),
            Proc::StrapChars if s.measurement.is_some() => Some(Stage::Descs),
            Proc::StrapDescs if s.cccd.is_some() => Some(Stage::Subscribe),
            Proc::StrapSubscribe if ok => Some(Stage::Live),
            _ => None,
        };
        match next {
            Some(Stage::Live) => {
                s.stage = Stage::Live;
                s.quiet_told = false;
                s.announced = false;
                s.live_at = Some(Instant::now());
                HR_AT_MS.store(now_ms(), Ordering::Relaxed);
                HR.store(HR_LIVE, Ordering::Relaxed);
                uart.write_line("[hr] subscribed to the strap's heart rate");
            }
            Some(stage) => s.stage = stage,
            None => {
                uart.write_line(match proc {
                    Proc::StrapService => "[hr] the device has no Heart Rate Service; `ble drop`",
                    Proc::StrapChars => "[hr] no Heart Rate Measurement in the service; `ble drop`",
                    Proc::StrapDescs => "[hr] the measurement has no configuration descriptor; `ble drop`",
                    _ => "[hr] the strap refused the subscription; `ble drop`",
                });
                // Left connected and stuck rather than retried for ever: the device is not a strap.
                HR.store(HR_NOT_STRAP, Ordering::Relaxed);
                self.strap = None;
            }
        }
    }
}

fn connect_timeout() -> Duration {
    Duration::from_millis(connect_timeout_ms(power()))
}

/// Counts and the last reading start again with each `hr strap` or `hr fake`.
fn new_source() {
    HR_READINGS.store(0, Ordering::Relaxed);
    HR_AT_MS.store(0, Ordering::Relaxed);
}

/// A heart-rate reading, from the strap or `hr fake`, into the statics `hr` reports.
fn reading(m: &Measurement) {
    HR_BPM.store(m.bpm, Ordering::Relaxed);
    HR_CONTACT.store(
        match m.contact {
            Contact::Unsupported => 0,
            Contact::Lost => 1,
            Contact::Detected => 2,
        },
        Ordering::Relaxed,
    );
    HR_RR_MS.store(m.rr().last().map_or(0, |&rr| rr_ms(rr)), Ordering::Relaxed);
    HR_AT_MS.store(now_ms().max(1), Ordering::Relaxed);
    HR_READINGS.fetch_add(1, Ordering::Relaxed);
}

impl Link<'_> {
    fn sensor_handle_bytes(&self) -> Option<[u8; 2]> {
        self.central.handle().map(u16::to_le_bytes)
    }

    /// A console request for the central side.
    pub(super) async fn central_request(&mut self, ask: Ask) {
        let uart = self.uart;
        if !self.central.available {
            return uart.write_line("[ble] the central role is not available on this chip (see the start-up log)");
        }
        match ask {
            Ask::Connect(a) => self.start_connect(a).await,
            Ask::Drop => {
                let peer = self.central.link.map(|(_, a)| a).or(self.central.connecting.map(|(a, _)| a));
                if self.central.strap.as_ref().is_some_and(|s| Some(s.address) == peer) {
                    self.central.strap = None;
                    HR.store(HR_OFF, Ordering::Relaxed);
                }
                self.drop_sensor().await;
            }
            Ask::Fake(bpm) => {
                self.drop_strap().await;
                self.central.fake = Some((bpm, Instant::now()));
                new_source();
                HR.store(HR_FAKE, Ordering::Relaxed);
                uart.write_line("[hr] fake readings once a second; `hr off` stops them");
            }
            Ask::HrOff => {
                self.central.fake = None;
                self.drop_strap().await;
                HR.store(HR_OFF, Ordering::Relaxed);
                uart.write_line("[hr] off");
            }
            Ask::Strap(a) => {
                if self.central.link.is_some_and(|(_, peer)| peer != a) || self.central.connecting.is_some() {
                    return uart.write_line("[hr] the sensor link is busy; `ble drop` first");
                }
                self.central.fake = None;
                let connected = self.central.link.is_some();
                self.central.strap = Some(Strap {
                    address: a,
                    stage: if connected { Stage::Service } else { Stage::Connect },
                    range: None,
                    measurement: None,
                    cccd: None,
                    after_measurement: false,
                    retry_at: Instant::now(),
                    lost_since: None,
                    live_at: None,
                    quiet_told: false,
                    announced: false,
                    contact: Contact::Unsupported,
                });
                new_source();
                HR.store(if connected { HR_DISCOVERING } else { HR_CONNECTING }, Ordering::Relaxed);
            }
            client => self.client_request(client).await,
        }
    }

    /// The strap given up for another source: its own link or attempt ends with it, no other.
    async fn drop_strap(&mut self) {
        let Some(a) = self.central.strap.take().map(|s| s.address) else {
            return;
        };
        if self.central.link.is_some_and(|(_, peer)| peer == a) || self.central.connecting.is_some_and(|(peer, _)| peer == a) {
            self.drop_sensor().await;
        }
    }

    /// `ble services|chars|descs|read|write|subscribe` on the sensor link.
    async fn client_request(&mut self, client: Ask) {
        let uart = self.uart;
        let Some(h) = self.sensor_handle_bytes() else {
            return uart.write_line("[ble] no sensor link; `ble connect <addr>` first");
        };
        if self.central.proc.is_some() {
            return uart.write_line("[ble] a GATT procedure is still running; one at a time");
        }
        let mut p = [0u8; 4 + 1 + WRITE_MAX];
        p[0..2].copy_from_slice(&h);
        let (opcode, proc, n) = match client {
            Ask::Services => (ACI_GATT_DISC_ALL_PRIMARY_SERVICES, Proc::Services, 2),
            Ask::Chars(s, e) | Ask::Descs(s, e) => {
                p[2..4].copy_from_slice(&s.to_le_bytes());
                p[4..6].copy_from_slice(&e.to_le_bytes());
                if matches!(client, Ask::Chars(..)) {
                    (ACI_GATT_DISC_ALL_CHAR_OF_SERVICE, Proc::Chars, 6)
                } else {
                    (ACI_GATT_DISC_ALL_CHAR_DESC, Proc::Descs, 6)
                }
            }
            Ask::Read(a) => {
                p[2..4].copy_from_slice(&a.to_le_bytes());
                (ACI_GATT_READ_CHAR_VALUE, Proc::Read, 4)
            }
            Ask::Write(a, value, len) => {
                let len = usize::from(len);
                p[2..4].copy_from_slice(&a.to_le_bytes());
                p[4] = u8::try_from(len).unwrap_or(0);
                p[5..5 + len].copy_from_slice(&value[..len]);
                (ACI_GATT_WRITE_CHAR_VALUE, Proc::Write, 5 + len)
            }
            Ask::Subscribe(cccd, value) => {
                p[2..4].copy_from_slice(&cccd.to_le_bytes());
                p[4] = 2;
                p[5] = value;
                (ACI_GATT_WRITE_CHAR_DESC, Proc::Subscribe, 7)
            }
            _ => return,
        };
        self.start_proc(opcode, &p[..n], proc).await;
    }

    async fn start_proc(&mut self, opcode: u16, params: &[u8], proc: Proc) {
        match self.command(opcode, params).await {
            Ok(r) if r.status == 0 => self.central.proc = Some((proc, Instant::now())),
            Ok(r) => {
                self.uart.write_str("[ble] GATT procedure refused, status ");
                self.uart.write_hex8(r.status);
                self.uart.write_line("");
                // A strap step refused is held as running, so its timeout drops the link and
                // the strap is set up again from a fresh connection.
                if matches!(proc, Proc::StrapService | Proc::StrapChars | Proc::StrapDescs | Proc::StrapSubscribe) {
                    self.central.proc = Some((proc, Instant::now()));
                }
            }
            Err(why) => {
                self.uart.write_str("[ble] GATT procedure: ");
                self.uart.write_line(why);
            }
        }
    }

    async fn start_connect(&mut self, a: Address) {
        if self.central.link.is_some() {
            return self.uart.write_line("[ble] the sensor link is up; `ble drop` first");
        }
        if self.scanner.until().is_some() || self.central.connecting.is_some() {
            return self.uart.write_line("[ble] a scan or a connection attempt is already running");
        }
        let s = scan(power());
        let t = sensor(power());
        let mut p = [0u8; 24];
        p[0..2].copy_from_slice(&s.interval.to_le_bytes());
        p[2..4].copy_from_slice(&s.window.to_le_bytes());
        p[4] = a.kind;
        p[5..11].copy_from_slice(&a.bytes);
        p[11] = address::STATIC_RANDOM;
        p[12..14].copy_from_slice(&t.interval_min.to_le_bytes());
        p[14..16].copy_from_slice(&t.interval_max.to_le_bytes());
        p[16..18].copy_from_slice(&t.latency.to_le_bytes());
        p[18..20].copy_from_slice(&t.timeout.to_le_bytes());
        // Connection event length: no preference.
        p[20..24].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        match self.while_advertising(ACI_GAP_CREATE_CONNECTION, &p).await {
            Ok(0) => {
                self.central.connecting = Some((a, Instant::now()));
                self.central.asked = Some(t);
                Central::set_peer(a);
                CENTRAL.store(CENTRAL_CONNECTING, Ordering::Relaxed);
                self.uart.write_str("[ble] connecting to ");
                write_address(self.uart, a);
                self.uart.write_str(" (");
                self.uart.write_u32(u32::try_from(connect_timeout().as_secs()).unwrap_or(0));
                self.uart.write_line(" s)");
            }
            Ok(status) => {
                self.uart.write_str("[ble] connection refused, status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
                self.resume_advertising();
                self.central.strap_retry();
            }
            Err(why) => {
                self.uart.write_str("[ble] connect: ");
                self.uart.write_line(why);
                self.central.strap_retry();
            }
        }
    }

    /// The sensor link's interval changed: advertising that no longer fits beside it is chosen
    /// again. An update has been read as 0 (HARDWARE.md); the interval asked for is taken then.
    pub(super) fn sensor_interval(&mut self, reported: u16) {
        let requested = self.central.requested.take();
        let iv = if reported == 0 { requested.unwrap_or(0) } else { reported };
        let state = STATE.load(Ordering::Relaxed);
        if iv != 0 {
            self.central.set_interval(iv);
        }
        if readvertise_for((state == STATE_ADVERTISING).then_some(self.advertised_every), iv) {
            self.advertised = None;
            self.readvertise |= state == STATE_IDLE;
        }
        self.uart.write_str("[ble] sensor link interval ");
        if iv == 0 {
            return self.uart.write_line("reported as 0: kept as it was");
        }
        super::write_interval(self.uart, iv);
        self.uart.write_line(if reported == 0 { " (reported as 0: the one asked for)" } else { "" });
    }

    /// The sensor link went down: advertising fitted to it goes back to the timing wanted.
    pub(super) fn sensor_disconnected(&mut self, reason: u8) {
        self.central.disconnected(reason, self.uart);
        if self.advertised.is_some_and(|t| t.advertise != self.advertised_every) {
            self.advertised = None;
        }
        // Up, not connected and not advertising: nothing fitted beside the link; try again.
        self.readvertise |= STATE.load(Ordering::Relaxed) == STATE_IDLE;
    }

    /// The sensor link closed, or the attempt to make it cancelled.
    pub(super) async fn drop_sensor(&mut self) {
        if let Some(h) = self.sensor_handle_bytes() {
            // Reason 0x13: remote user terminated connection.
            let _ = self.command(ACI_GAP_TERMINATE, &[h[0], h[1], 0x13]).await;
        } else if self.central.connecting.take().is_some() {
            let _ = self.command(ACI_GAP_TERMINATE_GAP_PROC, &[gap_proc::DIRECT_CONNECTION]).await;
            // The connection may have completed while the procedure ended.
            if let Some(h) = self.sensor_handle_bytes() {
                let _ = self.command(ACI_GAP_TERMINATE, &[h[0], h[1], 0x13]).await;
                return;
            }
            CENTRAL.store(CENTRAL_IDLE, Ordering::Relaxed);
            self.resume_advertising();
            self.uart.write_line("[ble] connection attempt cancelled");
        } else {
            self.uart.write_line("[ble] no sensor link");
        }
    }

    /// The central's work for this pass.
    pub(super) async fn central_tick(&mut self) {
        let now = Instant::now();
        let uart = self.uart;
        if self.central.connecting.is_some_and(|(_, since)| now >= since + connect_timeout()) {
            let _ = self.command(ACI_GAP_TERMINATE_GAP_PROC, &[gap_proc::DIRECT_CONNECTION]).await;
            if self.central.connecting.take().is_some() {
                CENTRAL.store(CENTRAL_IDLE, Ordering::Relaxed);
                uart.write_line("[ble] connection attempt: not found in time");
                self.central.strap_retry();
            }
        }
        if let Some((proc, since)) = self.central.proc {
            let limit = self.central.procedure_timeout();
            if now >= since + limit {
                self.central.proc = None;
                uart.write_str("[ble] the sensor did not finish a GATT procedure in ");
                uart.write_u32(u32::try_from(limit.as_secs()).unwrap_or(u32::MAX));
                uart.write_line(" s");
                if matches!(proc, Proc::StrapService | Proc::StrapChars | Proc::StrapDescs | Proc::StrapSubscribe) {
                    // Start over from a fresh connection.
                    self.drop_sensor().await;
                }
            }
        }
        if core::mem::take(&mut self.central.confirm) {
            if let Some(h) = self.sensor_handle_bytes() {
                let _ = self.command(ACI_GATT_CONFIRM_INDICATION, &h).await;
            }
        }
        if let Some(r) = self.central.update_request.take() {
            self.answer_update_request(r).await;
        }
        self.strap_tick(now).await;
        if let Some((bpm, at)) = self.central.fake {
            if now >= at {
                self.central.fake = Some((bpm, at.max(now - FAKE_EVERY) + FAKE_EVERY));
                reading(&Measurement::rate(bpm));
            }
        }
        self.retime_sensor().await;
    }

    async fn strap_tick(&mut self, now: Instant) {
        let Some(s) = self.central.strap.as_mut() else {
            return;
        };
        let uart = self.uart;
        if s.stage == Stage::Live {
            let age = u64::from(now_ms().wrapping_sub(HR_AT_MS.load(Ordering::Relaxed)));
            if !s.quiet_told && age > QUIET.as_millis() {
                s.quiet_told = true;
                uart.write_line("[hr] quiet: no reading for 5 s (strap off, or out of range)");
            } else if s.quiet_told && age > STALE.as_millis() && self.central.proc.is_none() {
                uart.write_line("[hr] no reading for 30 s on a live link: dropping it to look for the strap again");
                s.stage = Stage::Connect;
                return self.drop_sensor().await;
            }
        }
        if s.stage == Stage::Connect {
            // A link made beside advertising keeps it; advertising started beside a link may be refused.
            if self.central.link.is_some() || self.central.connecting.is_some() || self.scanner.until().is_some() || self.readvertise || now < s.retry_at {
                return;
            }
            if s.lost_since.is_some_and(|since| now >= since + Duration::from_millis(GIVE_UP_MS)) {
                self.central.strap = None;
                HR.store(HR_GAVE_UP, Ordering::Relaxed);
                return uart.write_line("[hr] strap not found for 5 minutes: gave up");
            }
            let a = s.address;
            HR.store(if s.lost_since.is_some() { HR_LOST } else { HR_CONNECTING }, Ordering::Relaxed);
            return self.start_connect(a).await;
        }
        if self.central.proc.is_some() || s.stage == Stage::Live {
            return;
        }
        let Some(h) = self.central.link.map(|(h, _)| h.to_le_bytes()) else {
            return;
        };
        let mut p = [0u8; 7];
        p[0..2].copy_from_slice(&h);
        let (opcode, proc, n) = match s.stage {
            Stage::Service => {
                let u = heart_rate::SERVICE.to_le_bytes();
                p[2] = 0x01;
                p[3..5].copy_from_slice(&u);
                (ACI_GATT_DISC_PRIMARY_SERVICE_BY_UUID, Proc::StrapService, 5)
            }
            Stage::Chars => {
                let (start, end) = s.range.unwrap_or((1, 0xFFFF));
                p[2..4].copy_from_slice(&start.to_le_bytes());
                p[4..6].copy_from_slice(&end.to_le_bytes());
                (ACI_GATT_DISC_ALL_CHAR_OF_SERVICE, Proc::StrapChars, 6)
            }
            Stage::Descs => {
                let value = s.measurement.unwrap_or(1);
                let end = s.range.map_or(0xFFFF, |(_, e)| e);
                s.after_measurement = false;
                p[2..4].copy_from_slice(&value.to_le_bytes());
                p[4..6].copy_from_slice(&end.to_le_bytes());
                (ACI_GATT_DISC_ALL_CHAR_DESC, Proc::StrapDescs, 6)
            }
            Stage::Subscribe => {
                p[2..4].copy_from_slice(&s.cccd.unwrap_or(0).to_le_bytes());
                p[4] = 2;
                p[5] = 0x01;
                p[6] = 0x00;
                (ACI_GATT_WRITE_CHAR_DESC, Proc::StrapSubscribe, 7)
            }
            Stage::Connect | Stage::Live => return,
        };
        self.start_proc(opcode, &p[..n], proc).await;
    }

    /// As the central, a peripheral's request for new parameters is refused: the chip refuses
    /// an acceptance too (HARDWARE.md), and the watch's own timing keeps advertising fitting.
    async fn answer_update_request(&mut self, r: L2capUpdateRequest) {
        let status = self.command(ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_RESP, &r.refusal()).await.map_or(0xFF, |r| r.status);
        self.uart.write_str("[ble] the sensor asked for ");
        super::write_interval(self.uart, r.interval_min);
        self.uart.write_str(" to ");
        super::write_interval(self.uart, r.interval_max);
        if status != 0 {
            self.uart.write_str(": the refusal failed, status ");
            self.uart.write_hex8(status);
            return self.uart.write_line("");
        }
        self.uart.write_line(": refused");
    }

    /// The sensor link asked for the power source's timing when that changes.
    async fn retime_sensor(&mut self) {
        let Some(h) = self.central.handle() else {
            return;
        };
        let want = sensor(power());
        if self.central.asked == Some(want) {
            self.central.retime_at = None;
            return;
        }
        if self.central.proc.is_some() || self.central.retime_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let mut p = [0u8; 14];
        p[0..2].copy_from_slice(&h.to_le_bytes());
        p[2..4].copy_from_slice(&want.interval_min.to_le_bytes());
        p[4..6].copy_from_slice(&want.interval_max.to_le_bytes());
        p[6..8].copy_from_slice(&want.latency.to_le_bytes());
        p[8..10].copy_from_slice(&want.timeout.to_le_bytes());
        let status = self.command(ACI_GAP_START_CONNECTION_UPDATE, &p).await.map_or(0xFF, |r| r.status);
        if status == 0 {
            self.central.asked = Some(want);
            self.central.retime_at = None;
            self.central.requested = Some(want.interval_min);
            return;
        }
        self.central.retime_at = Some(Instant::now() + RETIME_AGAIN);
        self.uart.write_str("[ble] the chip refused to re-time the sensor link, status ");
        self.uart.write_hex8(status);
        self.uart.write_line("; trying again in 5 s");
    }
}
