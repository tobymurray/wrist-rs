//! A capture of the watch's own driver (`ble-trace`) replayed through the core: the core sees the
//! chip's real events, its operations are taken as the driver's, and its choices and status are
//! set beside what the driver did ([`wrist_bluenrg2::shadow`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use wrist_ble::{Fault, Instant, Op, RadioEvent, Snapshot};
use wrist_ble::policy::chip::Action;
use wrist_ble::policy::engine::Tell;
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::{status, State};
use wrist_bluenrg2::shadow::verdict::{Act, Disagreement};
use wrist_bluenrg2::shadow::{record, trace, DriverOp, Excuse, Observer, Shadow};
use wrist_bluenrg2::INFO;

use crate::capture::{self, command, stamped, Command, Profile};

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
    /// Where the core and the driver acted differently.
    pub found: Vec<Disagreement>,
    /// The driver's asks owed nothing, and why.
    pub excused: Vec<(u64, Excuse)>,
    /// Answers the core was owed that the shadow had no room for.
    pub lost: u32,
    /// Snapshots of the driver's the core never asked for, and asks of the core's no snapshot
    /// answered: the capture's mode not the one the replay took ([`Shadow::driven`]).
    pub unasked_snapshots: u32,
    pub unanswered_snapshots: u32,
    /// Records the watch did not keep while a dump read its ring: inputs the core missed there.
    pub not_kept: u32,
    /// Bytes a dump said it had lost to the ring wrapping: the core's inputs are not whole.
    pub dump_lost: u64,
    /// `ble dump` reads seen.
    pub dumps: u32,
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

    /// Where the core and the driver acted differently ([`wrist_bluenrg2::shadow::verdict`]).
    #[must_use]
    pub fn disagreements(&self) -> Vec<String> {
        let mut found = self.found.clone();
        found.sort_by_key(|d| !d.core);
        let mut out: Vec<String> = found.iter().map(ToString::to_string).collect();
        if self.unknown_events > 0 {
            out.push(format!("{} events the core could not read", self.unknown_events));
        }
        if self.unasked_snapshots > 0 {
            out.push(format!("{} snapshots of the driver's the core never asked for", self.unasked_snapshots));
        }
        if self.dump_lost > 0 {
            out.push(format!("{} bytes the watch's dump had lost: the core missed inputs before them", self.dump_lost));
        }
        if self.not_kept > 0 {
            out.push(format!("{} records the watch did not keep while dumping: the core missed inputs there", self.not_kept));
        }
        if self.unanswered_snapshots > 0 {
            out.push(format!("{} snapshots the core asked for and the capture never gave", self.unanswered_snapshots));
        }
        if self.lost > 0 {
            out.push(format!("{} answers the core was owed and never given", self.lost));
        }
        out
    }
}

/// The goals the watch boots with: the radio on, the phone welcome, and `ble profile auto` on
/// the console's cable, which is cable timing.
fn boot_goals() -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF }
}

/// The replay's timeline and tally, kept as the shadow goes.
#[derive(Default)]
struct Record {
    report: Report,
    last_status: String,
}

impl Record {
    fn push(&mut self, at: Instant, who: Who, what: String) {
        self.report.timeline.push(Entry { at: at.0, who, what });
    }
}

impl Observer for Record {
    fn event(&mut self, at: Instant, e: &RadioEvent) {
        if let RadioEvent::Unknown { .. } = e {
            self.report.unknown_events += 1;
        }
        self.push(at, Who::Event, format!("{e:?}"));
    }

    fn driver(&mut self, at: Instant, op: DriverOp, name: &'static str, params: &[u8]) {
        let u16_at = |i: usize| params.get(i..i + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]));
        let detail = match op {
            DriverOp::Advertise(min, max) => format!(" {min}-{max} units"),
            DriverOp::AskParams(min, max, latency) => format!(" {min}-{max} units, latency {latency}, timeout {}", u16_at(8)),
            _ => String::new(),
        };
        self.report.driver_ops.push((at.0, op));
        self.push(at, Who::Driver, format!("{name}{detail}"));
        self.report.tally.entry(format!("{:?}", op.kind())).or_default().0 += 1;
    }

    fn op(&mut self, at: Instant, op: &Op, seeding: bool) {
        self.push(at, Who::Core, format!("{op:?}"));
        match &mut self.report.seeded {
            Some((_, ops)) if seeding => ops.push(*op),
            _ => {
                self.report.core_ops.push((at.0, *op));
                self.report.tally.entry(format!("{:?}", op.kind())).or_default().1 += 1;
            }
        }
    }

    fn chip(&mut self, at: Instant, action: Action) {
        self.push(at, Who::Core, format!("chip {action:?}"));
    }

    fn snapshot(&mut self, s: &Snapshot) {
        self.push(s.at, Who::Core, "snapshot".into());
    }

    fn tell(&mut self, at: Instant, tell: Tell) {
        self.push(at, Who::Tell, format!("{tell:?}"));
    }

    fn stepped(&mut self, at: Instant, state: &State) {
        let mut line = String::new();
        let _ = status::status(state, at).write_line(&mut line);
        let phone = line.split_whitespace().find_map(|f| f.strip_prefix("phone=")).unwrap_or_default().to_string();
        if phone != self.last_status {
            self.push(at, Who::Status, phone.clone());
            self.last_status = phone;
        }
    }

    fn disagreed(&mut self, d: Disagreement) {
        self.report.found.push(d);
    }

    fn lost(&mut self, _at: Instant) {
        self.report.lost += 1;
    }

    fn excused(&mut self, at: Instant, act: Act, why: Excuse) {
        self.report.excused.push((at.0, why));
        self.push(at, Who::Driver, format!("{act} alone: {why:?}"));
    }
}

struct Replay {
    shadow: Shadow<Record>,
    /// Where this epoch's milliseconds start on the timeline: 0 until the watch first resets,
    /// then unknown until the epoch's first trace line.
    base: Option<u64>,
    /// The chip's last milliseconds in this epoch.
    last_ms: Option<u64>,
    /// An image coming over the air, from its `[update] send` line to the line that ends it.
    transfer: bool,
    /// A USB `update` the console took and has not answered, whose image the phone's link does not carry.
    usb_update: Option<UsbUpdate>,
    /// This epoch began with the chip already up, and the core caught up; and its dumps so far.
    caught_up: bool,
    epoch_dumps: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UsbUpdate {
    /// Its length, until its `send` line.
    Asked(u32),
    Sending,
}

/// What `update::begin` and `app_update_allowed` in the firmware refuse a start with, before its
/// `send` line.
const REFUSED_BEFORE_SEND: &[&str] = &[
    "length out of range for this target",
    "a transfer is already in progress",
    "this image is not marked booted yet; wait for [boot] marked, then retry",
    "state partition unreadable",
];

impl Replay {
    fn push(&mut self, who: Who, what: String) {
        let at = self.shadow.now();
        self.shadow.observer.push(at, who, what);
    }

    /// The chip's ready answers the core's start at once, for a capture that never shows it.
    fn seed(&mut self) {
        if self.shadow.state().chip.starting().is_none() {
            return;
        }
        self.push(Who::Line, "-- the chip was already up".into());
        self.shadow.observer.report.seeded = Some((self.shadow.now().0, Vec::new()));
        self.caught_up = true;
        self.shadow.seed();
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

    fn unanswered(&mut self) {
        if self.shadow.snapshot_wanted() {
            self.shadow.observer.report.unanswered_snapshots += 1;
        }
    }

    /// A new epoch from `at`: the core and its chip start again as at the start, with the goals
    /// the watch boots with.
    fn reset(&mut self, at: u64) {
        self.unanswered();
        self.shadow.restart(Instant(at));
        self.shadow.driven(true);
        self.base = None;
        self.last_ms = None;
        self.transfer = false;
        self.usb_update = None;
        self.caught_up = false;
        self.epoch_dumps = 0;
        self.shadow.observer.report.resets += 1;
        self.push(Who::Line, "-- the watch reset".into());
        self.shadow.set_goals(self.shadow.now(), boot_goals());
    }

    fn line(&mut self, text: &str) {
        let text = text.trim();
        let now = self.shadow.now();
        if text.starts_with("[trace] ") {
            self.trace(text);
        } else if let Some(cmd) = text.strip_prefix(">> usb ") {
            // Sent stripped by `wrist_port.write_command`.
            match command(cmd.trim()) {
                Some(Command::Update(len, _)) => self.usb_update = Some(UsbUpdate::Asked(len)),
                Some(Command::Radio(on)) => self.shadow.radio(now, on),
                Some(Command::Profile(p)) => self.shadow.timing(now, if p == Profile::Battery { Power::Battery } else { Power::Cable }, self.transfer),
                Some(Command::Disconnect) => self.shadow.operator_disconnect(now),
                Some(Command::IntervalNow(min, max)) => self.shadow.interval_now(now, min, max),
                Some(Command::Answer(yes)) => self.shadow.answer(now, yes),
                None => {}
            }
            self.push(Who::Line, format!(">> {cmd}"));
        } else if let Some(update) = text.strip_prefix("[update] ") {
            self.update_line(update);
            self.push(Who::Line, text.to_string());
        } else if let Some(rest) = text.strip_prefix("[ble] dump next ") {
            // The first dump of a boot the replay caught up from: its lost head is caught up from,
            // as any capture's start is; any other loss is a gap in the core's inputs.
            let lost = rest.split(", ").find_map(|p| p.strip_suffix(" bytes lost")).and_then(|n| n.parse::<u64>().ok()).unwrap_or_default();
            let excused = self.caught_up && self.epoch_dumps == 0;
            self.epoch_dumps += 1;
            let report = &mut self.shadow.observer.report;
            if !excused {
                report.dump_lost += lost;
            }
            report.dumps += 1;
            self.push(Who::Line, text.to_string());
        } else if text.starts_with("[ble]") || text.starts_with("== ") {
            self.push(Who::Line, text.to_string());
        }
    }

    /// An over-the-air image is a transfer from its `send` line until any line but its progress.
    fn update_line(&mut self, rest: &str) {
        if rest.strip_prefix("received ").is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit())) {
            return;
        }
        let mut console_send = false;
        match self.usb_update {
            // Staging is the USB image's alone until its answer, so no line between is a transfer's.
            Some(UsbUpdate::Sending) => {
                if rest.starts_with("ok ") || rest.starts_with("error: ") {
                    self.usb_update = None;
                }
                return;
            }
            Some(UsbUpdate::Asked(len)) if rest.strip_prefix("send ").and_then(|r| r.strip_suffix(" bytes")) == Some(&*len.to_string()) => {
                self.usb_update = Some(UsbUpdate::Sending);
                console_send = true;
            }
            // The console's refusal, which an over-the-air start refused with the same text
            // cannot be told from.
            Some(UsbUpdate::Asked(_)) if rest.strip_prefix("error: ").is_some_and(|why| REFUSED_BEFORE_SEND.contains(&why)) => {
                self.usb_update = None;
                return;
            }
            _ => {}
        }
        let transfer = rest.starts_with("send ") && !console_send;
        if transfer != self.transfer {
            self.transfer = transfer;
            let now = self.shadow.now();
            let power = self.shadow.goals().power;
            self.shadow.timing(now, power, transfer);
        }
    }

    fn trace(&mut self, text: &str) {
        let Some(t) = capture::trace(text) else { return };
        let (ms, dir, cut) = (t.ms, t.kind, t.cut);
        let bytes = t.packet.unwrap_or_default();
        self.last_ms = Some(ms);
        let at = Instant(self.base.unwrap_or_default() + ms);
        self.shadow.advance(at);
        match dir {
            "r" => self.push(Who::Driver, "reset the chip".into()),
            "c" => {
                if let Some(&[lo, hi]) = bytes.get(1..3) {
                    self.shadow.command(at, u16::from_le_bytes([lo, hi]), bytes.get(4..).unwrap_or_default());
                }
            }
            "e" => {
                if cut && matches!(bluenrg2::parse(&bytes), bluenrg2::Event::Malformed(_)) {
                    self.shadow.observer.report.cut_by_the_trace += 1;
                    return;
                }
                self.shadow.event(at, &bytes);
            }
            // Whether the core drives from this start, the driver's lost read, and its snapshot
            // (none when it failed), as a driven core was told them.
            "m" => self.shadow.driven(bytes == [1]),
            "l" => self.shadow.lost_read(at),
            // The flight recorder's inputs beside the chip's: the core's goals and timing, the
            // radio, and the operator's answer, ask and hang-up.
            "G" => match record::decode_goals(&bytes) {
                Some(g) => self.shadow.set_goals(at, g),
                None => self.shadow.observer.report.unknown_events += 1,
            },
            "g" => {
                if let &[power, transfer] = bytes.as_slice() {
                    self.transfer = transfer == 1;
                    self.shadow.timing(at, if power == 1 { Power::Battery } else { Power::Cable }, self.transfer);
                }
            }
            "o" => self.shadow.radio(at, bytes == [1]),
            "a" => self.shadow.answer(at, bytes == [1]),
            "i" => {
                if let &[a, b, c, d] = bytes.as_slice() {
                    self.shadow.interval_now(at, u16::from_le_bytes([a, b]), u16::from_le_bytes([c, d]));
                }
            }
            "d" => self.shadow.operator_disconnect(at),
            "x" => match <[u8; 4]>::try_from(bytes.as_slice()) {
                Ok(count) => self.shadow.observer.report.not_kept += u32::from_le_bytes(count),
                Err(_) => self.shadow.observer.report.unknown_events += 1,
            },
            "s" => {
                let snapshot = if bytes.is_empty() {
                    Err(Fault::Silent)
                } else if let Some(s) = trace::decode(at, &bytes) {
                    Ok(s)
                } else {
                    self.shadow.observer.report.unknown_events += 1;
                    return;
                };
                if self.shadow.take_snapshot() {
                    self.shadow.snapshotted(at, snapshot);
                } else {
                    self.shadow.observer.report.unasked_snapshots += 1;
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
    let mut r = Replay { shadow: Shadow::new(INFO, boot_goals(), Record::default()), base: Some(0), last_ms: None, transfer: false, usb_update: None, caught_up: false, epoch_dumps: 0 };
    // Driven until a start's `m` says otherwise.
    r.shadow.driven(true);
    let mut started = false;
    let mut goals_first = false;
    let mut offset: Option<i64> = None;
    for line in capture.lines() {
        let (host, text) = stamped(line).map_or((None, line), |s| (i64::try_from(s.ms).ok(), s.text));
        let trace = capture::trace(text.trim());
        let traced = trace.as_ref().map(|t| t.ms);
        // This line on the timeline by the host's clock, which runs on through a reset.
        let now = r.shadow.now().0;
        let by_host = host.zip(offset).and_then(|(h, o)| u64::try_from(h + o).ok()).map_or(now, |t| t.max(now));
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
                    r.shadow.advance(Instant(at));
                }
            }
            _ => {}
        }
        if !started {
            if let Some(ms) = traced {
                r.shadow.jump(Instant(ms));
                let goals = r.shadow.goals();
                r.shadow.set_goals(Instant(ms), goals);
                started = true;
                // A dump from the watch's start opens with its bring-up's goals, then the reset:
                // which it is, the record after a first `G` says.
                goals_first = trace.as_ref().is_some_and(|t| t.kind == "G");
                if !goals_first && !trace.as_ref().is_some_and(comes_up) {
                    r.seed();
                }
            }
        } else if goals_first && traced.is_some() {
            goals_first = false;
            if !trace.as_ref().is_some_and(comes_up) {
                r.seed();
            }
        }
        r.line(text);
    }
    r.unanswered();
    r.shadow.finish();
    r.shadow.observer.report
}

/// Whether a trace line is the driver resetting the chip or the chip's ready.
fn comes_up(t: &capture::Trace<'_>) -> bool {
    let started = |p: &[u8]| {
        matches!(bluenrg2::parse(p), bluenrg2::Event::Vendor(bluenrg2::VendorEvent::BlueInitialized { reason: Some(bluenrg2::BootReason::STARTED), .. }))
    };
    t.kind == "r" || (t.kind == "e" && t.packet.as_deref().is_some_and(started))
}
