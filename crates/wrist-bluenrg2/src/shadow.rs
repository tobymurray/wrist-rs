//! The Bluetooth core fed what the BlueNRG-2 driver sends and the chip answers, each of its
//! operations set beside what the driver did ([`verdict`]): taken as the driver's and answered here
//! ([`Mode::Shadow`]), or handed to the driver to carry out ([`Mode::Drive`]).

pub mod menu;
pub mod record;
pub mod trace;
pub mod verdict;

use bluenrg2::opcode;
use wrist_ble::{
    AddrKind, BdAddr, ChipInfo, ConnHandle, ConnParams, Failure, FailureKind, Fault, Instant, LeConnRole, LinkParams, LinkStatus, Op, OpId, OpKind, Outcome, PeerAddr, RadioEvent, Snapshot,
    MAX_LINKS,
};
use wrist_ble::policy::chip::Action;
use wrist_ble::policy::engine::{Core, Counters, Input, Omitted, Tell};
use wrist_ble::policy::goals::{Goals, Power};
use wrist_ble::policy::State;
use crate::events::Translator;
use crate::ops::refused;
use crate::{Scheduler, CAPABILITIES};

use crate::shadow::menu::{interval_now_ask, MENU};
use crate::shadow::verdict::{Act, Disagreement, Verdict};

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

/// Whether the driver's command `opcode` is one of the core's operations.
#[must_use]
pub fn is_op(opcode: u16) -> bool {
    driver_op(opcode).is_some()
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

/// The driver's ask, as the core would make it.
fn ask_of(min: u16, max: u16, latency: u16, timeout: u16) -> ConnParams {
    ConnParams {
        interval_min: wrist_ble::Interval::from_u16(min),
        interval_max: wrist_ble::Interval::from_u16(max),
        latency,
        timeout: wrist_ble::Timeout::from_u16(timeout),
        ce_len: wrist_ble::Slots::from_u16(0),
    }
}

/// The chip's report that the central accepted the watch's parameter request.
fn accepted_update(packet: &[u8]) -> bool {
    matches!(bluenrg2::parse(packet), bluenrg2::Event::Vendor(bluenrg2::VendorEvent::L2capUpdateResponse { accepted: true, .. }))
}

/// Who carries out the core's operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nobody: each is taken as the driver's command, done at once or refused as that command was.
    #[default]
    Shadow,
    /// The driver: each is handed out ([`Shadow::issue`]) and answered with the driver's own
    /// answers ([`Shadow::submitted`], [`Shadow::reported`]); every command the driver sends that
    /// is one of the core's must be the core's.
    Drive,
}

/// Why a driver's ask the core did not make is no disagreement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Excuse {
    Core(Omitted),
    /// The chip refused it busy, so it never reached the air.
    ChipBusy,
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
    /// A driver's ask owed nothing: the core leaves it out by one of its rules, or the chip refused it.
    fn excused(&mut self, _at: Instant, _act: Act, _why: Excuse) {}
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

#[allow(clippy::struct_excessive_bools, reason = "each is its own fact of the record, not a state")]
pub struct Shadow<O> {
    core: Core<'static>,
    translator: Translator,
    info: ChipInfo,
    goals: Goals,
    now: Instant,
    next: Option<Instant>,
    ids: u16,
    phone: Option<ConnHandle>,
    /// The phone's link has been seen encrypted, which it stays until it ends.
    encrypted: bool,
    /// The core's parameter request on air, which the central's answer ends.
    asked_update: Option<OpId>,
    /// The core's latest operation that waits for an event to end it.
    awaiting: Option<(OpId, Op)>,
    /// The driver's latest command that is an operation, and the handle its parameters begin with.
    sent: Option<(u16, u16)>,
    /// The driver's latest ask for parameters.
    asked: Option<(u16, ConnParams)>,
    advertising: bool,
    /// The core is being told of a chip that was up before it was.
    seeding: bool,
    mode: Mode,
    /// The core's operation for the driver to carry out, in [`Mode::Drive`].
    issue: Option<Op>,
    /// The core's last word on the chip, in [`Mode::Drive`].
    chip_action: Option<Action>,
    /// The core asked for a snapshot, for the driver to take in [`Mode::Drive`].
    snapshot_wanted: bool,
    /// The snapshots the core asks for are the driver's: a record showed the driver driving
    /// ([`Self::driven`]) or one of its lost reads ([`Self::lost_read`]).
    driver_snapshots: bool,
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
            encrypted: false,
            asked_update: None,
            awaiting: None,
            sent: None,
            asked: None,
            advertising: false,
            seeding: false,
            mode: Mode::Shadow,
            issue: None,
            chip_action: None,
            snapshot_wanted: false,
            driver_snapshots: false,
            verdict: Verdict::new(),
            observer,
        }
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Who carries out the core's operations from the radio's next start; refused, false, while the
    /// radio is wanted on, since operations of the other mode may be in flight.
    pub fn set_mode(&mut self, mode: Mode) -> bool {
        if self.goals.radio_on {
            return false;
        }
        self.mode = mode;
        self.issue = None;
        self.chip_action = None;
        self.snapshot_wanted = false;
        // What the shadow answers for, which a driven core never leaves.
        self.asked_update = None;
        self.awaiting = None;
        self.asked = None;
        true
    }

    /// The operation the core asked for, for the driver to carry out and answer with
    /// [`Self::submitted`]; the core asks for no other until then.
    pub fn issue(&mut self) -> Option<Op> {
        self.issue.take()
    }

    /// Whether an operation waits for [`Self::issue`].
    #[must_use]
    pub fn issuing(&self) -> bool {
        self.issue.is_some()
    }

    /// What the core last asked of the chip: a start, or to hold it in reset.
    pub fn chip_action(&mut self) -> Option<Action> {
        self.chip_action.take()
    }

    /// Whether [`Self::chip_action`] has an action waiting.
    #[must_use]
    pub fn chip_acting(&self) -> bool {
        self.chip_action.is_some()
    }

    /// Whether the core asked for a snapshot, in [`Mode::Drive`], that [`Self::take_snapshot`]
    /// has yet to take.
    #[must_use]
    pub fn snapshot_wanted(&self) -> bool {
        self.snapshot_wanted
    }

    /// Whether the core asked for a snapshot, for the driver to take now and answer with
    /// [`Self::snapshotted`]; one asked for meanwhile is wanted again.
    pub fn take_snapshot(&mut self) -> bool {
        core::mem::take(&mut self.snapshot_wanted)
    }

    /// The driver's snapshot, holding every event handed out before it.
    pub fn snapshotted(&mut self, at: Instant, r: Result<Snapshot, Fault>) {
        self.advance(at);
        if let Ok(s) = &r {
            self.observer.snapshot(s);
        }
        self.feed(Input::Snapshot(r));
    }

    /// The driver's answer to the operation [`Self::issue`] handed out.
    pub fn submitted(&mut self, at: Instant, r: Result<OpId, Failure>) {
        self.advance(at);
        self.feed(Input::Submitted(r));
    }

    /// Whether the driver of a record drove the core from its last start, so that the snapshots
    /// the core asks for are the driver's, taken as [`Self::snapshot_wanted`] and given by
    /// [`Self::snapshotted`].
    pub fn driven(&mut self, yes: bool) {
        self.driver_snapshots = yes;
    }

    /// A lost read of the driver's that a record shows a driven core was told, in either mode, so
    /// the driver drove it ([`Self::driven`]).
    pub fn lost_read(&mut self, at: Instant) {
        self.advance(at);
        self.driver_snapshots = true;
        self.observer.event(self.now, &RadioEvent::EventsLost);
        self.feed(Input::Radio(RadioEvent::EventsLost));
    }

    /// What the driver itself reports, an operation's end or packets it dropped, taken in
    /// [`Mode::Drive`] only, as the shadow's own operations have ids of their own.
    pub fn reported(&mut self, at: Instant, e: RadioEvent) {
        if let (Mode::Drive, RadioEvent::Done(..) | RadioEvent::EventsLost) = (self.mode, e) {
            self.advance(at);
            self.observer.event(self.now, &e);
            self.feed(Input::Radio(e));
        }
    }

    #[must_use]
    pub fn counters(&self) -> &Counters {
        &self.core.counters
    }

    #[must_use]
    pub fn core_tally(&self) -> wrist_ble::policy::tally::Tally {
        self.core.tally
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

    /// The core's operation in flight.
    #[must_use]
    pub fn core_pending(&self) -> Option<&wrist_ble::policy::plan::Pending> {
        self.core.pending()
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

    /// `ble on` or `ble off`.
    pub fn radio(&mut self, at: Instant, on: bool) {
        self.set_goals(at, Goals { radio_on: on, ..self.goals });
    }

    /// The timing the power source and a transfer ask for.
    pub fn timing(&mut self, at: Instant, power: Power, transfer: bool) {
        if (power, transfer) == (self.goals.power, self.goals.transfer) {
            return;
        }
        self.set_goals(at, Goals { power, transfer, ..self.goals });
    }

    /// `ble interval now <min> <max>`, in 1.25 ms, as the core's [`Input::IntervalNow`].
    pub fn interval_now(&mut self, at: Instant, min: u16, max: u16) {
        self.advance(at);
        self.feed(Input::IntervalNow(interval_now_ask(min, max)));
    }

    /// What the core wants of the phone's link.
    #[must_use]
    pub fn phone_want(&self) -> Option<ConnParams> {
        self.core.phone_want()
    }

    /// The phone link's parameters as the core holds them.
    #[must_use]
    pub fn phone_params(&self) -> Option<LinkParams> {
        self.core.phone_params()
    }

    /// `ble yes` or `ble no`.
    pub fn answer(&mut self, at: Instant, yes: bool) {
        self.advance(at);
        self.feed(Input::Answer(yes));
    }

    /// The driver's bring-up stopped short of the chip being up.
    pub fn start_failed(&mut self, at: Instant) {
        self.advance(at);
        self.feed(Input::StartFailed);
    }

    /// The user asked for the radio again after the core gave up on the chip.
    pub fn retry_radio(&mut self, at: Instant) {
        self.advance(at);
        self.feed(Input::RetryRadio);
    }

    /// When the core, having given up on the chip, tries it again by itself.
    #[must_use]
    pub fn retry_at(&self) -> Option<Instant> {
        match self.core.state.chip.state {
            wrist_ble::policy::chip::Chip::Fault { retry_at, .. } => Some(retry_at),
            _ => None,
        }
    }

    /// A hang-up the operator asked for (`ble disconnect`, `ble forget`), which the core has no
    /// goal for.
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
        self.encrypted = false;
        self.asked_update = None;
        self.awaiting = None;
        self.sent = None;
        self.asked = None;
        self.advertising = false;
        self.issue = None;
        self.chip_action = None;
        self.snapshot_wanted = false;
        self.driver_snapshots = false;
    }

    /// A command the driver sent the chip.
    pub fn command(&mut self, at: Instant, code: u16, params: &[u8]) {
        self.advance(at);
        let u16_at = |i: usize| params.get(i..i + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]));
        self.sent = driver_op(code).map(|_| (code, u16_at(0)));
        if code == opcode::ACI_GAP_TERMINATE_GAP_PROC {
            self.scan_ended();
        }
        let Some((kind, name)) = driver_op(code) else { return };
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
        if self.mode == Mode::Drive {
            if let Some(act) = Act::of_driver(op) {
                let Self { verdict, observer, .. } = self;
                verdict.driver(self.now.0, act, &mut |d| observer.disagreed(d));
            }
            self.observer.driver(self.now, op, name, params);
            return;
        }
        // An ask the menu never makes is taken as the console's `ble interval now`, over USB or
        // over the air.
        if let DriverOp::AskParams(min, max, latency) = op {
            let menu = MENU.phone.iter().any(|c| (c.params.interval_min.as_u16(), c.params.interval_max.as_u16(), c.params.latency) == (min, max, latency));
            if !menu {
                self.feed(Input::IntervalNow(interval_now_ask(min, max)));
            }
        }
        let ask = match op {
            DriverOp::AskParams(min, max, latency) => Some(ask_of(min, max, latency, u16_at(8))),
            _ => None,
        };
        self.asked = ask.map(|a| (u16_at(0), a));
        let excuse = ask.and_then(|a| self.core.omits_ask(self.now, &a));
        if let Some(act) = Act::of_driver(op) {
            let Self { verdict, observer, .. } = self;
            match excuse {
                Some(why) => {
                    observer.excused(self.now, act, Excuse::Core(why));
                    verdict.excused(self.now.0, act, &mut |d| observer.disagreed(d));
                }
                None => verdict.driver(self.now.0, act, &mut |d| observer.disagreed(d)),
            }
        }
        self.observer.driver(self.now, op, name, params);
        self.advertised(name);
    }

    fn advertised(&mut self, name: &str) {
        match name {
            "advertise" => self.advertising = true,
            "stop advertising" => self.advertising = false,
            _ => {}
        }
    }

    /// A packet from the chip.
    pub fn event(&mut self, at: Instant, packet: &[u8]) {
        self.advance(at);
        self.refusal(packet);
        if matches!(bluenrg2::parse(packet), bluenrg2::Event::Vendor(bluenrg2::VendorEvent::GapProcComplete { procedure: bluenrg2::gap_proc::GENERAL_DISCOVERY, .. })) {
            self.scan_ended();
        }
        if accepted_update(packet) {
            if let Some(id) = self.asked_update.take() {
                self.feed(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
            }
        }
        let Some(e) = self.translator.event(packet) else { return };
        match e {
            RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, .. } => {
                self.phone = Some(link);
                self.encrypted = false;
                self.advertising = false;
            }
            RadioEvent::LinkDown { link, .. } if self.phone == Some(link) => self.phone = None,
            RadioEvent::Encryption { link, on: true } if self.phone == Some(link) => self.encrypted = true,
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
    }

    /// The driver's scan over: every refusal is asked again, since one may have been the scan's.
    fn scan_ended(&mut self) {
        self.core.forget_refusals();
        self.feed(Input::Tick);
    }

    /// The chip's refusal of the driver's command, given to the core as its own answer where the
    /// core waits on the same operation.
    fn refusal(&mut self, packet: &[u8]) {
        let (code, status) = match bluenrg2::parse(packet) {
            bluenrg2::Event::CommandStatus { cmd_opcode, status, .. } | bluenrg2::Event::CommandComplete { cmd_opcode, status: Some(status), .. } => (cmd_opcode, status.into_inner()),
            _ => return,
        };
        let Some((sent, handle)) = self.sent.filter(|&(c, _)| c == code && status != 0) else { return };
        let Some((kind, _)) = driver_op(sent) else { return };
        if let Some((link, ask)) = self.asked.filter(|_| kind == OpKind::UpdateParams) {
            let op = Op::UpdateParams(ConnHandle::new(link), ask);
            if refused(&op, status).kind == FailureKind::Busy {
                if let Some(act) = Act::of_core(&op) {
                    self.verdict.refused_busy(act);
                    self.observer.excused(self.now, act, Excuse::ChipBusy);
                }
            }
        }
        let Some((id, op)) = self.awaiting.filter(|(_, op)| op.kind() == kind && op.link().is_none_or(|l| l.raw() == handle)) else { return };
        self.awaiting = None;
        self.feed(Input::Radio(RadioEvent::Done(id, Err(refused(&op, status)))));
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
                Owed::Snapshot if self.mode == Mode::Drive || self.driver_snapshots => {
                    self.snapshot_wanted = true;
                    continue;
                }
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
    fn step(&mut self, input: Input, owed: &mut [Owed; OWED], len: usize) -> usize {
        let out = self.core.step(self.now, input);
        self.next = out.next;
        for t in out.tell.into_iter().flatten() {
            self.observer.tell(self.now, t);
        }
        if let Some(a) = out.chip {
            self.observer.chip(self.now, a);
            if self.mode == Mode::Drive {
                self.chip_action = Some(a);
            }
        }
        let mut answers = [Some(Owed::Stepped), out.snapshot.then_some(Owed::Snapshot), None, None];
        if let Some(op) = out.op {
            self.observer.op(self.now, &op, self.seeding);
            if let (false, Some(act)) = (self.seeding, Act::of_core(&op)) {
                let Self { verdict, observer, .. } = self;
                verdict.core(self.now.0, act, &mut |d| observer.disagreed(d));
            }
            if self.mode == Mode::Drive {
                self.issue = Some(op);
                return self.owe(&mut answers, owed, len);
            }
            self.ids = self.ids.wrapping_add(1);
            let id = OpId(self.ids);
            if op.kind() == OpKind::UpdateParams {
                self.asked_update = Some(id);
            }
            if op.kind().ends_with_event() {
                self.awaiting = Some((id, op));
            }
            answers[2] = (!op.kind().ends_with_event()).then_some(Owed::Done(id));
            answers[3] = Some(Owed::Submitted(id));
        }
        self.owe(&mut answers, owed, len)
    }

    /// `answers` pushed on `owed[..len]` in reverse, so taken in the order owed, `Stepped` after
    /// everything the step led to; the new length.
    fn owe(&mut self, answers: &mut [Option<Owed>; 4], owed: &mut [Owed; OWED], mut len: usize) -> usize {
        for a in answers.iter_mut().filter_map(Option::take) {
            if let Some(slot) = owed.get_mut(len) {
                *slot = a;
                len += 1;
            } else {
                self.observer.lost(self.now);
            }
        }
        len
    }

    /// The chip as the driver's commands and the chip's events have left it, the phone's link
    /// encrypted once seen so and otherwise unknown, since the lost events may have held it; in
    /// [`Mode::Shadow`], as a driven core's is the driver's.
    fn snapshot(&self) -> Snapshot {
        let mut links = [None; MAX_LINKS];
        if let Some(link) = self.phone {
            let encrypted = self.encrypted.then_some(true);
            links[0] = Some(LinkStatus { link, role: LeConnRole::Peripheral, peer: PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([0; 6]) }, encrypted });
        }
        Snapshot { at: self.now, links, advertising: self.advertising, scanning: false, initiating: false, anchor: None }
    }
}

