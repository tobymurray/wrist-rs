//! A capture of the watch's own driver (`ble-trace`) replayed through the core: the core sees the
//! chip's real events, its operations are taken as the driver's, and its choices and status are
//! set beside what the driver did.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use bluenrg2::opcode;
use wrist_ble::{ConnHandle, Instant, LeConnRole, LinkStatus, Op, OpId, OpKind, Outcome, RadioEvent, Snapshot, MAX_LINKS};
use wrist_ble_core::engine::{Core, Input, Outputs};
use wrist_ble_core::goals::{Goals, PhoneGoal, Power};
use wrist_ble_core::status;
use wrist_bluenrg2::events::Translator;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

mod agree;

use crate::fake::INFO;
use crate::menu::{interval_now, MENU};

/// One line of the replay's timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub at: u64,
    pub who: Who,
    pub what: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Who {
    /// A command the driver sent.
    Driver,
    /// An operation the core asked for.
    Core,
    /// The core's status changed.
    Status,
    /// What the core told the user or the log.
    Tell,
    /// An event from the chip, as the core received it.
    Event,
    /// A line the driver printed.
    Line,
}

#[derive(Debug, Default)]
pub struct Report {
    pub timeline: Vec<Entry>,
    /// How many times each kind of operation was done by the driver and asked for by the core.
    pub tally: BTreeMap<String, (u32, u32)>,
    pub unknown_events: u32,
    /// Events the trace kept too little of to decode.
    pub cut_by_the_trace: u32,
    /// Times the watch reset, each the start of a new epoch.
    pub resets: u32,
    /// Where a capture began with the chip already up, and the catch-up operations, not compared.
    pub seeded: Option<(u64, Vec<Op>)>,
    pub core_ops: Vec<(u64, Op)>,
    pub driver_ops: Vec<(u64, DriverOp)>,
    /// When the operator told the driver to drop the phone's link (`ble disconnect`), which the
    /// core has no goal for.
    pub operator_disconnects: Vec<u64>,
}

/// A driver command that is one of the core's operations, with what the comparison reads of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverOp {
    /// Interval min and max, in 0.625 ms.
    Advertise(u16, u16),
    StopAdvertising,
    /// Interval min and max in 1.25 ms, and latency.
    AskParams(u16, u16, u16),
    /// The answer to a numeric comparison.
    Confirm(bool),
    Other(OpKind),
}


impl Report {
    #[must_use]
    pub fn render(&self, events: bool) -> String {
        let mut s = String::new();
        for e in self.timeline.iter().filter(|e| events || e.who != Who::Event) {
            let tag = match e.who {
                Who::Driver => "driver",
                Who::Core => "core  ",
                Who::Status => "status",
                Who::Tell => "tell  ",
                Who::Event => "event ",
                Who::Line => "      ",
            };
            let _ = writeln!(s, "{:>9} {tag} {}", e.at, e.what);
        }
        let _ = writeln!(s, "\noperation            driver  core");
        for (k, (d, c)) in &self.tally {
            let mark = if d == c { "" } else { "  <- differs" };
            let _ = writeln!(s, "{k:<20} {d:>6} {c:>5}{mark}");
        }
        let _ = writeln!(s, "unknown events: {}, cut by the trace: {}, watch resets: {}", self.unknown_events, self.cut_by_the_trace, self.resets);
        if let Some((at, ops)) = &self.seeded {
            let _ = writeln!(s, "the chip already up at {at}; not compared, the core's catching up: {ops:?}");
        }
        let d = self.disagreements();
        let _ = writeln!(s, "disagreements: {}", d.len());
        for line in d {
            let _ = writeln!(s, "  {line}");
        }
        s
    }
}

/// The chip's report that the central accepted the watch's parameter request.
fn accepted_update(packet: &[u8]) -> bool {
    matches!(bluenrg2::parse(packet), bluenrg2::Event::Vendor(bluenrg2::VendorEvent::L2capUpdateResponse { accepted: true, .. }))
}

/// The driver's command as the operation it carries out, for the commands that are one.
fn driver_op(packet: &[u8]) -> Option<(OpKind, &'static str)> {
    let code = u16::from_le_bytes([*packet.get(1)?, *packet.get(2)?]);
    Some(match code {
        opcode::ACI_GAP_SET_DISCOVERABLE => (OpKind::Advertise, "advertise"),
        opcode::ACI_GAP_SET_NON_DISCOVERABLE => (OpKind::Advertise, "stop advertising"),
        opcode::ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ => (OpKind::UpdateParams, "ask the central for parameters"),
        opcode::ACI_GAP_SLAVE_SECURITY_REQ => (OpKind::RequestSecurity, "ask the central to pair"),
        opcode::ACI_GAP_ALLOW_REBOND => (OpKind::AllowRebond, "allow a rebond"),
        opcode::ACI_GAP_NUMERIC_COMPARISON_CONFIRM => (OpKind::ConfirmNumeric, "answer the comparison"),
        opcode::ACI_GAP_PASS_KEY_RESP => (OpKind::Passkey, "answer with a passkey"),
        opcode::ACI_GAP_TERMINATE => (OpKind::Disconnect, "disconnect"),
        opcode::ACI_GAP_START_GENERAL_DISCOVERY_PROC => (OpKind::Scan, "scan"),
        opcode::ACI_GAP_TERMINATE_GAP_PROC => (OpKind::Scan, "end the scan"),
        _ => return None,
    })
}

/// The goals the watch boots with: the radio on, the phone welcome, and `ble profile auto` on
/// the console's cable, which is cable timing.
fn boot_goals() -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF }
}

struct Replay<'a> {
    core: Core<'a>,
    translator: Translator,
    goals: Goals,
    now: Instant,
    next: Option<Instant>,
    /// Where this epoch's milliseconds start on the timeline: 0 until the watch first resets,
    /// then unknown until the epoch's first trace line.
    base: Option<u64>,
    /// The chip's last milliseconds in this epoch.
    last_ms: Option<u64>,
    ids: u16,
    phone: Option<ConnHandle>,
    /// The core's parameter request on air, which the central's answer ends.
    asked_update: Option<OpId>,
    advertising: bool,
    /// The core is being told of a chip the capture began with already up.
    seeding: bool,
    last_status: String,
    report: Report,
}

impl Replay<'_> {
    fn push(&mut self, who: Who, what: String) {
        self.report.timeline.push(Entry { at: self.now.0, who, what });
    }

    /// Ticks owed before `at`, then the clock at `at`.
    fn advance(&mut self, at: Instant) {
        while let Some(n) = self.next.filter(|&n| n <= at) {
            self.now = Instant(self.now.0.max(n.0));
            self.next = None;
            self.feed(Input::Tick);
        }
        self.now = Instant(self.now.0.max(at.0));
    }

    fn feed(&mut self, input: Input) {
        let out = self.core.step(self.now, input);
        self.next = out.next;
        self.act(&out);
        self.status();
    }

    fn act(&mut self, out: &Outputs) {
        for t in out.tell.iter().flatten() {
            self.push(Who::Tell, format!("{t:?}"));
        }
        if let Some(a) = out.chip {
            self.push(Who::Core, format!("chip {a:?}"));
        }
        if let Some(op) = out.op {
            self.push(Who::Core, format!("{op:?}"));
            match &mut self.report.seeded {
                Some((_, ops)) if self.seeding => ops.push(op),
                _ => {
                    self.report.core_ops.push((self.now.0, op));
                    self.report.tally.entry(format!("{:?}", op.kind())).or_default().1 += 1;
                }
            }
            self.ids = self.ids.wrapping_add(1);
            let id = OpId(self.ids);
            if op.kind() == OpKind::UpdateParams {
                self.asked_update = Some(id);
            }
            self.feed(Input::Submitted(Ok(id)));
            if !op.kind().ends_with_event() {
                self.feed(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
            }
        }
        if out.snapshot {
            let mut links = [None; MAX_LINKS];
            if let Some(link) = self.phone {
                links[0] = Some(LinkStatus { link, role: LeConnRole::Peripheral, peer: wrist_ble::PeerAddr { kind: wrist_ble::AddrKind::RANDOM, addr: wrist_ble::BdAddr::new([0; 6]) } });
            }
            let s = Snapshot { at: self.now, links, advertising: self.advertising, scanning: false, initiating: false, anchor: None };
            self.push(Who::Core, "snapshot".into());
            self.feed(Input::Snapshot(Ok(s)));
        }
    }

    fn status(&mut self) {
        let mut line = String::new();
        let _ = status::status(&self.core.state, self.now).write_line(&mut line);
        let phone = line.split_whitespace().find_map(|f| f.strip_prefix("phone=")).unwrap_or_default().to_string();
        if phone != self.last_status {
            self.push(Who::Status, phone.clone());
            self.last_status = phone;
        }
    }

    fn goals(&mut self) {
        let g = self.goals;
        self.feed(Input::Goals(g));
    }

    /// The chip's ready answers the core's start at once, for a capture that never shows it.
    fn seed(&mut self) {
        let Some(id) = self.core.state.chip.starting() else { return };
        self.push(Who::Line, "-- the chip was already up".into());
        self.report.seeded = Some((self.now.0, Vec::new()));
        self.seeding = true;
        self.feed(Input::Started(id, Ok(INFO)));
        self.seeding = false;
    }

    /// Whether the watch has reset since this epoch's last trace line: its milliseconds, or the
    /// uptime a heartbeat or `uptime` reports, went backwards.
    fn reset_seen(&self, traced: Option<u64>, uptime: Option<u64>) -> bool {
        let Some(last) = self.last_ms else { return false };
        match (traced, uptime) {
            (Some(ms), _) => ms < last,
            // Whole seconds, read a little after the line before was traced.
            (None, Some(ms)) => ms + 2_000 < last,
            (None, None) => false,
        }
    }

    /// A new epoch from `at`: the core and its chip start again as at the start, with the goals
    /// the watch boots with.
    fn reset(&mut self, at: u64) {
        self.now = Instant(self.now.0.max(at));
        self.base = None;
        self.last_ms = None;
        self.core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
        self.translator = Translator::new(INFO);
        self.goals = boot_goals();
        self.next = None;
        self.phone = None;
        self.asked_update = None;
        self.advertising = false;
        self.report.resets += 1;
        self.push(Who::Line, "-- the watch reset".into());
        self.goals();
    }

    fn line(&mut self, text: &str) {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("[trace] ") {
            self.trace(rest);
        } else if let Some(cmd) = text.strip_prefix(">> usb ") {
            match cmd.trim() {
                "ble off" => {
                    self.goals.radio_on = false;
                    self.goals();
                }
                "ble on" => {
                    self.goals.radio_on = true;
                    self.goals();
                }
                "ble profile battery" => {
                    self.goals.phone_params = None;
                    self.goals.power = Power::Battery;
                    self.goals();
                }
                "ble profile cable" | "ble profile auto" => {
                    self.goals.phone_params = None;
                    self.goals.power = Power::Cable;
                    self.goals();
                }
                "ble disconnect" => self.report.operator_disconnects.push(self.now.0),
                c if c.starts_with("ble interval now ") => {
                    let mut units = c[17..].split_whitespace().filter_map(|w| w.parse::<u16>().ok());
                    if let (Some(min), Some(max)) = (units.next(), units.next()) {
                        self.goals.phone_params = Some(interval_now(min, max));
                        self.goals();
                    }
                }
                "ble yes" => self.feed(Input::Answer(true)),
                "ble no" => self.feed(Input::Answer(false)),
                _ => {}
            }
            self.push(Who::Line, format!(">> {cmd}"));
        } else if text.starts_with("[ble]") || text.starts_with("== ") {
            self.push(Who::Line, text.to_string());
        }
    }

    fn trace(&mut self, rest: &str) {
        let mut f = rest.split_whitespace();
        let (Some(ms), Some(dir)) = (f.next().and_then(|m| m.parse::<u64>().ok()), f.next()) else { return };
        let bytes: Vec<u8> = f
            .next()
            .filter(|h| !h.starts_with('+'))
            .map(|h| (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok()).collect())
            .unwrap_or_default();
        let cut = rest.contains(" +");
        self.last_ms = Some(ms);
        self.advance(Instant(self.base.unwrap_or_default() + ms));
        match dir {
            "r" => self.push(Who::Driver, "reset the chip".into()),
            "c" => {
                if let Some((kind, what)) = driver_op(&bytes) {
                    let at = |i: usize| bytes.get(i..i + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]));
                    let op = match (kind, what) {
                        // Advertising type, then interval min and max (0.625 ms).
                        (OpKind::Advertise, "advertise") => DriverOp::Advertise(at(5), at(7)),
                        (OpKind::Advertise, _) => DriverOp::StopAdvertising,
                        // Handle, then interval min and max (1.25 ms), latency, timeout (10 ms).
                        (OpKind::UpdateParams, _) => DriverOp::AskParams(at(6), at(8), at(10)),
                        // Handle, then 1 to confirm.
                        (OpKind::ConfirmNumeric, _) => DriverOp::Confirm(bytes.get(6) == Some(&1)),
                        _ => DriverOp::Other(kind),
                    };
                    let detail = match op {
                        DriverOp::Advertise(min, max) => format!(" {min}-{max} units"),
                        DriverOp::AskParams(min, max, latency) => format!(" {min}-{max} units, latency {latency}, timeout {}", at(12)),
                        _ => String::new(),
                    };
                    // An ask the menu never makes is the console's `ble interval now`, over USB or
                    // over the air (which the trace does not show): the core is given it as a goal.
                    if let DriverOp::AskParams(min, max, latency) = op {
                        let menu = MENU.phone.iter().any(|c| (c.params.interval_min.as_u16(), c.params.interval_max.as_u16(), c.params.latency) == (min, max, latency));
                        let now = interval_now(min, max);
                        if !menu && self.goals.phone_params != Some(now) {
                            self.goals.phone_params = Some(now);
                            self.goals();
                        }
                    }
                    self.report.driver_ops.push((self.now.0, op));
                    self.push(Who::Driver, format!("{what}{detail}"));
                    self.report.tally.entry(format!("{kind:?}")).or_default().0 += 1;
                    match what {
                        "advertise" => self.advertising = true,
                        "stop advertising" => self.advertising = false,
                        _ => {}
                    }
                }
            }
            "e" => {
                if cut && matches!(bluenrg2::parse(&bytes), bluenrg2::Event::Malformed(_)) {
                    self.report.cut_by_the_trace += 1;
                    return;
                }
                if accepted_update(&bytes) {
                    if let Some(id) = self.asked_update.take() {
                        self.feed(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
                    }
                }
                if let Some(e) = self.translator.event(&bytes) {
                    match e {
                        RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, .. } => {
                            self.phone = Some(link);
                            self.advertising = false;
                        }
                        RadioEvent::LinkDown { link, .. } if self.phone == Some(link) => self.phone = None,
                        RadioEvent::Unknown { .. } => self.report.unknown_events += 1,
                        _ => {}
                    }
                    self.push(Who::Event, format!("{e:?}"));
                    // The driver's reset ends in the chip's ready, which answers the core's start
                    // as `Radio::start` would.
                    match (e, self.core.state.chip.starting()) {
                        (RadioEvent::Ready(info), Some(id)) => self.feed(Input::Started(id, Ok(info))),
                        _ => self.feed(Input::Radio(e)),
                    }
                    // The driver's `ble interval now` is for its link only, once the core has seen it end.
                    if matches!(e, RadioEvent::LinkDown { .. }) && self.phone.is_none() && self.goals.phone_params.take().is_some() {
                        self.goals();
                    }
                }
            }
            _ => {}
        }
    }
}

/// The uptime a heartbeat (`[hb] <n>  uptime <s> s ...`) or `uptime` reports, in milliseconds.
fn uptime_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    if !(text.starts_with("[hb] ") || text.starts_with("[usb] uptime ")) {
        return None;
    }
    let (_, rest) = text.split_once(" uptime ")?;
    let (s, _) = rest.split_once(" s")?;
    s.parse::<u64>().ok().map(|s| s * 1_000)
}

/// Replays lines of `<host seconds> <console line>`, as `tools/ble-trace-capture.py` writes them.
/// A watch reset starts the core again, and the host's clock carries the timeline across it.
#[must_use]
pub fn replay(capture: &str) -> Report {
    let mut r = Replay {
        core: Core::new(&CAPABILITIES, &Scheduler, &MENU),
        translator: Translator::new(INFO),
        goals: boot_goals(),
        now: Instant(0),
        next: None,
        base: Some(0),
        last_ms: None,
        ids: 0,
        phone: None,
        asked_update: None,
        advertising: false,
        seeding: false,
        last_status: String::new(),
        report: Report::default(),
    };
    let mut started = false;
    let mut offset: Option<i64> = None;
    for line in capture.lines() {
        let (host, text) = match line.trim_start().split_once(' ') {
            Some((t, rest)) => host_ms(t).map_or((None, line), |t| (Some(t), rest)),
            None => (None, line),
        };
        let traced = text.trim().strip_prefix("[trace] ").and_then(|r| r.split_whitespace().next()).and_then(|m| m.parse::<u64>().ok());
        // This line on the timeline by the host's clock, which runs on through a reset.
        let by_host = host.zip(offset).and_then(|(h, o)| u64::try_from(h + o).ok()).map_or(r.now.0, |t| t.max(r.now.0));
        if r.reset_seen(traced, uptime_ms(text)) {
            r.reset(by_host);
        }
        if let (Some(ms), None) = (traced, r.base) {
            r.base = Some(by_host.saturating_sub(ms));
        }
        match (traced, host) {
            // The watch's clock against the host's, so a host line falls where it happened.
            (Some(ms), Some(h)) => offset = i64::try_from(r.base.unwrap_or_default() + ms).ok().map(|ms| ms - h),
            (None, Some(h)) => {
                if let Some(at) = offset.and_then(|o| u64::try_from(h + o).ok()) {
                    r.advance(Instant(at));
                }
            }
            _ => {}
        }
        if !started {
            if let Some(ms) = traced {
                r.now = Instant(ms);
                r.goals();
                started = true;
                if !comes_up(text) {
                    r.seed();
                }
            }
        }
        r.line(text);
    }
    r.report
}

/// Whether a trace line is the driver resetting the chip or the chip's ready.
fn comes_up(text: &str) -> bool {
    let mut f = text.split_whitespace().skip(2);
    // `ACI_BLUE_INITIALIZED`, started normally.
    matches!((f.next(), f.next()), (Some("r"), _) | (Some("e"), Some("04FF03010001")))
}

/// A line's host time in milliseconds, from seconds with three decimals or a wall clock's `HH:MM:SS.mmm`.
fn host_ms(t: &str) -> Option<i64> {
    let (s, ms) = t.split_once('.')?;
    (ms.len() == 3 && ms.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    let mut seconds = 0i64;
    let mut fields = 0;
    for field in s.split(':') {
        fields += 1;
        let ok = !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit()) && (fields == 1 || field.len() == 2);
        if !ok || fields > 3 {
            return None;
        }
        seconds = seconds * 60 + field.parse::<i64>().ok()?;
    }
    (fields == 1 || fields == 3).then_some(())?;
    Some(seconds * 1_000 + ms.parse::<i64>().ok()?)
}
