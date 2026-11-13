//! A capture of the watch's own driver (`ble-trace`) replayed through the core: the core sees the
//! chip's real events, its operations are taken as the driver's, and its choices and status are
//! set beside what the driver did ([`wrist_ble_shadow`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use wrist_ble::{Instant, Op, RadioEvent, Snapshot};
use wrist_ble_core::chip::Action;
use wrist_ble_core::engine::Tell;
use wrist_ble_core::goals::{Goals, PhoneGoal, Power};
use wrist_ble_core::{status, State};
use wrist_ble_shadow::verdict::Disagreement;
use wrist_ble_shadow::{DriverOp, Observer, Shadow};

use crate::fake::INFO;

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
    /// Answers the core was owed that the shadow had no room for.
    pub lost: u32,
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

    /// Where the core and the driver acted differently ([`wrist_ble_shadow::verdict`]).
    #[must_use]
    pub fn disagreements(&self) -> Vec<String> {
        let mut found = self.found.clone();
        found.sort_by_key(|d| !d.core);
        let mut out: Vec<String> = found.iter().map(ToString::to_string).collect();
        if self.unknown_events > 0 {
            out.push(format!("{} events the core could not read", self.unknown_events));
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
}

struct Replay {
    shadow: Shadow<Record>,
    /// Where this epoch's milliseconds start on the timeline: 0 until the watch first resets,
    /// then unknown until the epoch's first trace line.
    base: Option<u64>,
    /// The chip's last milliseconds in this epoch.
    last_ms: Option<u64>,
}

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

    /// A new epoch from `at`: the core and its chip start again as at the start, with the goals
    /// the watch boots with.
    fn reset(&mut self, at: u64) {
        self.shadow.restart(Instant(at));
        self.base = None;
        self.last_ms = None;
        self.shadow.observer.report.resets += 1;
        self.push(Who::Line, "-- the watch reset".into());
        self.shadow.set_goals(self.shadow.now(), boot_goals());
    }

    fn line(&mut self, text: &str) {
        let text = text.trim();
        let now = self.shadow.now();
        if let Some(rest) = text.strip_prefix("[trace] ") {
            self.trace(rest);
        } else if let Some(cmd) = text.strip_prefix(">> usb ") {
            match cmd.trim() {
                "ble off" => self.shadow.radio(now, false),
                "ble on" => self.shadow.radio(now, true),
                "ble profile battery" => self.shadow.power(now, Power::Battery),
                "ble profile cable" | "ble profile auto" => self.shadow.power(now, Power::Cable),
                "ble disconnect" => self.shadow.operator_disconnect(now),
                c if c.starts_with("ble interval now ") => {
                    let mut units = c[17..].split_whitespace().filter_map(|w| w.parse::<u16>().ok());
                    if let (Some(min), Some(max)) = (units.next(), units.next()) {
                        self.shadow.interval_now(now, min, max);
                    }
                }
                "ble yes" => self.shadow.answer(now, true),
                "ble no" => self.shadow.answer(now, false),
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
    let mut r = Replay { shadow: Shadow::new(INFO, boot_goals(), Record::default()), base: Some(0), last_ms: None };
    let mut started = false;
    let mut offset: Option<i64> = None;
    for line in capture.lines() {
        let (host, text) = match line.trim_start().split_once(' ') {
            Some((t, rest)) => host_ms(t).map_or((None, line), |t| (Some(t), rest)),
            None => (None, line),
        };
        let traced = text.trim().strip_prefix("[trace] ").and_then(|r| r.split_whitespace().next()).and_then(|m| m.parse::<u64>().ok());
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
                if !comes_up(text) {
                    r.seed();
                }
            }
        }
        r.line(text);
    }
    r.shadow.finish();
    r.shadow.observer.report
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
