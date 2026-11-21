//! What went wrong and how often, kept from the core's inputs and state for the console.

use core::fmt::{self, Write};

use crate::policy::engine::Input;
use crate::policy::status::{status, PhoneStatus};
use crate::policy::State;
use crate::{ConnHandle, FailureKind, Instant, LeConnRole, Millis, PairingEvent, PairingStep, PeerAddr, RadioEvent, ResetCause, SchedKind};

/// A link that ends this soon after encrypting is counted as ending early.
pub const EARLY_END: Millis = Millis::secs(10);

/// The window [`Tally`] counts the core's operations over, for their peak.
pub const COMMAND_WINDOW: Millis = Millis::mins(1);

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

/// The phone's link as the tally follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Link {
    link: ConnHandle,
    peer: PeerAddr,
    encrypted_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tally {
    /// Refusals and failures of the core's operations, by kind and the chip's own code.
    pub failures: Counts<(FailureKind, u16), 8>,
    /// Ends of the phone's links, by reason.
    pub hang_ups: Counts<u8, 6>,
    pub pairings_ok: u32,
    pub pairing_failures: Counts<PairingStep, 3>,
    /// The peer and step of the last pairing that failed.
    pub last_pairing_failure: Option<(PeerAddr, PairingStep)>,
    /// Ends of the phone's links within [`EARLY_END`] of encrypting, by reason.
    pub early_ends: Counts<u8, 4>,
    pub chip_resets: Counts<ResetCause, 4>,
    /// The most operations the core asked for in one [`COMMAND_WINDOW`].
    pub commands_peak: u32,
    commands: u32,
    window_from: Instant,
    pub unreachable_now: Millis,
    pub unreachable_worst: Millis,
    pub drift_worst: Millis,
    phone: Option<Link>,
}

impl Tally {
    pub const NEW: Self = Self {
        failures: Counts::NEW,
        hang_ups: Counts::NEW,
        pairings_ok: 0,
        pairing_failures: Counts::NEW,
        last_pairing_failure: None,
        early_ends: Counts::NEW,
        chip_resets: Counts::NEW,
        commands_peak: 0,
        commands: 0,
        window_from: Instant(0),
        unreachable_now: Millis(0),
        unreachable_worst: Millis(0),
        drift_worst: Millis(0),
        phone: None,
    };

    /// One step of the core: what it was told, the state it reached, and whether it asked for an
    /// operation.
    pub fn step(&mut self, now: Instant, input: &Input, state: &State, asked: bool) {
        match *input {
            Input::Submitted(Err(f)) => self.failures.add((f.kind, f.native)),
            Input::Radio(e) => self.event(now, e),
            _ => {}
        }
        if now.since(self.window_from) >= COMMAND_WINDOW {
            self.window_from = now;
            self.commands = 0;
        }
        if asked {
            self.commands += 1;
            self.commands_peak = self.commands_peak.max(self.commands);
        }
        self.unreachable_now = match status(state, now).phone {
            PhoneStatus::Unreachable { since, .. } => since,
            _ => Millis(0),
        };
        self.unreachable_worst = self.unreachable_worst.max(self.unreachable_now);
        if let Some(at) = state.drift_since {
            self.drift_worst = self.drift_worst.max(now.since(at));
        }
    }

    fn event(&mut self, now: Instant, e: RadioEvent) {
        match e {
            RadioEvent::Done(_, Err(f)) | RadioEvent::Params { result: Err(f), .. } => self.failures.add((f.kind, f.native)),
            RadioEvent::ChipReset(cause) => {
                self.chip_resets.add(cause);
                self.phone = None;
            }
            RadioEvent::Ready(_) => self.phone = None,
            RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer, .. } => {
                self.phone = Some(Link { link, peer, encrypted_at: None });
            }
            RadioEvent::LinkDown { link, reason } => {
                let Some(l) = self.phone.filter(|l| l.link == link) else { return };
                let reason = reason.into_inner();
                self.hang_ups.add(reason);
                if l.encrypted_at.is_some_and(|at| now.since(at) <= EARLY_END) {
                    self.early_ends.add(reason);
                }
                self.phone = None;
            }
            RadioEvent::Encryption { link, on } => {
                if let Some(l) = self.phone.as_mut().filter(|l| l.link == link) {
                    l.encrypted_at = on.then_some(now);
                }
            }
            RadioEvent::Pairing(PairingEvent { link, step }) => {
                let Some(l) = self.phone.filter(|l| l.link == link) else { return };
                match step {
                    PairingStep::Complete => self.pairings_ok += 1,
                    PairingStep::Failed | PairingStep::LocalKeyMissing | PairingStep::PeerKeyMissing => {
                        self.pairing_failures.add(step);
                        self.last_pairing_failure = Some((l.peer, step));
                    }
                    PairingStep::Numeric(_) | PairingStep::PasskeyWanted => {}
                }
            }
            _ => {}
        }
    }

    /// `[ble] counts …`, one line.
    ///
    /// # Errors
    ///
    /// Only those of `w`.
    pub fn write_line<W: Write>(&self, w: &mut W) -> fmt::Result {
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
            write!(w, " (last {} from ", step_name(step))?;
            write_peer(w, peer)?;
            w.write_char(')')?;
        }
        write!(w, "; hung up within {} s of encrypting", EARLY_END.0 / 1_000)?;
        self.early_ends.write(w, |w, r| write!(w, "{r:02X}"))?;
        w.write_str("; chip resets")?;
        self.chip_resets.write(w, |w, c| w.write_str(cause_name(c)))?;
        write!(
            w,
            "; operations a minute at most {}; unreachable now {} s, worst {} s; plan drift worst {} s",
            self.commands_peak,
            self.unreachable_now.0 / 1_000,
            self.unreachable_worst.0 / 1_000,
            self.drift_worst.0 / 1_000,
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

fn cause_name(c: ResetCause) -> &'static str {
    match c {
        ResetCause::PowerOn => "power-on",
        ResetCause::Watchdog => "watchdog",
        ResetCause::Lockup => "lockup",
        ResetCause::Brownout => "brownout",
        ResetCause::Crash => "crash",
        ResetCause::Updater => "updater",
        ResetCause::Other => "other",
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
