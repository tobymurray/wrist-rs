#![no_std]

//! The Bluetooth core fed what the BlueNRG-2 driver sends and the chip answers, its operations
//! taken as the driver's and done at once, each set beside what the driver did ([`verdict`]).

pub mod menu;
pub mod verdict;

use bluenrg2::opcode;
use wrist_ble::{
    AddrKind, BdAddr, ChipInfo, ConnHandle, Instant, LeConnRole, LinkStatus, Op, OpId, OpKind, Outcome, PeerAddr, RadioEvent, Snapshot,
    MAX_LINKS,
};
use wrist_ble_core::chip::Action;
use wrist_ble_core::engine::{Core, Input, Tell};
use wrist_ble_core::goals::{Goals, Power};
use wrist_ble_core::State;
use wrist_bluenrg2::events::Translator;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};
use wrist_wake::ble as wake;

use crate::menu::{interval_now_ask, MENU};
use crate::verdict::{Act, Disagreement, Verdict};

/// A driver command that is one of the core's operations, with what the comparison reads of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverOp {
    /// Interval min and max, in 0.625 ms.
    Advertise(u16, u16),
    StopAdvertising,
    /// Interval min and max in 1.25 ms, and latency.
    AskParams(u16, u16, u16),
    /// The answer to a numeric comparison.
    Confirm(bool),
    Other(OpKind),
}

impl DriverOp {
    #[must_use]
    pub fn kind(self) -> OpKind {
        match self {
            Self::Advertise(..) | Self::StopAdvertising => OpKind::Advertise,
            Self::AskParams(..) => OpKind::UpdateParams,
            Self::Confirm(_) => OpKind::ConfirmNumeric,
            Self::Other(k) => k,
        }
    }
}

/// The driver's command as the operation it carries out, for the commands that are one.
fn driver_op(opcode: u16) -> Option<(OpKind, &'static str)> {
    Some(match opcode {
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

/// The chip's report that the central accepted the watch's parameter request.
fn accepted_update(packet: &[u8]) -> bool {
    matches!(bluenrg2::parse(packet), bluenrg2::Event::Vendor(bluenrg2::VendorEvent::L2capUpdateResponse { accepted: true, .. }))
}

/// Told what the shadow does, for a caller that keeps a record of it.
pub trait Observer {
    /// A packet from the chip, as the core is told it.
    fn event(&mut self, _at: Instant, _e: &RadioEvent) {}
    /// A command of the driver's that is one of the core's operations: what it is called, and
    /// its parameters.
    fn driver(&mut self, _at: Instant, _op: DriverOp, _name: &'static str, _params: &[u8]) {}
    /// An operation the core asked for; `seeding` while it catches up with a chip already up,
    /// and then not compared.
    fn op(&mut self, _at: Instant, _op: &Op, _seeding: bool) {}
    fn chip(&mut self, _at: Instant, _action: Action) {}
    fn snapshot(&mut self, _s: &Snapshot) {}
    fn tell(&mut self, _at: Instant, _tell: Tell) {}
    /// After each of the core's steps.
    fn stepped(&mut self, _at: Instant, _state: &State) {}
    fn disagreed(&mut self, _d: Disagreement) {}
    /// An answer the core was owed that found no room, so was never given.
    fn lost(&mut self, _at: Instant) {}
}

impl Observer for () {}

/// Work the core's last step left: each answer it is owed, in the order it is owed them.
#[derive(Clone, Copy)]
enum Owed {
    Submitted(OpId),
    Done(OpId),
    /// The snapshot it asked for, taken when this is reached.
    Snapshot,
    /// Its status once everything its step led to is done.
    Stepped,
}

/// Answers owed at once, a step's nesting inside the last as each operation leads to the next.
const OWED: usize = 32;

/// Ticks given at once, so that a core that kept asking for them could not hold its caller.
const TICKS: usize = 1024;

pub struct Shadow<O> {
    core: Core<'static>,
    translator: Translator,
    info: ChipInfo,
    goals: Goals,
    now: Instant,
    next: Option<Instant>,
    ids: u16,
    phone: Option<ConnHandle>,
    /// The core's parameter request on air, which the central's answer ends.
    asked_update: Option<OpId>,
    advertising: bool,
    /// The core is being told of a chip that was up before it was.
    seeding: bool,
    verdict: Verdict,
    pub observer: O,
}

impl<O: Observer> Shadow<O> {
    /// A core told nothing yet, at the clock's start, that will want `goals` once told them.
    #[must_use]
    pub fn new(info: ChipInfo, goals: Goals, observer: O) -> Self {
        Self {
            core: Core::new(&CAPABILITIES, &Scheduler, &MENU),
            translator: Translator::new(info),
            info,
            goals,
            now: Instant(0),
            next: None,
            ids: 0,
            phone: None,
            asked_update: None,
            advertising: false,
            seeding: false,
            verdict: Verdict::new(),
            observer,
        }
    }

    #[must_use]
    pub fn now(&self) -> Instant {
        self.now
    }

    /// When [`Self::advance`] next has work: a tick the core is owed, or an action to judge.
    #[must_use]
    pub fn next(&self) -> Option<Instant> {
        [self.next, self.verdict.next().map(Instant)].into_iter().flatten().min()
    }

    #[must_use]
    pub fn state(&self) -> &State {
        &self.core.state
    }

    #[must_use]
    pub fn goals(&self) -> Goals {
        self.goals
    }

    /// The ticks owed before `at`, then the clock at `at`, judging what can no longer agree.
    pub fn advance(&mut self, at: Instant) {
        for _ in 0..TICKS {
            let Some(n) = self.next.filter(|&n| n <= at) else { break };
            self.now = Instant(self.now.0.max(n.0));
            self.next = None;
            self.feed(Input::Tick);
        }
        self.now = Instant(self.now.0.max(at.0));
        let Self { verdict, observer, .. } = self;
        verdict.advance(self.now.0, &mut |d| observer.disagreed(d));
    }

    /// Everything still waiting for the other side judged, as at the end of a record.
    pub fn finish(&mut self) {
        let Self { verdict, observer, .. } = self;
        verdict.finish(&mut |d| observer.disagreed(d));
    }

    /// What the core works toward from `at`.
    pub fn set_goals(&mut self, at: Instant, goals: Goals) {
        self.advance(at);
        self.goals = goals;
        self.feed(Input::Goals(goals));
    }

    /// `ble on` or `ble off`, which ends the console's ask with its link.
    pub fn radio(&mut self, at: Instant, on: bool) {
        let phone_params = if on { self.goals.phone_params } else { None };
        self.set_goals(at, Goals { radio_on: on, phone_params, ..self.goals });
    }

    /// The timing the power source and a transfer ask for; where that timing changes, the
    /// console's ask ends.
    pub fn timing(&mut self, at: Instant, power: Power, transfer: bool) {
        if (power, transfer) == (self.goals.power, self.goals.transfer) {
            return;
        }
        let asks = |p: Power, t: bool| wake::timing(if p == Power::Cable { wake::Power::Cable } else { wake::Power::Battery }, t);
        let phone_params = if asks(power, transfer) == asks(self.goals.power, self.goals.transfer) { self.goals.phone_params } else { None };
        self.set_goals(at, Goals { power, transfer, phone_params, ..self.goals });
    }

    /// `ble interval now <min> <max>`, in 1.25 ms, for the phone's link while it is up.
    pub fn interval_now(&mut self, at: Instant, min: u16, max: u16) {
        if self.phone.is_some() {
            self.set_goals(at, Goals { phone_params: Some(interval_now_ask(min, max)), ..self.goals });
        }
    }

    /// `ble yes` or `ble no`.
    pub fn answer(&mut self, at: Instant, yes: bool) {
        self.advance(at);
        self.feed(Input::Answer(yes));
    }

    /// `ble disconnect`, which the core has no goal for.
    pub fn operator_disconnect(&mut self, at: Instant) {
        self.advance(at);
        let Self { verdict, observer, .. } = self;
        verdict.operator(self.now.0, &mut |d| observer.disagreed(d));
    }

    /// The core's start answered at once by the chip's ready, for a record that began with the
    /// chip already up; whether the core was starting it.
    pub fn seed(&mut self) -> bool {
        let Some(id) = self.core.state.chip.starting() else { return false };
        self.seeding = true;
        self.feed(Input::Started(id, Ok(self.info)));
        self.seeding = false;
        true
    }

    /// The clock at `at` with the ticks owed before it dropped, for a record whose earlier lines
    /// have no time of their own; the next input sets the core's next tick again.
    pub fn jump(&mut self, at: Instant) {
        self.now = Instant(self.now.0.max(at.0));
        self.next = None;
        let Self { verdict, observer, .. } = self;
        verdict.advance(self.now.0, &mut |d| observer.disagreed(d));
    }

    /// The watch reset: the core and its chip start again from nothing, at `at` or later.
    pub fn restart(&mut self, at: Instant) {
        self.jump(at);
        self.core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
        self.translator = Translator::new(self.info);
        self.phone = None;
        self.asked_update = None;
        self.advertising = false;
    }

    /// A command the driver sent the chip.
    pub fn command(&mut self, at: Instant, code: u16, params: &[u8]) {
        self.advance(at);
        let Some((kind, name)) = driver_op(code) else { return };
        let u16_at = |i: usize| params.get(i..i + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]));
        let op = match (kind, name) {
            // Advertising type, then interval min and max (0.625 ms).
            (OpKind::Advertise, "advertise") => DriverOp::Advertise(u16_at(1), u16_at(3)),
            (OpKind::Advertise, _) => DriverOp::StopAdvertising,
            // Handle, then interval min and max (1.25 ms), latency, timeout (10 ms).
            (OpKind::UpdateParams, _) => DriverOp::AskParams(u16_at(2), u16_at(4), u16_at(6)),
            // Handle, then 1 to confirm.
            (OpKind::ConfirmNumeric, _) => DriverOp::Confirm(params.get(2) == Some(&1)),
            _ => DriverOp::Other(kind),
        };
        // An ask the menu never makes is taken as the console's `ble interval now`, over USB or
        // over the air, and given the core as a goal.
        if let DriverOp::AskParams(min, max, latency) = op {
            let menu = MENU.phone.iter().any(|c| (c.params.interval_min.as_u16(), c.params.interval_max.as_u16(), c.params.latency) == (min, max, latency));
            let now = interval_now_ask(min, max);
            if !menu && self.goals.phone_params != Some(now) {
                self.set_goals(self.now, Goals { phone_params: Some(now), ..self.goals });
            }
        }
        if let Some(act) = Act::of_driver(op) {
            let Self { verdict, observer, .. } = self;
            verdict.driver(self.now.0, act, &mut |d| observer.disagreed(d));
        }
        self.observer.driver(self.now, op, name, params);
        match name {
            "advertise" => self.advertising = true,
            "stop advertising" => self.advertising = false,
            _ => {}
        }
    }

    /// A packet from the chip.
    pub fn event(&mut self, at: Instant, packet: &[u8]) {
        self.advance(at);
        if accepted_update(packet) {
            if let Some(id) = self.asked_update.take() {
                self.feed(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
            }
        }
        let Some(e) = self.translator.event(packet) else { return };
        match e {
            RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, .. } => {
                self.phone = Some(link);
                self.advertising = false;
            }
            RadioEvent::LinkDown { link, .. } if self.phone == Some(link) => self.phone = None,
            RadioEvent::Ready(_) => {
                self.phone = None;
                self.advertising = false;
            }
            _ => {}
        }
        self.observer.event(self.now, &e);
        // The driver's reset ends in the chip's ready, which answers the core's start as
        // `Radio::start` would.
        match (e, self.core.state.chip.starting()) {
            (RadioEvent::Ready(info), Some(id)) => self.feed(Input::Started(id, Ok(info))),
            _ => self.feed(Input::Radio(e)),
        }
        // The console's ask ends with the phone's link, once the core has seen it end or the chip
        // come up again.
        if matches!(e, RadioEvent::LinkDown { .. } | RadioEvent::Ready(_)) && self.phone.is_none() && self.goals.phone_params.is_some() {
            self.set_goals(self.now, Goals { phone_params: None, ..self.goals });
        }
    }

    /// One step of the core, then each answer it is owed, depth first, as if each were fed
    /// where it was owed.
    fn feed(&mut self, input: Input) {
        let mut owed = [Owed::Stepped; OWED];
        let mut len = self.step(input, &mut owed, 0);
        while let Some(top) = len.checked_sub(1) {
            len = top;
            let input = match owed[top] {
                Owed::Submitted(id) => Input::Submitted(Ok(id)),
                Owed::Done(id) => Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))),
                Owed::Snapshot => {
                    let s = self.snapshot();
                    self.observer.snapshot(&s);
                    Input::Snapshot(Ok(s))
                }
                Owed::Stepped => {
                    self.observer.stepped(self.now, &self.core.state);
                    continue;
                }
            };
            len = self.step(input, &mut owed, len);
        }
    }

    /// The core's step, what it is owed for it pushed on `owed[..len]`; the new length.
    fn step(&mut self, input: Input, owed: &mut [Owed; OWED], mut len: usize) -> usize {
        let out = self.core.step(self.now, input);
        self.next = out.next;
        for t in out.tell.into_iter().flatten() {
            self.observer.tell(self.now, t);
        }
        if let Some(a) = out.chip {
            self.observer.chip(self.now, a);
        }
        let mut answers = [Some(Owed::Stepped), out.snapshot.then_some(Owed::Snapshot), None, None];
        if let Some(op) = out.op {
            self.observer.op(self.now, &op, self.seeding);
            if let (false, Some(act)) = (self.seeding, Act::of_core(&op)) {
                let Self { verdict, observer, .. } = self;
                verdict.core(self.now.0, act, &mut |d| observer.disagreed(d));
            }
            self.ids = self.ids.wrapping_add(1);
            let id = OpId(self.ids);
            if op.kind() == OpKind::UpdateParams {
                self.asked_update = Some(id);
            }
            answers[2] = (!op.kind().ends_with_event()).then_some(Owed::Done(id));
            answers[3] = Some(Owed::Submitted(id));
        }
        // Pushed in reverse, so taken in the order owed, `Stepped` after everything the step led to.
        for a in answers.into_iter().flatten() {
            if let Some(slot) = owed.get_mut(len) {
                *slot = a;
                len += 1;
            } else {
                self.observer.lost(self.now);
            }
        }
        len
    }

    /// The chip as the driver's commands and the chip's events have left it.
    fn snapshot(&self) -> Snapshot {
        let mut links = [None; MAX_LINKS];
        if let Some(link) = self.phone {
            links[0] = Some(LinkStatus { link, role: LeConnRole::Peripheral, peer: PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([0; 6]) }, encrypted: None });
        }
        Snapshot { at: self.now, links, advertising: self.advertising, scanning: false, initiating: false, anchor: None }
    }
}

