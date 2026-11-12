//! The operations the chip has taken: their ids, their deadlines, and what ends each.

use wrist_ble::{ConnHandle, Failure, FailureKind, LinkStatus, Op, OpId, OpKind, Outcome, PairingEvent, RadioEvent, SchedKind, MAX_LINKS};

use crate::driver::Unanswered;

/// The most operations waiting for their end at once.
pub const MAX_PENDING: usize = 8;
/// The most endings waiting to be reported.
const MAX_ENDED: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    id: OpId,
    kind: OpKind,
    link: Option<ConnHandle>,
    deadline: u64,
}

/// The operations taken and not yet ended, and the endings to report, oldest first.
#[derive(Clone, Copy, Debug)]
pub struct Ops {
    pending: [Option<Pending>; MAX_PENDING],
    next: u16,
    ended: [Option<(OpId, Result<(), Failure>)>; MAX_ENDED],
    /// An ending found no room: the next report says events were lost.
    lost: bool,
    /// Links seen going down and not up since, oldest first; let go when events are lost.
    down: [Option<ConnHandle>; MAX_LINKS],
}

impl Default for Ops {
    fn default() -> Self {
        Self { pending: [None; MAX_PENDING], next: 0, ended: [None; MAX_ENDED], lost: false, down: [None; MAX_LINKS] }
    }
}

/// A command's refusal as the core sees it, the chip's code kept (ST's `ble_status.h`).
#[must_use]
pub fn refusal(status: u8) -> Failure {
    let kind = match status {
        0x0C | 0x46 => FailureKind::ModeExclusion,
        0x82 => FailureKind::Schedule(SchedKind::NoSlot),
        0x84 => FailureKind::Schedule(SchedKind::NewIntervalFailed),
        0x85 => FailureKind::Schedule(SchedKind::IntervalTooLarge),
        0x86 => FailureKind::Schedule(SchedKind::LengthFailed),
        0x64 => FailureKind::NoResources,
        0x02 => FailureKind::NotConnected,
        0x3E => FailureKind::NotEstablished,
        _ => FailureKind::Unknown,
    };
    Failure { kind, native: u16::from(status) }
}

/// A command that got no answer, as the core sees it.
#[must_use]
pub fn unanswered(why: Unanswered) -> Failure {
    let kind = match why {
        Unanswered::NoReply => FailureKind::Timeout,
        Unanswered::NotTaken | Unanswered::Restarted => FailureKind::Busy,
        Unanswered::TooLong => FailureKind::Unknown,
    };
    Failure { kind, native: 0 }
}

impl Ops {
    /// `op`, answered with `status`: taken, with its id and `deadline`, or refused. An
    /// operation its answer ends is reported ended at once, and so is one whose link went down
    /// before the answer: a hang-up done, anything else not connected.
    ///
    /// # Errors
    ///
    /// The chip refused it, or more are waiting than fit.
    pub fn answered(&mut self, op: &Op, status: u8, deadline: u64) -> Result<OpId, Failure> {
        if status != 0 {
            return Err(refusal(status));
        }
        let kind = op.kind();
        let id = OpId(self.next);
        let down = op.link().is_some_and(|l| self.down.contains(&Some(l)));
        if kind.ends_with_event() && down {
            self.end(id, if kind == OpKind::Disconnect { Ok(()) } else { Err(Failure { kind: FailureKind::NotConnected, native: 0 }) });
        } else if kind.ends_with_event() {
            let slot = self.pending.iter_mut().find(|p| p.is_none()).ok_or(Failure { kind: FailureKind::NoResources, native: 0 })?;
            *slot = Some(Pending { id, kind, link: op.link(), deadline });
        } else {
            self.end(id, Ok(()));
        }
        self.next = self.next.wrapping_add(1);
        Ok(id)
    }

    /// What `event` ends: an operation it is the end of, or one on a link it takes down; a chip
    /// that restarted holds none.
    pub fn saw(&mut self, event: &RadioEvent) {
        match *event {
            RadioEvent::Ready(_) | RadioEvent::ChipReset(_) => self.pending = [None; MAX_PENDING],
            RadioEvent::EventsLost => self.down = [None; MAX_LINKS],
            RadioEvent::LinkUp { link, .. } => {
                self.settle(|p| p.kind == OpKind::Connect, None);
                if let Some(i) = self.down.iter().position(|d| *d == Some(link)) {
                    self.down.copy_within(i + 1.., i);
                    self.down[MAX_LINKS - 1] = None;
                }
            }
            RadioEvent::LinkDown { link, .. } => {
                self.went_down(link);
                if !self.down.contains(&Some(link)) {
                    if let Some(i) = self.down.iter().position(Option::is_none) {
                        self.down[i] = Some(link);
                    } else {
                        self.down.rotate_left(1);
                        self.down[MAX_LINKS - 1] = Some(link);
                    }
                }
            }
            RadioEvent::Params { link, .. } => self.settle(|p| p.kind == OpKind::UpdateParams && p.link == Some(link), None),
            RadioEvent::Encryption { link, .. } | RadioEvent::Pairing(PairingEvent { link, .. }) => {
                self.settle(|p| p.kind == OpKind::RequestSecurity && p.link == Some(link), None);
            }
            _ => {}
        }
    }

    /// The central took the parameters asked of it on `link`: the oldest ask is done.
    pub fn accepted(&mut self, link: ConnHandle) {
        let next = self.next;
        let asks = self.pending.iter_mut().filter(|p| p.is_some_and(|p| p.kind == OpKind::UpdateParams && p.link == Some(link)));
        if let Some(p) = asks.max_by_key(|p| p.map_or(0, |p| next.wrapping_sub(p.id.0))).and_then(Option::take) {
            self.end(p.id, Ok(()));
        }
    }

    /// `link` went down: its hang-up is over, and what else waited on it fails.
    fn went_down(&mut self, link: ConnHandle) {
        self.settle(|p| p.kind == OpKind::Disconnect && p.link == Some(link), None);
        self.settle(|p| p.link == Some(link), Some(Failure { kind: FailureKind::NotConnected, native: 0 }));
    }

    /// A snapshot found only `links`: an operation on any other link ends as its going down ends it.
    pub fn resynced(&mut self, links: &[Option<LinkStatus>]) {
        let gone = |l: ConnHandle| !links.iter().flatten().any(|k| k.link == l);
        self.settle(|p| p.kind == OpKind::Disconnect && p.link.is_some_and(gone), None);
        self.settle(|p| p.link.is_some_and(gone), Some(Failure { kind: FailureKind::NotConnected, native: 0 }));
    }

    /// Operations past their deadline, ended with a timeout.
    pub fn expire(&mut self, now: u64) {
        while let Some(p) = self.take(|p| now >= p.deadline) {
            self.end(p.id, Err(Failure { kind: FailureKind::Timeout, native: 0 }));
        }
    }

    /// The next ending to report.
    pub fn ended(&mut self) -> Option<RadioEvent> {
        if core::mem::take(&mut self.lost) {
            return Some(RadioEvent::EventsLost);
        }
        let (id, r) = self.ended[0].take()?;
        self.ended.rotate_left(1);
        Some(RadioEvent::Done(id, r.map(|()| Outcome::Done)))
    }

    /// The earliest deadline of those waiting.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        self.pending.iter().flatten().map(|p| p.deadline).min()
    }

    /// Every pending operation `which` picks ends: by the event, or with `failure`.
    fn settle(&mut self, which: impl Fn(&Pending) -> bool, failure: Option<Failure>) {
        while let Some(p) = self.take(&which) {
            if let Some(f) = failure {
                self.end(p.id, Err(f));
            }
        }
    }

    fn take(&mut self, which: impl Fn(&Pending) -> bool) -> Option<Pending> {
        self.pending.iter_mut().find(|p| p.as_ref().is_some_and(&which))?.take()
    }

    fn end(&mut self, id: OpId, r: Result<(), Failure>) {
        match self.ended.iter_mut().find(|e| e.is_none()) {
            Some(slot) => *slot = Some((id, r)),
            None => self.lost = true,
        }
    }
}

