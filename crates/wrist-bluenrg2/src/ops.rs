//! The operations the chip has taken: their ids, their deadlines, and what ends each.

use wrist_ble::{ConnHandle, Failure, FailureKind, Op, OpId, OpKind, Outcome, PairingEvent, RadioEvent, SchedKind};

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
}

impl Default for Ops {
    fn default() -> Self {
        Self { pending: [None; MAX_PENDING], next: 0, ended: [None; MAX_ENDED], lost: false }
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
    /// operation its answer ends is reported ended at once.
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
        if kind.ends_with_event() {
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
            RadioEvent::LinkUp { .. } => self.settle(|p| p.kind == OpKind::Connect, None),
            RadioEvent::LinkDown { link, .. } => {
                self.settle(|p| p.kind == OpKind::Disconnect && p.link == Some(link), None);
                self.settle(|p| p.link == Some(link), Some(Failure { kind: FailureKind::NotConnected, native: 0 }));
            }
            RadioEvent::Params { link, .. } => self.settle(|p| p.kind == OpKind::UpdateParams && p.link == Some(link), None),
            RadioEvent::Encryption { link, .. } | RadioEvent::Pairing(PairingEvent { link, .. }) => {
                self.settle(|p| p.kind == OpKind::RequestSecurity && p.link == Some(link), None);
            }
            _ => {}
        }
    }

    /// The central took the parameters asked of it on `link`: the ask is done.
    pub fn accepted(&mut self, link: ConnHandle) {
        if let Some(p) = self.take(|p| p.kind == OpKind::UpdateParams && p.link == Some(link)) {
            self.end(p.id, Ok(()));
        }
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

