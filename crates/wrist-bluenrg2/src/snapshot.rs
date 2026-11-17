//! The chip's own account of its links: `ACI_HAL_GET_LINK_STATUS`, `ACI_HAL_GET_ANCHOR_PERIOD`,
//! then `ACI_GAP_GET_SECURITY_LEVEL` for each link, pulled a command at a time.

use bluenrg2::hal::{self, link_status, AnchorPeriod};
use bluenrg2::opcode::{ACI_GAP_GET_SECURITY_LEVEL, ACI_HAL_GET_ANCHOR_PERIOD, ACI_HAL_GET_LINK_STATUS};
use bluenrg2::SecurityLevel;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;
use wrist_ble::{
    AddrKind, Anchor, BdAddr, ConnHandle, Failure, FailureKind, Fault, Instant, LeConnRole, LinkStatus, Micros, PeerAddr, RadioEvent, Snapshot,
    MAX_LINKS,
};

use crate::commands::Command;
use crate::driver::{Clock, Driver, Got, Reply, Unanswered};
use crate::transport::MAX_READ;
use crate::{ops, SLOT};

/// The peer of a link whose connection the driver never saw.
pub const UNSEEN: PeerAddr = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr([0; 6]) };

/// Each link's peer, as its connection event named it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Peers([Option<(ConnHandle, PeerAddr)>; MAX_LINKS]);

impl Peers {
    /// What `event` says of the links' peers.
    pub(crate) fn saw(&mut self, event: &RadioEvent) {
        match *event {
            RadioEvent::LinkUp { link, peer, .. } => {
                self.forget(link);
                if let Some(free) = self.0.iter_mut().find(|p| p.is_none()) {
                    *free = Some((link, peer));
                }
            }
            RadioEvent::LinkDown { link, .. } => self.forget(link),
            RadioEvent::Ready(_) | RadioEvent::ChipReset(_) => *self = Self::default(),
            _ => {}
        }
    }

    fn of(&self, link: ConnHandle) -> Option<PeerAddr> {
        self.0.iter().flatten().find(|(l, _)| *l == link).map(|&(_, peer)| peer)
    }

    fn forget(&mut self, link: ConnHandle) {
        for p in &mut self.0 {
            if p.is_some_and(|(l, _)| l == link) {
                *p = None;
            }
        }
    }

    /// Only the peers of `links`: a link whose end was lost is let go.
    pub(crate) fn keep(&mut self, links: &[Option<LinkStatus>]) {
        for p in &mut self.0 {
            if p.is_some_and(|(l, _)| !links.iter().flatten().any(|k| k.link == l)) {
                *p = None;
            }
        }
    }
}

/// A snapshot under way.
#[derive(Clone, Copy, Debug)]
pub struct Snap {
    stage: Stage,
    snapshot: Snapshot,
    /// The links have been read: what is handed out from here came after the reading, and is
    /// folded into it, so the snapshot taken is the chip as of [`Snapping::Taken`].
    read: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Send(Ask),
    Write(Ask),
    Wait(Ask),
    /// Every answer is in; what the last drain holds is handed out before the snapshot.
    Ending(Result<(), Fault>),
    Over,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    Links,
    Anchor,
    /// The security of the link at this index of the snapshot's links.
    Security(usize),
}

/// What a snapshot hands out next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Snapping {
    Got(Got),
    /// The command about to be sent.
    Sending(Command),
    /// The snapshot is over.
    Taken(Result<Snapshot, Fault>),
}

impl Default for Snap {
    fn default() -> Self {
        let snapshot = Snapshot { at: Instant(0), links: [None; MAX_LINKS], advertising: false, scanning: false, initiating: false, anchor: None };
        Self { stage: Stage::Send(Ask::Links), snapshot, read: false }
    }
}

impl Snap {
    fn command(&self, ask: Ask) -> Option<Command> {
        match ask {
            Ask::Links => Command::new(ACI_HAL_GET_LINK_STATUS, &[]),
            Ask::Anchor => Command::new(ACI_HAL_GET_ANCHOR_PERIOD, &[]),
            Ask::Security(i) => Command::new(ACI_GAP_GET_SECURITY_LEVEL, &[&self.snapshot.links[i]?.link.raw().to_le_bytes()]),
        }
    }

    /// The security of the first link from index `from` on, if any is left.
    fn security_from(&self, from: usize) -> Option<Ask> {
        (from..MAX_LINKS).find(|&i| self.snapshot.links[i].is_some()).map(Ask::Security)
    }

    /// `ask`'s answer, at `now` ms: the stage it leads to.
    fn next(&mut self, ask: Ask, reply: &Result<Reply, Unanswered>, peers: &Peers, now: u64) -> Stage {
        match self.answered(ask, reply, peers, now) {
            Ok(Some(next)) => Stage::Send(next),
            Ok(None) => Stage::Ending(Ok(())),
            Err(f) => Stage::Ending(Err(f)),
        }
    }

    /// What the driver handed out after the reading, as the chip's record would have it.
    fn fold(&mut self, got: &Got) {
        let (true, Got::Packet { event: Some(e), .. } | Got::Event(e)) = (self.read, got) else { return };
        let s = &mut self.snapshot;
        match *e {
            RadioEvent::LinkUp { link, role, peer, .. } => {
                forget(&mut s.links, link);
                if let Some(free) = s.links.iter_mut().find(|l| l.is_none()) {
                    *free = Some(LinkStatus { link, role, peer, encrypted: Some(false) });
                }
                s.advertising &= role != LeConnRole::Peripheral;
            }
            RadioEvent::LinkDown { link, .. } => forget(&mut s.links, link),
            RadioEvent::Encryption { link, on } => {
                if let Some(l) = s.links.iter_mut().flatten().find(|l| l.link == link) {
                    l.encrypted = Some(on);
                }
            }
            // A restart after the reading fails the snapshot.
            _ => {}
        }
    }

    /// `ask`'s answer, at `now` ms: what to ask next, if anything.
    fn answered(&mut self, ask: Ask, reply: &Result<Reply, Unanswered>, peers: &Peers, now: u64) -> Result<Option<Ask>, Fault> {
        let r = reply.as_ref().map_err(|&why| fault(why))?;
        match ask {
            Ask::Links => {
                if r.status != 0 {
                    return Err(Fault::Refused(ops::refusal(r.status)));
                }
                let links = hal::LinkStatus::decode(&r.params[..r.len]).ok_or(Fault::Refused(Failure { kind: FailureKind::Unknown, native: 0 }))?;
                self.taken(&links, peers, now);
                self.read = true;
                Ok(Some(Ask::Anchor))
            }
            Ask::Anchor => {
                self.snapshot.anchor = AnchorPeriod::decode(&r.params[..r.len]).filter(|_| r.status == 0).and_then(anchor);
                Ok(self.security_from(0))
            }
            Ask::Security(i) => {
                if let Some(l) = &mut self.snapshot.links[i] {
                    l.encrypted = SecurityLevel::decode(&r.params[..r.len]).filter(|_| r.status == 0).map(SecurityLevel::is_encrypted);
                }
                Ok(self.security_from(i + 1))
            }
        }
    }

    /// The links and activities `links` shows, each link's peer as `peers` knows it.
    fn taken(&mut self, links: &hal::LinkStatus, peers: &Peers, now: u64) {
        let s = &mut self.snapshot;
        s.at = Instant(now);
        for ((out, &state), &handle) in s.links.iter_mut().zip(&links.link_status).zip(&links.link_connection_handle) {
            let role = match state {
                link_status::PERIPHERAL => LeConnRole::Peripheral,
                link_status::CENTRAL => LeConnRole::Central,
                _ => continue,
            };
            if let Some(link) = bluenrg2::conn_handle(handle) {
                *out = Some(LinkStatus { link, role, peer: peers.of(link).unwrap_or(UNSEEN), encrypted: None });
            }
        }
        s.advertising = links.link_status.contains(&link_status::ADVERTISING);
        s.scanning = links.link_status.contains(&link_status::SCANNING);
        s.initiating = links.link_status.contains(&link_status::INITIATING);
    }
}

fn forget(links: &mut [Option<LinkStatus>; MAX_LINKS], link: ConnHandle) {
    for l in links.iter_mut().filter(|l| l.is_some_and(|l| l.link == link)) {
        *l = None;
    }
}

/// What an unanswered command makes of the snapshot.
fn fault(why: Unanswered) -> Fault {
    match why {
        Unanswered::NotTaken => Fault::Bus,
        Unanswered::NoReply | Unanswered::Restarted => Fault::Silent,
        Unanswered::TooLong => Fault::Refused(ops::unanswered(why)),
    }
}

/// The anchor `ACI_HAL_GET_ANCHOR_PERIOD` read, `None` where it read 0 for both.
fn anchor(a: AnchorPeriod) -> Option<Anchor> {
    let us = |slots: u32| Micros(u32::try_from(u64::from(slots) * SLOT / 1_000).unwrap_or(u32::MAX));
    (a.anchor_period != 0 || a.max_free_slot != 0).then(|| Anchor { period: us(a.anchor_period), max_free_slot: us(a.max_free_slot) })
}

impl<B, C, I, D, R, K> Driver<'_, B, C, I, D, R, K>
where
    B: SpiBus,
    C: OutputPin,
    I: InputPin + Wait,
    D: DelayNs + Clone,
    R: OutputPin,
    K: Clock,
{
    /// The snapshot's next step: what a drain hands out first, then each note, until
    /// [`Snapping::Taken`]; `None` after. Each reply is read at its place among the chip's packets,
    /// so the events after the links' reading are folded into the snapshot.
    pub async fn snapshotting(&mut self, s: &mut Snap, out: &mut [u8; MAX_READ]) -> Option<Snapping> {
        loop {
            if let (Stage::Wait(ask), true) = (s.stage, self.replied()) {
                let reply = self.reply();
                s.stage = s.next(ask, &reply, self.peers(), self.now_ms());
            }
            if let Some(got) = self.next(out).await {
                s.fold(&got);
                return Some(Snapping::Got(got));
            }
            s.stage = match s.stage {
                Stage::Over => return None,
                Stage::Ending(r) => {
                    s.stage = Stage::Over;
                    let taken = match r {
                        Ok(()) if self.restarted() => Err(Fault::Silent),
                        Ok(()) => {
                            self.resynced(&s.snapshot.links);
                            Ok(s.snapshot)
                        }
                        Err(f) => Err(f),
                    };
                    return Some(Snapping::Taken(taken));
                }
                // A link that went down after the reading has no security to read.
                Stage::Send(Ask::Security(i)) if s.snapshot.links[i].is_none() => s.security_from(i + 1).map_or(Stage::Ending(Ok(())), Stage::Send),
                Stage::Send(ask) => match s.command(ask) {
                    Some(c) if !self.restarted() => {
                        s.stage = Stage::Write(ask);
                        return Some(Snapping::Sending(c));
                    }
                    Some(_) => s.next(ask, &Err(Unanswered::Restarted), self.peers(), self.now_ms()),
                    None => s.next(ask, &Err(Unanswered::TooLong), self.peers(), self.now_ms()),
                },
                Stage::Write(ask) => {
                    let sent = match s.command(ask) {
                        Some(c) => self.send(c.opcode, c.params()).await,
                        None => Err(Unanswered::TooLong),
                    };
                    match sent {
                        Ok(()) => Stage::Wait(ask),
                        Err(why) => s.next(ask, &Err(why), self.peers(), self.now_ms()),
                    }
                }
                Stage::Wait(ask) => {
                    if self.awaiting().await {
                        continue;
                    }
                    let reply = self.reply();
                    s.next(ask, &reply, self.peers(), self.now_ms())
                }
            };
        }
    }
}
