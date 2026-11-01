//! The core in one step: inputs in, at most one operation out, with the time it must next run.

use wrist_ble::{
    Activities, Capabilities, Change, ChipInfo, ConnHandle, ConnParams, DisconnectReason, Failure, FailureKind, Fault, Instant, LeConnRole,
    LinkParams, Millis, Op, OpId, OpKind, RadioEvent, Schedule, Snapshot, Status,
};

use crate::chip::{self, Chip};
use crate::goals::{Goals, Power, MAX_PERIPHERALS};
use crate::invariants::REACHABLE_GAP;
use crate::peripheral::{self, Ctx, Peripheral, Stage, StopReason, Then};
use crate::phone::{self, Phone, Security, ASK_TO_PAIR_AFTER};
use crate::plan::{fingerprint, Menu, MenuPlanner, Pending, PlanHealth, PlanInput, Planner, Refused, Target, Verdict};
use crate::{Peer, State};

#[derive(Clone, Copy, Debug)]
pub enum Input {
    Radio(RadioEvent),
    Goals(Goals),
    /// The radio's answer to the operation the last step asked for.
    Submitted(Result<OpId, Failure>),
    /// The chip's bring-up, which the last step asked for, ended.
    Started(Result<ChipInfo, Fault>),
    /// The snapshot the last step asked for.
    Snapshot(Result<Snapshot, Fault>),
    /// The user's answer to a numeric comparison.
    Answer(bool),
    /// The user asked to try a stopped peripheral again.
    Retry(usize),
    /// The user asked for the radio again after a fault.
    RetryRadio,
    Tick,
}

/// What to tell the user or the log, beyond the status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tell {
    /// The phone has lost its bond with this device.
    BondLost(ConnHandle),
    /// The chip refused what its model admitted; the configuration is not tried again.
    Divergence { kind: OpKind, native: u16 },
    /// A peer's request for new parameters was refused.
    RefusedPeerRequest(ConnHandle),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outputs {
    /// Submit this, and answer with [`Input::Submitted`].
    pub op: Option<Op>,
    /// Bring the chip up (answer with [`Input::Started`]) or hold it in reset.
    pub chip: Option<chip::Action>,
    /// Take a snapshot, and answer with [`Input::Snapshot`].
    pub snapshot: bool,
    pub tell: [Option<Tell>; 2],
    /// Step again with [`Input::Tick`] by then, if nothing else comes first.
    pub next: Option<Instant>,
}

impl Outputs {
    fn tell(&mut self, t: Tell) {
        if let Some(slot) = self.tell.iter_mut().find(|s| s.is_none()) {
            *slot = Some(t);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub submitted: u32,
    pub refused: u32,
    pub divergences: u32,
    pub failed: u32,
    pub timed_out: u32,
    pub unknown_events: u32,
    pub resets: u32,
    pub update_failures: u32,
    pub audit_mismatches: u32,
    pub mic_failures: u32,
    pub instant_passed: u32,
    pub refused_peer_requests: u32,
}

/// Waits before an operation kind that failed is tried again: 1 s, doubling to 32 s.
#[derive(Clone, Copy, Debug, Default)]
struct Backoff {
    failures: u8,
    until: Option<Instant>,
}

const KINDS: usize = 17;

fn kind_index(k: OpKind) -> usize {
    k as usize
}

/// Ops the machines asked for, issued in order as the radio frees up.
const QUEUE: usize = 4;

pub struct Core<'a> {
    pub state: State,
    pub counters: Counters,
    goals: Goals,
    caps: &'a Capabilities,
    schedule: &'a dyn Schedule,
    menu: &'a Menu,
    refused: Refused,
    target: Target,
    /// Asked for, and not yet answered by [`Input::Submitted`].
    asked: Option<Op>,
    pending: Option<Pending>,
    queue: [Option<Op>; QUEUE],
    backoff: [Backoff; KINDS],
    /// The phone link's parameters, and what this device last asked of it.
    phone_params: Option<LinkParams>,
    phone_asked: Option<ConnParams>,
    retry: [bool; MAX_PERIPHERALS],
    snapshot: bool,
    /// When goals or power last changed, or the chip came up.
    pub settled_since: Instant,
}

impl<'a> Core<'a> {
    #[must_use]
    pub fn new(caps: &'a Capabilities, schedule: &'a dyn Schedule, menu: &'a Menu) -> Self {
        Self {
            state: State::OFF,
            counters: Counters::default(),
            goals: Goals::OFF,
            caps,
            schedule,
            menu,
            refused: Refused::EMPTY,
            target: Target { activities: Activities::IDLE, phone: Verdict::NotWanted, peripherals: [Verdict::NotWanted; MAX_PERIPHERALS] },
            asked: None,
            pending: None,
            queue: [None; QUEUE],
            backoff: [Backoff::default(); KINDS],
            phone_params: None,
            phone_asked: None,
            retry: [false; MAX_PERIPHERALS],
            snapshot: false,
            settled_since: Instant(0),
        }
    }

    /// What the planner last asked the radio for.
    #[must_use]
    pub fn target(&self) -> &Target {
        &self.target
    }

    #[must_use]
    pub fn goals(&self) -> &Goals {
        &self.goals
    }

    /// The operation in flight.
    #[must_use]
    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    pub fn step(&mut self, now: Instant, input: Input) -> Outputs {
        let mut out = Outputs::default();
        match input {
            Input::Radio(e) => self.event(now, e, &mut out),
            Input::Goals(g) => self.set_goals(now, g, &mut out),
            Input::Submitted(r) => self.submitted(now, r, &mut out),
            Input::Started(Ok(info)) => self.ready(now, info),
            Input::Started(Err(_)) => self.chip(now, chip::Event::StartFailed, &mut out),
            Input::Snapshot(r) => self.resync(now, r),
            Input::Answer(yes) => self.phone(now, phone::Event::Answer(yes), &mut out),
            Input::Retry(i) => {
                if let Some(r) = self.retry.get_mut(i) {
                    *r = true;
                }
            }
            Input::RetryRadio => self.chip(now, chip::Event::Retry, &mut out),
            Input::Tick => {
                self.chip(now, chip::Event::Tick, &mut out);
                self.phone(now, phone::Event::Tick, &mut out);
                self.peers(now, peripheral::Event::Tick);
                self.expire(now);
            }
        }
        self.reconcile(now, &mut out);
        out.next = self.deadline(now);
        out
    }

    fn set_goals(&mut self, now: Instant, g: Goals, out: &mut Outputs) {
        let was = self.goals;
        self.goals = g;
        if was.power != g.power || was.transfer != g.transfer || was.phone != g.phone || was.radio_on != g.radio_on {
            self.settled_since = now;
        }
        if g.radio_on && !was.radio_on {
            self.chip(now, chip::Event::RadioOn, out);
        } else if !g.radio_on && was.radio_on {
            self.chip(now, chip::Event::RadioOff, out);
            self.phone(now, phone::Event::RadioOff, out);
            self.forget_radio(now);
        }
        for i in 0..MAX_PERIPHERALS {
            let same = match (self.state.peers[i], g.peripherals[i]) {
                (Some(p), Some(w)) => p.want.peer == w.peer && core::ptr::eq(p.want.profile, w.profile),
                (None, None) => true,
                _ => false,
            };
            if same {
                if let (Some(p), Some(w)) = (self.state.peers[i].as_mut(), g.peripherals[i]) {
                    p.want = w;
                }
                continue;
            }
            if let Some(p) = self.state.peers[i] {
                self.peer_step(now, i, peripheral::Event::Unwant);
                if !matches!(p.machine.stage, Stage::Linked { .. } | Stage::Live { .. } | Stage::Dropping { .. }) {
                    self.state.peers[i] = None;
                }
            }
            if self.state.peers[i].is_none() {
                if let Some(w) = g.peripherals[i] {
                    self.state.peers[i] = Some(Peer { want: w, machine: Peripheral::default() });
                    self.peer_step(now, i, peripheral::Event::Want);
                }
            }
        }
    }

    fn chip(&mut self, now: Instant, e: chip::Event, out: &mut Outputs) {
        let (next, action) = self.state.chip.step(now, e);
        self.state.chip = next;
        if let Some(a) = action {
            if a == chip::Action::Start {
                self.counters.resets += u32::from(matches!(e, chip::Event::Reset(_) | chip::Event::BaselineRefused | chip::Event::StartFailed));
            }
            self.forget_radio(now);
            out.chip = Some(a);
        }
    }

    fn ready(&mut self, now: Instant, info: ChipInfo) {
        if !matches!(self.state.chip.state, Chip::Starting { .. }) {
            return;
        }
        self.state.chip = self.state.chip.step(now, chip::Event::Ready(info)).0;
        self.forget_radio(now);
        self.state.phone = self.state.phone.step(now, phone::Event::RadioOn).0;
        self.settled_since = now;
    }

    /// Every link, operation and refusal goes with the chip.
    fn forget_radio(&mut self, now: Instant) {
        if let Some(link) = self.state.phone.link() {
            self.state.phone = self.state.phone.step(now, phone::Event::LinkDown(link)).0;
        }
        for i in 0..MAX_PERIPHERALS {
            if let Some(link) = self.state.peers[i].and_then(|p| linked(&p.machine)) {
                self.peer_step(now, i, peripheral::Event::LinkDown(link, Status::HARDWARE_FAILURE));
            }
        }
        self.state.radio = Activities::IDLE;
        self.state.silent_since = now;
        self.refused.clear();
        self.asked = None;
        self.pending = None;
        self.queue = [None; QUEUE];
        self.backoff = [Backoff::default(); KINDS];
        self.phone_params = None;
        self.phone_asked = None;
    }

    fn phone(&mut self, now: Instant, e: phone::Event, out: &mut Outputs) {
        let (next, actions) = self.state.phone.step(now, e);
        self.state.phone = next;
        for a in actions.iter() {
            match a {
                phone::Action::RequestSecurity(l) => self.enqueue(Op::RequestSecurity(l)),
                phone::Action::ConfirmNumeric(l, yes) => self.enqueue(Op::ConfirmNumeric(l, yes)),
                phone::Action::AllowRebond(l) => self.enqueue(Op::AllowRebond(l)),
                phone::Action::TellBondLost(l) => out.tell(Tell::BondLost(l)),
            }
        }
    }

    fn enqueue(&mut self, op: Op) {
        if let Some(slot) = self.queue.iter_mut().find(|s| s.is_none()) {
            *slot = Some(op);
        }
    }

    fn event(&mut self, now: Instant, e: RadioEvent, out: &mut Outputs) {
        match e {
            RadioEvent::Ready(info) => self.ready(now, info),
            RadioEvent::ChipReset(cause) => {
                self.chip(now, chip::Event::Reset(cause), out);
                self.forget_radio(now);
            }
            RadioEvent::Done(id, r) => self.done(now, id, r),
            RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, params, .. } => {
                if self.state.phone.link().is_none() {
                    self.phone(now, phone::Event::LinkUp(link), out);
                    self.phone_params = Some(params);
                    self.phone_asked = None;
                }
                self.state.radio.peripheral = self.state.radio.peripheral.saturating_add(1);
                self.state.radio.advertising = None;
            }
            RadioEvent::LinkUp { link, peer, .. } => {
                self.end_pending(|op| op.kind() == OpKind::Connect);
                if let Some(i) = self.state.peers.iter().position(|p| p.is_some_and(|p| p.want.peer == peer)) {
                    self.peer_step(now, i, peripheral::Event::LinkUp(link));
                }
            }
            RadioEvent::LinkDown { link, reason } => {
                self.end_pending(|op| op.link() == Some(link));
                if reason == Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE {
                    self.counters.mic_failures += 1;
                }
                if reason == Status::INSTANT_PASSED {
                    self.counters.instant_passed += 1;
                }
                if self.state.phone.link() == Some(link) {
                    self.phone(now, phone::Event::LinkDown(link), out);
                    self.state.radio.peripheral = self.state.radio.peripheral.saturating_sub(1);
                    self.state.silent_since = now;
                    self.phone_params = None;
                    self.phone_asked = None;
                }
                for i in 0..MAX_PERIPHERALS {
                    if self.state.peers[i].and_then(|p| linked(&p.machine)) == Some(link) {
                        self.peer_step(now, i, peripheral::Event::LinkDown(link, reason));
                    }
                }
            }
            RadioEvent::Params { link, result } => {
                self.end_pending(|op| op.kind() == OpKind::UpdateParams && op.link() == Some(link));
                match result {
                    Ok(p) if self.state.phone.link() == Some(link) => {
                        self.phone_params = Some(p);
                        self.phone(now, phone::Event::ParamsChanged(link), out);
                    }
                    Ok(_) => {}
                    Err(_) => self.counters.update_failures += 1,
                }
            }
            RadioEvent::Encryption { link, on } => {
                self.end_pending(|op| op.kind() == OpKind::RequestSecurity && op.link() == Some(link));
                self.phone(now, phone::Event::Encryption(link, on), out);
            }
            RadioEvent::Pairing(p) => {
                self.end_pending(|op| op.kind() == OpKind::RequestSecurity && op.link() == Some(p.link));
                self.phone(now, phone::Event::Pairing(p.link, p.step), out);
            }
            RadioEvent::BondLost(link) => self.phone(now, phone::Event::BondLost(link), out),
            RadioEvent::PeerParamRequest { link, .. } => {
                self.enqueue(Op::AnswerParamRequest(link, false));
                self.counters.refused_peer_requests += 1;
                out.tell(Tell::RefusedPeerRequest(link));
            }
            RadioEvent::Notification { link, data, .. } => {
                for i in 0..MAX_PERIPHERALS {
                    let Some(p) = self.state.peers[i] else { continue };
                    if matches!(p.machine.stage, Stage::Live { link: l, .. } if l == link) {
                        if let Some(r) = (p.want.profile.parse)(data.as_slice()) {
                            self.peer_step(now, i, peripheral::Event::Reading(r));
                        }
                    }
                }
            }
            RadioEvent::ScanDone => self.state.radio.scanning = None,
            RadioEvent::EventsLost => self.snapshot = true,
            RadioEvent::Unknown { .. } => self.counters.unknown_events += 1,
            RadioEvent::Mtu(..) | RadioEvent::DataLength { .. } | RadioEvent::ServerWrite { .. } | RadioEvent::ServerSubscribe { .. } | RadioEvent::Report(_) => {}
        }
    }

    /// The pending operation, if `ends` says this event is its outcome.
    fn end_pending(&mut self, ends: impl Fn(&Op) -> bool) {
        if self.pending.is_some_and(|p| ends(&p.op)) {
            self.pending = None;
        }
    }

    fn done(&mut self, now: Instant, id: OpId, r: Result<wrist_ble::Outcome, Failure>) {
        let Some(p) = self.pending.filter(|p| p.id == id) else { return };
        self.pending = None;
        match r {
            Ok(_) => {
                self.backoff[kind_index(p.op.kind())] = Backoff::default();
                if let Op::Advertise(a) = p.op {
                    self.set_advertising(now, a);
                }
            }
            Err(f) => {
                self.counters.failed += 1;
                if f.kind == FailureKind::Timeout {
                    self.counters.timed_out += 1;
                }
                self.back_off(now, p.op.kind());
            }
        }
    }

    fn set_advertising(&mut self, now: Instant, a: Option<wrist_ble::AdvParams>) {
        let was = self.state.radio.advertising.is_some_and(|a| a.connectable);
        self.state.radio.advertising = a;
        if was && !a.is_some_and(|a| a.connectable) {
            self.state.silent_since = now;
        }
    }

    fn submitted(&mut self, now: Instant, r: Result<OpId, Failure>, out: &mut Outputs) {
        let Some(op) = self.asked.take() else { return };
        match r {
            Ok(id) => {
                let deadline = now + (self.caps.op_deadline)(op.kind());
                self.pending = Some(Pending { id, op, deadline });
                if matches!(op, Op::Advertise(Some(_))) {
                    self.chip(now, chip::Event::Accepted, out);
                }
            }
            Err(f) => {
                self.counters.refused += 1;
                let change = change_of(&op);
                match (f.kind, change) {
                    (FailureKind::Schedule(_) | FailureKind::ModeExclusion, Some(change)) => {
                        self.counters.divergences += 1;
                        out.tell(Tell::Divergence { kind: op.kind(), native: f.native });
                        if !self.refused.insert(fingerprint(&self.state.radio, &change)) {
                            self.chip(now, chip::Event::BaselineRefused, out);
                        }
                    }
                    _ => self.back_off(now, op.kind()),
                }
            }
        }
    }

    fn back_off(&mut self, now: Instant, kind: OpKind) {
        let b = &mut self.backoff[kind_index(kind)];
        let wait = Millis(1_000 << b.failures.min(5));
        b.failures = b.failures.saturating_add(1);
        b.until = Some(now + wait);
    }

    fn resync(&mut self, now: Instant, r: Result<Snapshot, Fault>) {
        let Ok(s) = r else { return };
        let mut mismatch = false;
        let phone = s.links.iter().flatten().find(|l| l.role == LeConnRole::Peripheral).map(|l| l.link);
        if phone != self.state.phone.link() {
            mismatch = true;
            if let Some(l) = self.state.phone.link() {
                self.state.phone = self.state.phone.step(now, phone::Event::LinkDown(l)).0;
                self.state.silent_since = now;
            }
            if let Some(l) = phone {
                self.state.phone = self.state.phone.step(now, phone::Event::LinkUp(l)).0;
            }
        }
        self.state.radio.peripheral = u8::from(phone.is_some());
        let gone = |l: ConnHandle| !s.links.iter().flatten().any(|k| k.link == l);
        self.end_pending(|op| op.link().is_some_and(gone));
        if let Some(Pending { op: Op::Advertise(a), .. }) = self.pending.filter(|p| s.advertising == matches!(p.op, Op::Advertise(Some(_)))) {
            self.pending = None;
            self.set_advertising(now, a);
        }
        if s.advertising != self.state.radio.advertising.is_some() {
            mismatch = true;
            self.set_advertising(now, None);
        }
        if !s.scanning {
            self.state.radio.scanning = None;
        }
        self.counters.audit_mismatches += u32::from(mismatch);
    }

    /// An operation the driver let pass its deadline is ended here.
    fn expire(&mut self, now: Instant) {
        if let Some(p) = self.pending.filter(|p| now >= p.deadline + GRACE) {
            self.pending = None;
            self.counters.timed_out += 1;
            self.back_off(now, p.op.kind());
        }
    }

    fn peers(&mut self, now: Instant, e: peripheral::Event) {
        for i in 0..MAX_PERIPHERALS {
            if self.state.peers[i].is_some() {
                self.peer_step(now, i, e);
            }
        }
    }

    fn peer_step(&mut self, now: Instant, i: usize, e: peripheral::Event) {
        let goals = self.goals;
        let Some(p) = self.state.peers[i].as_mut() else { return };
        let ctx = Ctx { profile: p.want.profile, peer: p.want.peer, power: goals.power, workout: goals.workout };
        p.machine = p.machine.step(&ctx, now, e);
        if p.machine.stage == Stage::Off && goals.peripherals[i].is_none() {
            self.state.peers[i] = None;
        }
    }

    fn plan(&mut self, now: Instant) {
        let input = PlanInput { goals: &self.goals, state: &self.state, caps: self.caps, schedule: self.schedule, menu: self.menu, refused: &self.refused, now };
        self.target = MenuPlanner.plan(&input);
        self.state.phone_verdict = self.target.phone;
        self.state.plan = match self.target.phone {
            Verdict::Busy(c) => PlanHealth::Infeasible(c),
            _ => PlanHealth::Ok,
        };
    }

    fn reconcile(&mut self, now: Instant, out: &mut Outputs) {
        if self.snapshot && matches!(self.state.chip.state, Chip::Ready(_)) {
            self.snapshot = false;
            out.snapshot = true;
        }
        self.plan(now);
        if matches!(self.state.chip.state, Chip::Ready(_)) && self.target.phone == Verdict::Busy(crate::plan::Conflict::Refused) {
            self.chip(now, chip::Event::BaselineRefused, out);
            self.plan(now);
        }
        // The planner's verdicts reach each peripheral, every step.
        for i in 0..MAX_PERIPHERALS {
            let Some(p) = self.state.peers[i] else { continue };
            match (self.target.peripherals[i], p.machine.stage) {
                (Verdict::Busy(c), s) if !matches!(s, Stage::Stopped(StopReason::RadioBusy(_)) | Stage::Dropping { .. }) => {
                    self.peer_step(now, i, peripheral::Event::Busy(c));
                }
                (Verdict::Fits, Stage::Stopped(StopReason::RadioBusy(_))) => self.peer_step(now, i, peripheral::Event::Free),
                _ => {}
            }
            if self.retry[i] {
                match self.state.peers[i].map(|p| p.machine.stage) {
                    Some(Stage::Stopped(_)) => {
                        self.retry[i] = false;
                        self.peer_step(now, i, peripheral::Event::Retry);
                    }
                    Some(Stage::Dropping { then: Then::Stop(_), .. }) => {}
                    _ => self.retry[i] = false,
                }
            }
        }
        let drift = self.target.activities.advertising != self.state.radio.advertising;
        self.state.drift_since = match (drift, self.state.drift_since) {
            (true, None) => Some(now),
            (true, since) => since,
            (false, _) => None,
        };
        if self.asked.is_some() || self.pending.is_some() || !matches!(self.state.chip.state, Chip::Ready(_)) {
            return;
        }
        if let Some(op) = self.next_op(now) {
            self.counters.submitted += 1;
            self.asked = Some(op);
            out.op = Some(op);
        }
    }

    fn ready_for(&self, now: Instant, kind: OpKind) -> bool {
        self.backoff[kind_index(kind)].until.is_none_or(|t| now >= t)
    }

    fn next_op(&mut self, now: Instant) -> Option<Op> {
        if let Some(i) = self.queue.iter().position(Option::is_some) {
            let op = self.queue[i].take();
            self.queue[i..].rotate_left(1);
            return op;
        }
        for p in self.state.peers.iter().flatten() {
            if let Stage::Dropping { link, .. } = p.machine.stage {
                if self.ready_for(now, OpKind::Disconnect) {
                    return Some(Op::Disconnect(link, DisconnectReason::RemoteUserTerminatedConn));
                }
            }
        }
        let want = self.target.activities.advertising;
        if want != self.state.radio.advertising && self.ready_for(now, OpKind::Advertise) {
            return Some(Op::Advertise(want));
        }
        self.phone_ask(now)
    }

    /// Ask the phone for the menu's parameters once a link, if its own do not already fit.
    fn phone_ask(&mut self, now: Instant) -> Option<Op> {
        let link = self.state.phone.link()?;
        if !self.state.phone.may_ask_params(now) || !self.ready_for(now, OpKind::UpdateParams) {
            return None;
        }
        if matches!(self.state.phone, Phone::Up(l) if matches!(l.security, Security::Pairing { .. })) {
            return None;
        }
        let power = if self.goals.transfer { Power::Cable } else { self.goals.power };
        let want = self.menu.phone.iter().find(|c| c.when.power.is_none_or(|p| p == power))?.params;
        if self.phone_asked == Some(want) {
            return None;
        }
        self.phone_asked = Some(want);
        let fits = self.phone_params.is_some_and(|p| (want.interval_min.as_u16()..=want.interval_max.as_u16()).contains(&p.interval.as_u16()) && p.latency == want.latency);
        (!fits).then_some(Op::UpdateParams(link, want))
    }

    fn deadline(&self, now: Instant) -> Option<Instant> {
        let goals = &self.goals;
        let peers = self.state.peers.iter().flatten().filter_map(|p| {
            let ctx = Ctx { profile: p.want.profile, peer: p.want.peer, power: goals.power, workout: goals.workout };
            p.machine.deadline(&ctx, now)
        });
        let backoff = self.backoff.iter().filter_map(|b| b.until).filter(|&t| t > now);
        let pending = self.pending.map(|p| p.deadline + GRACE);
        let marks = [self.state.chip.deadline(), self.state.phone.deadline(), pending, self.phone_mark(now)];
        marks.into_iter().flatten().chain(peers).chain(backoff).min()
    }

    /// When the phone's status next changes by time alone.
    fn phone_mark(&self, now: Instant) -> Option<Instant> {
        if !matches!(self.state.chip.state, Chip::Ready(_)) {
            return None;
        }
        match self.state.phone {
            Phone::Up(l) => match l.security {
                Security::Unsecured => Some(l.since + ASK_TO_PAIR_AFTER + Millis(1)).filter(|&t| t > now),
                Security::Pairing { deadline, .. } => {
                    let left = deadline.since(now).0;
                    (left > 0).then(|| deadline - Millis((left.div_ceil(1_000) - 1) * 1_000))
                }
                Security::Secure => None,
            },
            Phone::Discoverable | Phone::Off if !self.state.radio.advertising.is_some_and(|a| a.connectable) => {
                Some(self.state.silent_since + REACHABLE_GAP + Millis(1)).filter(|&t| t > now)
            }
            _ => None,
        }
    }
}

/// Past an operation's deadline, the driver has this long to report its timeout.
const GRACE: Millis = Millis(1_000);

fn linked(p: &Peripheral) -> Option<ConnHandle> {
    match p.stage {
        Stage::Linked { link, .. } | Stage::Live { link, .. } | Stage::Dropping { link, .. } => Some(link),
        _ => None,
    }
}

/// The scheduler's view of an operation, for those its model admits.
fn change_of(op: &Op) -> Option<Change> {
    match *op {
        Op::Advertise(p) => Some(Change::Advertise(p)),
        Op::Scan(p) => Some(Change::Scan(p)),
        Op::Connect(_, p) => Some(Change::Initiate(Some(p))),
        _ => None,
    }
}
