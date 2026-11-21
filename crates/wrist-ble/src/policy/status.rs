//! The one description of Bluetooth the user sees: a pure function of the core's state and the
//! time, rendered by every display alike.

use core::fmt::{self, Write};

use crate::{Instant, Millis};

use crate::policy::chip::{Chip, FaultCause};
use crate::policy::goals::MAX_PERIPHERALS;
use crate::policy::invariants::REACHABLE_GAP;
use crate::policy::peripheral::{fresh_for, Peripheral, Stage, StopReason, Then};
use crate::policy::phone::{Phone, Security, ASK_TO_PAIR_AFTER};
use crate::policy::plan::{Conflict, PlanHealth, Verdict};
use crate::policy::profile::{Contact, Kind, Profile, Silence};
use crate::policy::State;

/// No reading older than this is shown as current.
pub const CURRENT_FOR: Millis = Millis::secs(3);

/// A reading older than this is not shown at all.
pub const HIDE_AFTER: Millis = Millis::secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub radio: RadioStatus,
    pub phone: PhoneStatus,
    pub peers: [Option<PeerStatus>; MAX_PERIPHERALS],
    pub plan: PlanHealth,
    pub drift: Millis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioStatus {
    Off,
    Starting,
    Ok,
    Fault(FaultCause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneStatus {
    Off,
    Fault { cause: FaultCause, retry_at: Instant },
    Pairing { value: u32, left: Millis },
    Connected,
    /// Connected, unencrypted and still within the time a bonded phone takes to encrypt, or not
    /// known to be either.
    Securing,
    Unsecured { since: Millis },
    /// `None` within a planned gap in advertising.
    Discoverable { interval_ms: Option<u32> },
    Unreachable { reason: Unreachable, since: Millis },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreachable {
    Starting,
    /// The user turned discoverability off.
    Hidden,
    Busy(Conflict),
    /// The plan wants advertising and the radio has not reached it.
    NotConverged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerStatus {
    pub kind: Kind,
    pub unit: &'static str,
    pub state: PeerState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerState {
    Off,
    Finding { slow: bool, since: Millis },
    Connecting,
    /// `current` is false once the reading is older than the profile allows: shown greyed.
    Live { value: u16, age: Millis, current: bool },
    Unstable { value: Option<u16>, age: Millis, current: bool },
    Idle,
    NoContact,
    Quiet { value: Option<u16>, age: Millis },
    Reconnecting { since: Millis, attempts: u16 },
    /// Let go for lacking the service, and looked at again once `left` is up.
    Rechecking { left: Option<Millis> },
    Stopped(StopReason),
    Incompatible,
}

#[must_use]
pub fn status(state: &State, now: Instant) -> Status {
    let mut peers = [None; MAX_PERIPHERALS];
    for (out, peer) in peers.iter_mut().zip(&state.peers) {
        *out = peer.as_ref().map(|p| PeerStatus {
            kind: p.want.profile.kind,
            unit: p.want.profile.unit,
            state: peer_state(&p.machine, p.want.profile, now),
        });
    }
    Status {
        radio: match state.chip.state {
            Chip::Off => RadioStatus::Off,
            Chip::Starting { .. } => RadioStatus::Starting,
            Chip::Ready(_) => RadioStatus::Ok,
            Chip::Fault { cause, .. } => RadioStatus::Fault(cause),
        },
        phone: phone_status(state, now),
        peers,
        plan: state.plan,
        drift: state.drift_since.map_or(Millis(0), |at| now.since(at)),
    }
}

fn phone_status(state: &State, now: Instant) -> PhoneStatus {
    match state.chip.state {
        Chip::Off => PhoneStatus::Off,
        Chip::Fault { cause, retry_at } => PhoneStatus::Fault { cause, retry_at },
        Chip::Starting { since } => PhoneStatus::Unreachable { reason: Unreachable::Starting, since: now.since(since) },
        Chip::Ready(_) => match state.phone {
            Phone::Up(l) => match l.security {
                Security::Pairing { value, answered: true, .. } => PhoneStatus::Pairing { value, left: Millis(0) },
                Security::Pairing { value, deadline, .. } => PhoneStatus::Pairing { value, left: deadline.since(now) },
                Security::Secure => PhoneStatus::Connected,
                Security::Unsecured if now.since(l.since) > ASK_TO_PAIR_AFTER => PhoneStatus::Unsecured { since: now.since(l.since) },
                Security::Unsecured | Security::Unknown => PhoneStatus::Securing,
            },
            Phone::Off | Phone::Discoverable => match state.radio.advertising {
                Some(a) if a.connectable => PhoneStatus::Discoverable { interval_ms: Some(a.interval_min.as_millis()) },
                _ if now.since(state.silent_since) <= REACHABLE_GAP => PhoneStatus::Discoverable { interval_ms: None },
                _ => PhoneStatus::Unreachable {
                    reason: match state.phone_verdict {
                        Verdict::NotWanted => Unreachable::Hidden,
                        Verdict::Busy(c) => Unreachable::Busy(c),
                        Verdict::Fits => Unreachable::NotConverged,
                    },
                    since: now.since(state.silent_since),
                },
            },
        },
    }
}

/// The status of one peripheral; a [`Then`] being dropped towards shows as where it leads.
#[must_use]
pub fn peer_state(p: &Peripheral, profile: &Profile, now: Instant) -> PeerState {
    let stopped = |r| if r == StopReason::Incompatible { PeerState::Incompatible } else { PeerState::Stopped(r) };
    let searching = || match (p.slow_until, p.lost_since) {
        (Some(_), _) => PeerState::Finding { slow: true, since: now.since(p.chosen_at) },
        (None, Some(lost)) => PeerState::Reconnecting { since: now.since(lost), attempts: p.attempts },
        (None, None) => PeerState::Finding { slow: false, since: now.since(p.chosen_at) },
    };
    match p.stage {
        Stage::Off | Stage::Dropping { then: Then::Off, .. } => PeerState::Off,
        Stage::Stopped(r) | Stage::Dropping { then: Then::Stop(r), .. } => stopped(r),
        Stage::Dropping { then: Then::Reconnect, .. } | Stage::Seeking { .. } | Stage::Backoff { .. } | Stage::SlowWait { .. } => {
            searching()
        }
        Stage::Linked { .. } => PeerState::Connecting,
        Stage::Waiting { until } => PeerState::Rechecking { left: Some(until.since(now)) },
        Stage::Dropping { then: Then::Rediscover, .. } => PeerState::Rechecking { left: None },
        Stage::Live { since, .. } => live_state(p, profile, since, now),
    }
}

fn live_state(p: &Peripheral, profile: &Profile, since: Instant, now: Instant) -> PeerState {
    let reading = p.last.filter(|(at, _)| *at >= since);
    let heard = reading.map_or(since, |(at, _)| at);
    let age = now.since(heard);
    let shown = reading.filter(|(_, r)| age <= HIDE_AFTER && r.contact != Contact::Lost).map(|(_, r)| r.value);
    if age > profile.quiet {
        return match profile.silence {
            Silence::Idle => PeerState::Idle,
            Silence::Fault if p.unstable(profile, now, true) => PeerState::Unstable { value: shown, age, current: false },
            Silence::Fault => PeerState::Quiet { value: shown, age },
        };
    }
    let Some((_, r)) = reading else { return PeerState::Connecting };
    if r.contact == Contact::Lost {
        return PeerState::NoContact;
    }
    let current = age <= fresh_for(profile);
    if p.unstable(profile, now, false) {
        PeerState::Unstable { value: Some(r.value), age, current }
    } else {
        PeerState::Live { value: r.value, age, current }
    }
}

impl Status {
    /// The console's machine line, `[ble] status phone=… peers=… radio=… plan=… drift=…s`.
    ///
    /// # Errors
    ///
    /// Only those of `w`.
    pub fn write_line<W: Write>(&self, w: &mut W) -> fmt::Result {
        w.write_str("[ble] status phone=")?;
        write_phone(w, self.phone)?;
        w.write_str(" peers=")?;
        let mut any = false;
        for (i, p) in self.peers.iter().enumerate() {
            let Some(p) = p else { continue };
            if any {
                w.write_char(' ')?;
            }
            any = true;
            w.write_str(p.kind.key())?;
            let same = self.peers[..i].iter().flatten().filter(|q| q.kind == p.kind).count();
            if same > 0 {
                write!(w, "#{}", same + 1)?;
            }
            w.write_char(':')?;
            write_peer(w, p)?;
        }
        if !any {
            w.write_char('-')?;
        }
        w.write_str(" radio=")?;
        match self.radio {
            RadioStatus::Off => w.write_str("off")?,
            RadioStatus::Starting => w.write_str("starting")?,
            RadioStatus::Ok => w.write_str("ok")?,
            RadioStatus::Fault(c) => write!(w, "fault({})", cause(c))?,
        }
        w.write_str(" plan=")?;
        match self.plan {
            PlanHealth::Ok => w.write_str("ok")?,
            PlanHealth::Infeasible(c) => write!(w, "infeasible({})", conflict(c))?,
        }
        write!(w, " drift={}s", self.drift.0 / 1_000)
    }
}

/// The least time between two status lines, so a flickering status cannot flood the console.
pub const LOG_GAP: Millis = Millis::secs(1);

/// When the status next changes by time alone, with no event.
#[must_use]
pub fn next_change(state: &State, now: Instant) -> Option<Instant> {
    let after = |at: Instant| Some(at + Millis(1)).filter(|&t| t > now);
    let phone = match (state.chip.state, state.phone) {
        (Chip::Ready(_), Phone::Up(l)) if matches!(l.security, Security::Unsecured | Security::Unknown) => {
            after(l.since + ASK_TO_PAIR_AFTER)
        }
        (Chip::Ready(_), Phone::Off | Phone::Discoverable)
            if !matches!(state.radio.advertising, Some(a) if a.connectable) =>
        {
            after(state.silent_since + REACHABLE_GAP)
        }
        _ => None,
    };
    let peers = state.peers.iter().any(Option::is_some).then(|| now + LOG_GAP);
    [phone, peers].into_iter().flatten().min()
}

impl Status {
    /// The status without its ages, countdowns and readings: what a status line is printed for a
    /// change of.
    #[must_use]
    pub fn shape(&self) -> Self {
        let phone = match self.phone {
            PhoneStatus::Pairing { value, .. } => PhoneStatus::Pairing { value, left: Millis(0) },
            PhoneStatus::Unsecured { .. } => PhoneStatus::Unsecured { since: Millis(0) },
            PhoneStatus::Unreachable { reason, .. } => PhoneStatus::Unreachable { reason, since: Millis(0) },
            p => p,
        };
        let mut peers = self.peers;
        for p in peers.iter_mut().flatten() {
            p.state = match p.state {
                PeerState::Finding { slow, .. } => PeerState::Finding { slow, since: Millis(0) },
                PeerState::Live { current, .. } => PeerState::Live { value: 0, age: Millis(0), current },
                PeerState::Unstable { current, .. } => PeerState::Unstable { value: None, age: Millis(0), current },
                PeerState::Quiet { .. } => PeerState::Quiet { value: None, age: Millis(0) },
                PeerState::Reconnecting { attempts, .. } => PeerState::Reconnecting { since: Millis(0), attempts },
                PeerState::Rechecking { left } => PeerState::Rechecking { left: left.map(|_| Millis(0)) },
                s => s,
            };
        }
        Self { phone, peers, drift: Millis(0), ..*self }
    }

    /// The phone's status in words, without ages, for the `ble` line.
    ///
    /// # Errors
    ///
    /// Only those of `w`.
    pub fn write_words<W: Write>(&self, w: &mut W) -> fmt::Result {
        match self.phone {
            PhoneStatus::Off => w.write_str("off"),
            PhoneStatus::Fault { cause: c, .. } => write!(w, "radio fault ({}), retrying", cause(c)),
            PhoneStatus::Pairing { value, .. } => write!(w, "connected, pairing: compare {value:06}"),
            PhoneStatus::Connected => w.write_str("connected, encrypted"),
            PhoneStatus::Securing => w.write_str("connected, securing"),
            PhoneStatus::Unsecured { .. } => w.write_str("connected, NOT encrypted"),
            PhoneStatus::Discoverable { interval_ms: Some(ms) } => write!(w, "advertising as UNA every {ms} ms"),
            PhoneStatus::Discoverable { interval_ms: None } => w.write_str("advertising as UNA, between intervals"),
            PhoneStatus::Unreachable { reason: Unreachable::Starting, .. } => w.write_str("starting"),
            PhoneStatus::Unreachable { reason: Unreachable::Hidden, .. } => w.write_str("up, not advertising (hidden)"),
            PhoneStatus::Unreachable { reason: Unreachable::Busy(c), .. } => {
                write!(w, "up, not advertising (busy: {})", conflict(c))
            }
            PhoneStatus::Unreachable { reason: Unreachable::NotConverged, .. } => {
                w.write_str("up, not advertising (not yet where the plan wants)")
            }
        }
    }
}

/// Which statuses the console prints: each change of [`Status::shape`], held to [`LOG_GAP`].
#[derive(Clone, Copy, Debug, Default)]
pub struct StatusLog {
    shown: Option<Status>,
    at: Option<Instant>,
    held: bool,
}

impl StatusLog {
    pub const NEW: Self = Self { shown: None, at: None, held: false };

    /// Whether to print `s` now; once true, `s` counts as shown.
    pub fn due(&mut self, now: Instant, s: &Status) -> bool {
        let shape = s.shape();
        if self.shown == Some(shape) {
            self.held = false;
            return false;
        }
        if self.at.is_some_and(|at| now.since(at) < LOG_GAP) {
            self.held = true;
            return false;
        }
        self.shown = Some(shape);
        self.at = Some(now);
        self.held = false;
        true
    }

    /// When a held change may be printed.
    #[must_use]
    pub fn next(&self) -> Option<Instant> {
        self.at.filter(|_| self.held).map(|at| at + LOG_GAP)
    }
}

fn write_phone<W: Write>(w: &mut W, p: PhoneStatus) -> fmt::Result {
    match p {
        PhoneStatus::Off => w.write_str("off"),
        PhoneStatus::Fault { cause: c, .. } => write!(w, "fault({})", cause(c)),
        PhoneStatus::Pairing { value, left } => write!(w, "pairing({value:06},{}s)", left.0.div_ceil(1_000)),
        PhoneStatus::Connected => w.write_str("connected"),
        PhoneStatus::Securing => w.write_str("securing"),
        PhoneStatus::Unsecured { since } => write!(w, "unsecured({}s)", since.0 / 1_000),
        PhoneStatus::Discoverable { interval_ms: Some(ms) } => write!(w, "discoverable({ms}ms)"),
        PhoneStatus::Discoverable { interval_ms: None } => w.write_str("discoverable"),
        PhoneStatus::Unreachable { reason, since } => {
            let reason = match reason {
                Unreachable::Starting => "starting",
                Unreachable::Hidden => "hidden",
                Unreachable::Busy(c) => conflict(c),
                Unreachable::NotConverged => "not-converged",
            };
            write!(w, "unreachable({reason},{}s)", since.0 / 1_000)
        }
    }
}

fn write_peer<W: Write>(w: &mut W, p: &PeerStatus) -> fmt::Result {
    let tenths = |age: Millis| (age.0 / 1_000, age.0 % 1_000 / 100);
    let unit = p.unit;
    match p.state {
        PeerState::Off => w.write_str("off"),
        PeerState::Finding { slow: true, .. } => w.write_str("finding(slow)"),
        PeerState::Finding { slow: false, since } => write!(w, "finding({}s)", since.0 / 1_000),
        PeerState::Connecting => w.write_str("connecting"),
        PeerState::Live { value, age, .. } => {
            let (secs, tenth) = tenths(age);
            write!(w, "live({value}{unit},{secs}.{tenth}s)")
        }
        PeerState::Unstable { value, age, .. } => {
            let (secs, tenth) = tenths(age);
            match value {
                Some(value) => write!(w, "unstable({value}{unit},{secs}.{tenth}s)"),
                None => write!(w, "unstable(-,{secs}.{tenth}s)"),
            }
        }
        PeerState::Idle => w.write_str("idle"),
        PeerState::NoContact => w.write_str("no-contact"),
        PeerState::Quiet { value: Some(value), age } => write!(w, "quiet({value}{unit},{}s)", age.0 / 1_000),
        PeerState::Quiet { value: None, age } => write!(w, "quiet(-,{}s)", age.0 / 1_000),
        PeerState::Reconnecting { since, .. } => write!(w, "reconnecting({}s)", since.0 / 1_000),
        PeerState::Rechecking { left: Some(left) } => write!(w, "rechecking({}s)", left.0.div_ceil(1_000)),
        PeerState::Rechecking { left: None } => w.write_str("rechecking"),
        PeerState::Stopped(reason) => {
            let reason = match reason {
                StopReason::NotFound => "not-found",
                StopReason::KeptDropping => "kept-dropping",
                StopReason::PairingFailed => "pairing-failed",
                StopReason::Incompatible => "incompatible",
                StopReason::RadioBusy(c) => return write!(w, "stopped(busy-{})", conflict(c)),
            };
            write!(w, "stopped({reason})")
        }
        PeerState::Incompatible => w.write_str("incompatible"),
    }
}

fn cause(c: FaultCause) -> &'static str {
    match c {
        FaultCause::StartFailed => "start-failed",
        FaultCause::BaselineRefused => "baseline-refused",
        FaultCause::KeptResetting => "kept-resetting",
    }
}

fn conflict(c: Conflict) -> &'static str {
    match c {
        Conflict::Phone => "phone",
        Conflict::Peripheral => "peripheral",
        Conflict::Scan => "scan",
        Conflict::Transfer => "transfer",
        Conflict::Refused => "refused",
        Conflict::Capacity => "capacity",
    }
}
