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
use crate::menu::MENU;

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
    pub core_ops: Vec<(u64, Op)>,
    pub driver_ops: Vec<(u64, DriverOp)>,
}

/// A driver command that is one of the core's operations, with what the comparison reads of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverOp {
    /// Interval min and max, in 0.625 ms.
    Advertise(u16, u16),
    StopAdvertising,
    /// Interval min and max in 1.25 ms, and latency.
    AskParams(u16, u16, u16),
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
        let _ = writeln!(s, "unknown events: {}, cut by the trace: {}", self.unknown_events, self.cut_by_the_trace);
        let d = self.disagreements();
        let _ = writeln!(s, "disagreements: {}", d.len());
        for line in d {
            let _ = writeln!(s, "  {line}");
        }
        s
    }
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

struct Replay<'a> {
    core: Core<'a>,
    translator: Translator,
    goals: Goals,
    now: Instant,
    next: Option<Instant>,
    ids: u16,
    phone: Option<ConnHandle>,
    advertising: bool,
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
            self.report.core_ops.push((self.now.0, op));
            self.report.tally.entry(format!("{:?}", op.kind())).or_default().1 += 1;
            self.ids = self.ids.wrapping_add(1);
            let id = OpId(self.ids);
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
                    self.goals.power = Power::Battery;
                    self.goals();
                }
                "ble profile cable" => {
                    self.goals.power = Power::Cable;
                    self.goals();
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
        self.advance(Instant(ms));
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
                        _ => DriverOp::Other(kind),
                    };
                    let detail = match op {
                        DriverOp::Advertise(min, max) => format!(" {min}-{max} units"),
                        DriverOp::AskParams(min, max, latency) => format!(" {min}-{max} units, latency {latency}, timeout {}", at(12)),
                        _ => String::new(),
                    };
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
                    self.feed(Input::Radio(e));
                }
            }
            _ => {}
        }
    }
}

/// Replays lines of `<host seconds> <console line>`, as `tools/ble-trace-capture.py` writes them.
#[must_use]
pub fn replay(capture: &str) -> Report {
    let goals = Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF };
    let mut r = Replay {
        core: Core::new(&CAPABILITIES, &Scheduler, &MENU),
        translator: Translator::new(INFO),
        goals,
        now: Instant(0),
        next: None,
        ids: 0,
        phone: None,
        advertising: false,
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
        match (traced, host) {
            // The watch's clock against the host's, so a host line falls where it happened.
            (Some(ms), Some(h)) => offset = i64::try_from(ms).ok().map(|ms| ms - h),
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
            }
        }
        r.line(text);
    }
    r.report
}

/// The capture tool's host time, seconds with three decimals, in milliseconds.
fn host_ms(t: &str) -> Option<i64> {
    let (s, ms) = t.split_once('.')?;
    (ms.len() == 3).then_some(())?;
    Some(s.parse::<i64>().ok()? * 1_000 + ms.parse::<i64>().ok()?)
}
