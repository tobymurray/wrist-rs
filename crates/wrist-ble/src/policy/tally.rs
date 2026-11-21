//! What went wrong and how often, kept as the core judges it, for the console.

use core::fmt::{self, Write};

use crate::policy::chip;
use crate::policy::engine::Input;
use crate::policy::phone::{Phone, Security};
use crate::policy::status::{status, PhoneStatus, Unreachable};
use crate::policy::State;
use crate::{ConnHandle, Failure, FailureKind, Instant, LeConnRole, Millis, PairingEvent, PairingStep, PeerAddr, RadioEvent, ResetCause, SchedKind};

/// A phone link that ends this soon after encrypting is counted as ending early.
pub const EARLY_END: Millis = Millis::secs(10);

/// The window the operations' peak is counted over, as I2 counts it.
pub const COMMAND_WINDOW: Millis = Millis::mins(1);

/// The most operations in one [`COMMAND_WINDOW`] the peak can tell apart, more than I2 allows;
/// more read as at least this.
pub const PEAK_KEPT: usize = 64;

/// Counts by key, for the first `N` keys seen; later keys are only counted together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts<K: Copy + PartialEq, const N: usize> {
    pub rows: [Option<(K, u32)>; N],
    pub other: u32,
}

impl<K: Copy + PartialEq, const N: usize> Counts<K, N> {
    pub const NEW: Self = Self { rows: [None; N], other: 0 };

    pub fn add(&mut self, key: K) {
        if let Some((_, n)) = self.rows.iter_mut().flatten().find(|(k, _)| *k == key) {
            *n += 1;
        } else if let Some(slot) = self.rows.iter_mut().find(|r| r.is_none()) {
            *slot = Some((key, 1));
        } else {
            self.other += 1;
        }
    }

    #[must_use]
    pub fn total(&self) -> u32 {
        self.rows.iter().flatten().map(|(_, n)| n).sum::<u32>() + self.other
    }

    fn write<W: Write>(&self, w: &mut W, key: impl Fn(&mut W, K) -> fmt::Result) -> fmt::Result {
        let mut any = false;
        for &(k, n) in self.rows.iter().flatten() {
            w.write_char(' ')?;
            key(w, k)?;
            write!(w, " x{n}")?;
            any = true;
        }
        if self.other > 0 {
            write!(w, " other x{}", self.other)?;
        } else if !any {
            w.write_str(" none")?;
        }
        Ok(())
    }
}

/// Why the core restarted a chip that was starting or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    Reset(ResetCause),
    StartFailed,
    BaselineRefused,
    /// A start's deadline passed.
    Deadline,
}

impl Restart {
    /// Why, from the event that restarted a chip starting or up: only a reset, a failed start, a
    /// refused baseline or a deadline does.
    #[must_use]
    pub fn of(e: chip::Event) -> Self {
        match e {
            chip::Event::Reset(c) => Self::Reset(c),
            chip::Event::StartFailed => Self::StartFailed,
            chip::Event::BaselineRefused => Self::BaselineRefused,
            _ => Self::Deadline,
        }
    }
}

/// When a spell started, and the longest one that has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spell {
    from: Option<Instant>,
    worst: Millis,
}

impl Spell {
    fn track(&mut self, now: Instant, from: Option<Instant>) {
        match (self.from, from) {
            (Some(was), None) => {
                self.worst = self.worst.max(now.since(was));
                self.from = None;
            }
            (None, Some(f)) => self.from = Some(f),
            _ => {}
        }
    }

    #[must_use]
    pub fn now(&self, now: Instant) -> Millis {
        self.from.map_or(Millis(0), |f| now.since(f))
    }

    #[must_use]
    pub fn worst(&self, now: Instant) -> Millis {
        self.worst.max(self.now(now))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tally {
    /// The core's operations refused or failed, by kind and the chip's own code; an ask held
    /// for the central's answer is not one.
    pub failures: Counts<(FailureKind, u16), 8>,
    /// Ends of the core's phone link, by reason.
    pub hang_ups: Counts<u8, 6>,
    pub pairings_ok: u32,
    pub pairing_failures: Counts<PairingStep, 3>,
    /// The step of the last pairing that failed, and its peer when its link's start was seen.
    pub last_pairing_failure: Option<(Option<PeerAddr>, PairingStep)>,
    /// Ends of the phone link within [`EARLY_END`] of its encrypting, by reason.
    pub early_ends: Counts<u8, 4>,
    pub restarts: Counts<Restart, 4>,
    /// The most operations the core asked for in any [`COMMAND_WINDOW`], up to [`PEAK_KEPT`].
    pub commands_peak: u32,
    asked: [Option<Instant>; PEAK_KEPT],
    asked_next: usize,
    /// The phone unreachable for a reason other than being hidden.
    pub unreachable: Spell,
    pub drift: Spell,
    /// Hidden at the last step: an unreachable spell after it starts then, not where advertising
    /// stopped.
    hidden: bool,
    phone: Option<ConnHandle>,
    phone_peer: Option<PeerAddr>,
    secure_since: Option<Instant>,
}

impl Tally {
    pub const NEW: Self = Self {
        failures: Counts::NEW,
        hang_ups: Counts::NEW,
        pairings_ok: 0,
        pairing_failures: Counts::NEW,
        last_pairing_failure: None,
        early_ends: Counts::NEW,
        restarts: Counts::NEW,
        commands_peak: 0,
        asked: [None; PEAK_KEPT],
        asked_next: 0,
        unreachable: Spell { from: None, worst: Millis(0) },
        drift: Spell { from: None, worst: Millis(0) },
        hidden: false,
        phone: None,
        phone_peer: None,
        secure_since: None,
    };

    /// An operation the core counts as refused or failed.
    pub fn failure(&mut self, f: Failure) {
        self.failures.add((f.kind, f.native));
    }

    /// A start the core asked for that cut short a chip starting or up.
    pub fn restart(&mut self, why: Restart) {
        self.restarts.add(why);
    }

    /// The end of one of the core's steps: what it was told, the state it reached, and whether it
    /// asked for an operation.
    pub fn step(&mut self, now: Instant, input: &Input, state: &State, asked: bool) {
        if let Input::Radio(e) = *input {
            self.event(now, e);
        }
        if asked {
            self.ask(now);
        }
        let link = state.phone.link();
        if link != self.phone {
            self.phone = link;
            self.secure_since = None;
            self.phone_peer = None;
        }
        if let Input::Radio(RadioEvent::LinkUp { link: up, role: LeConnRole::Peripheral, peer, .. }) = *input {
            if Some(up) == link {
                self.phone_peer = Some(peer);
            }
        }
        let secure = matches!(state.phone, Phone::Up(l) if l.security == Security::Secure);
        self.secure_since = if secure { self.secure_since.or(Some(now)) } else { None };
        let (unreachable, hidden) = match status(state, now).phone {
            PhoneStatus::Unreachable { reason: Unreachable::Hidden, .. } => (None, true),
            PhoneStatus::Unreachable { since, .. } => (Some(if self.hidden { now } else { now - since }), false),
            _ => (None, false),
        };
        self.hidden = hidden;
        self.unreachable.track(now, unreachable);
        self.drift.track(now, state.drift_since);
    }

    /// The core's phone link as it stood before this step's event.
    fn event(&mut self, now: Instant, e: RadioEvent) {
        match e {
            RadioEvent::LinkDown { link, reason } if self.phone == Some(link) => {
                let reason = reason.into_inner();
                self.hang_ups.add(reason);
                if self.secure_since.is_some_and(|at| now.since(at) <= EARLY_END) {
                    self.early_ends.add(reason);
                }
            }
            RadioEvent::Pairing(PairingEvent { link, step }) if self.phone == Some(link) => match step {
                PairingStep::Complete => self.pairings_ok += 1,
                PairingStep::Failed | PairingStep::LocalKeyMissing | PairingStep::PeerKeyMissing => {
                    self.pairing_failures.add(step);
                    self.last_pairing_failure = Some((self.phone_peer, step));
                }
                PairingStep::Numeric(_) | PairingStep::PasskeyWanted => {}
            },
            _ => {}
        }
    }

    /// One operation asked at `now`; the peak is the most within any window ending at an ask.
    #[allow(clippy::cast_possible_truncation)]
    fn ask(&mut self, now: Instant) {
        self.asked[self.asked_next] = Some(now);
        self.asked_next = (self.asked_next + 1) % PEAK_KEPT;
        let within = self.asked.iter().flatten().filter(|&&t| now.since(t) < COMMAND_WINDOW).count();
        self.commands_peak = self.commands_peak.max(within as u32);
    }

    /// `[ble] counts …`, one line, its spells measured to `now`.
    ///
    /// # Errors
    ///
    /// Only those of `w`.
    pub fn write_line<W: Write>(&self, w: &mut W, now: Instant) -> fmt::Result {
        w.write_str("[ble] counts: failures")?;
        self.failures.write(w, |w, (kind, native)| {
            write_kind(w, kind)?;
            write!(w, "/{native:02X}")
        })?;
        w.write_str("; phone hang-ups")?;
        self.hang_ups.write(w, |w, r| write!(w, "{r:02X}"))?;
        write!(w, "; pairings ok {} failed", self.pairings_ok)?;
        self.pairing_failures.write(w, |w, s| w.write_str(step_name(s)))?;
        if let Some((peer, step)) = self.last_pairing_failure {
            write!(w, " (last {}", step_name(step))?;
            if let Some(peer) = peer {
                w.write_str(" from ")?;
                write_peer(w, peer)?;
            }
            w.write_char(')')?;
        }
        write!(w, "; hung up within {} s of encrypting", EARLY_END.0 / 1_000)?;
        self.early_ends.write(w, |w, r| write!(w, "{r:02X}"))?;
        w.write_str("; chip restarts")?;
        self.restarts.write(w, |w, r| w.write_str(restart_name(r)))?;
        let at_least = if self.commands_peak as usize >= PEAK_KEPT { "at least " } else { "" };
        write!(
            w,
            "; operations in a minute at most {at_least}{}; unreachable now {} s, worst {} s; plan drift now {} s, worst {} s",
            self.commands_peak,
            self.unreachable.now(now).0 / 1_000,
            self.unreachable.worst(now).0 / 1_000,
            self.drift.now(now).0 / 1_000,
            self.drift.worst(now).0 / 1_000,
        )
    }
}

impl Default for Tally {
    fn default() -> Self {
        Self::NEW
    }
}

fn write_kind<W: Write>(w: &mut W, k: FailureKind) -> fmt::Result {
    w.write_str(match k {
        FailureKind::Schedule(SchedKind::NewIntervalFailed) => "schedule-interval",
        FailureKind::Schedule(SchedKind::IntervalTooLarge) => "schedule-too-large",
        FailureKind::Schedule(SchedKind::LengthFailed) => "schedule-length",
        FailureKind::Schedule(SchedKind::Excluded(_)) => "schedule-excluded",
        FailureKind::Schedule(SchedKind::NoSlot) => "schedule-no-slot",
        FailureKind::ModeExclusion => "mode",
        FailureKind::Busy => "busy",
        FailureKind::NotTaken => "not-taken",
        FailureKind::NotConnected => "not-connected",
        FailureKind::PeerRejected => "peer-rejected",
        FailureKind::NoResources => "no-resources",
        FailureKind::Timeout => "timeout",
        FailureKind::Security => "security",
        FailureKind::NotEstablished => "not-established",
        FailureKind::Unknown => "unknown",
    })
}

fn step_name(s: PairingStep) -> &'static str {
    match s {
        PairingStep::LocalKeyMissing => "local-key-missing",
        PairingStep::PeerKeyMissing => "peer-key-missing",
        _ => "failed",
    }
}

fn restart_name(r: Restart) -> &'static str {
    match r {
        Restart::Reset(ResetCause::PowerOn) => "reset-power-on",
        Restart::Reset(ResetCause::Watchdog) => "reset-watchdog",
        Restart::Reset(ResetCause::Lockup) => "reset-lockup",
        Restart::Reset(ResetCause::Brownout) => "reset-brownout",
        Restart::Reset(ResetCause::Crash) => "reset-crash",
        Restart::Reset(ResetCause::Updater) => "reset-updater",
        Restart::Reset(ResetCause::Other) => "reset-other",
        Restart::StartFailed => "start-failed",
        Restart::BaselineRefused => "baseline-refused",
        Restart::Deadline => "deadline",
    }
}

fn write_peer<W: Write>(w: &mut W, p: PeerAddr) -> fmt::Result {
    for (i, b) in p.addr.raw().iter().rev().enumerate() {
        if i > 0 {
            w.write_char(':')?;
        }
        write!(w, "{b:02X}")?;
    }
    Ok(())
}
