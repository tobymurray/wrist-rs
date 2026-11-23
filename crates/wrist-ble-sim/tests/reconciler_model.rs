//! The core, unchanged, against a radio that may refuse, fail, lose outcomes and events, drop the
//! phone's link or reset, within a budget of faults, after which it behaves.

use std::hash::{Hash, Hasher};

use stateright::{Checker, Model, Property};
use wrist_ble::policy::chip::{Action, Chip, StartId};
use wrist_ble::policy::engine::{Core, Input, Outputs};
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::invariants::COMMANDS_PER_MINUTE;
use wrist_ble::policy::plan::PlanHealth;
use wrist_ble::{
    AddrKind, BdAddr, ConnHandle, Duration, Failure, FailureKind, Instant, LeConnRole, LinkParams, LinkStatus, Millis, Op, OpId, OpKind, Outcome,
    PeerAddr, RadioEvent, ResetCause, SchedKind, Schedule, Snapshot, Status, MAX_LINKS,
};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES, INFO};

/// The radio makes no fault after this, and the core has until [`HORIZON`] to converge.
const FAULTS_UNTIL: Instant = Instant(60_000);
const HORIZON: Instant = Instant(240_000);

/// Time moves at most this far at once.
const WAIT: Millis = Millis::secs(30);

fn phone() -> ConnHandle {
    ConnHandle::new(0x801)
}

const OPENED: LinkParams = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };

/// The radio as it is, which the core learns only from events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Radio {
    advertising: Option<wrist_ble::AdvParams>,
    /// The operation that set [`Self::advertising`].
    advertised_by: Option<OpId>,
    phone: bool,
    encrypted: bool,
    /// Accepted operations without their outcome yet.
    on_air: [Option<(OpId, Op)>; 4],
    next_id: u16,
}

#[derive(Clone, Debug)]
struct S {
    core: Core<'static>,
    now: Instant,
    radio: Radio,
    /// What the core asked of the radio and has not had answered.
    asked: Option<Op>,
    snapshot: bool,
    start: Option<StartId>,
    next: Option<Instant>,
    faults: u8,
    acts: u8,
    /// An event the core never saw, and no resync since.
    unsynced: bool,
    /// The first promise the core broke, if any.
    broke: Option<&'static str>,
}

impl S {
    fn key(&self) -> String {
        format!("{:?}", (&self.core, self.now, self.radio, self.asked, self.snapshot, self.start, self.next, self.faults, self.acts, self.unsynced, self.broke))
    }

    fn may_fault(&self, m: Reconciler) -> bool {
        self.faults < m.faults && self.now < FAULTS_UNTIL
    }

    fn feed(&mut self, input: Input) {
        let had = self.asked;
        let out = self.core.step(self.now, input);
        self.judge(&out, had);
        if let Some(op) = out.op {
            self.asked = Some(op);
        }
        self.snapshot |= out.snapshot;
        match out.chip {
            Some(Action::Start(id)) => {
                self.start = Some(id);
                self.radio = Radio { next_id: self.radio.next_id, ..Radio::OFF };
            }
            Some(Action::Hold) => self.start = None,
            None => {}
        }
        self.next = out.next;
    }

    /// The outcome of the operation on air in slot `i`, as the chip would give it.
    fn complete(&mut self, i: usize) -> Option<()> {
        let (id, op) = self.radio.on_air[i].take()?;
        let link = op.link().unwrap_or(phone());
        let events: &[RadioEvent] = match op {
            Op::UpdateParams(..) => &[RadioEvent::ParamsAnswered { link, accepted: true }, RadioEvent::Params { link, result: Ok(OPENED) }],
            Op::RequestSecurity(_) => {
                self.radio.encrypted = true;
                &[RadioEvent::Encryption { link, on: true }]
            }
            _ => &[RadioEvent::Done(id, Ok(Outcome::Done))],
        };
        for e in events {
            self.feed(Input::Radio(*e));
        }
        Some(())
    }

    /// The promises each step keeps, the first broken kept.
    fn judge(&mut self, out: &Outputs, had: Option<Op>) {
        let c = &self.core;
        let broke = if out.op.is_some() && had.is_some() && !matches!(out.chip, Some(Action::Start(_))) {
            Some("a second operation asked before the first was answered")
        } else if let (Some(op), Some(p)) = (out.op, c.pending()) {
            let answer = matches!(op.kind(), OpKind::AllowRebond | OpKind::ConfirmNumeric | OpKind::Passkey | OpKind::AnswerParamRequest);
            let security = op.kind() == OpKind::RequestSecurity && p.op.link().is_some() && p.op.link() != op.link();
            (!answer && !security).then_some("past an operation on air went more than an answer")
        } else if let Some(Op::Advertise(p)) = out.op.filter(|_| !matches!(out.chip, Some(Action::Start(_)))) {
            let planned = c.target().path.first() == Some(wrist_ble::Change::Advertise(p));
            let admitted = Scheduler.admit(&c.state.radio, &wrist_ble::Change::Advertise(p)).is_ok();
            // A stop of advertising the core has no record of is the planned way to the baseline.
            (!(planned && admitted) && p.is_some()).then_some("an advertising start not the plan's next step, admitted")
        } else if matches!(out.op, Some(Op::UpdateParams(..))) && c.pending().into_iter().chain(c.passed()).any(|p| p.op.kind() == OpKind::RequestSecurity) {
            Some("a parameter ask while encryption is pending (I5)")
        } else if c.record(self.now).commands > COMMANDS_PER_MINUTE {
            Some("more commands in a minute than I2 allows")
        } else if !self.unsynced && c.state.phone.link().is_some() != self.radio.phone && matches!(c.state.chip.state, Chip::Ready(_)) && self.start.is_none() {
            Some("the core's phone link is not the radio's")
        } else {
            None
        };
        self.broke = self.broke.or(broke);
    }

    /// Nothing is owed, asked or on air, and the radio advertises as the plan says.
    fn converged(&self) -> bool {
        let c = &self.core;
        let quiet = self.asked.is_none() && !self.snapshot && self.start.is_none() && c.pending().is_none() && c.passed().next().is_none();
        let planned = c.target().activities.advertising;
        quiet && matches!(c.state.chip.state, Chip::Ready(_)) && c.state.plan == PlanHealth::Ok && c.state.radio.advertising == planned && self.radio.advertising == planned
    }

    fn snapshot(&self) -> Snapshot {
        let mut links = [None; MAX_LINKS];
        if self.radio.phone {
            let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) };
            links[0] = Some(LinkStatus { link: phone(), role: LeConnRole::Peripheral, peer, encrypted: Some(self.radio.encrypted) });
        }
        Snapshot { at: self.now, links, advertising: self.radio.advertising.is_some(), scanning: false, initiating: false, anchor: None }
    }
}

impl Radio {
    const OFF: Self = Self { advertising: None, advertised_by: None, phone: false, encrypted: false, on_air: [None; 4], next_id: 0 };
}

impl PartialEq for S {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for S {}

impl Hash for S {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.key().hash(h);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Act {
    Started,
    Snapshot,
    Accept,
    /// The chip's own refusal: advertising over advertising, or a stop with none.
    Refuse(FailureKind),
    /// The outcome of the operation on air in this slot, as the chip would give it.
    Complete(usize),
    /// A failure of it, or no outcome ever.
    Fail(usize),
    Lose(usize),
    PhoneUp,
    PhoneDown,
    Encrypted,
    PhoneMoves,
    EventsLost,
    Reset,
    Power,
    Hide,
    Wait,
}

/// The model's bounds.
#[derive(Clone, Copy)]
struct Reconciler {
    /// Faults the radio may make: refusals, failures, lost outcomes and events, a dropped link, a
    /// reset.
    faults: u8,
    /// The phone's own moves and the user's changes of goals, power and discoverability, in all.
    acts: u8,
}

fn goals(power: Power, discoverable: bool) -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable, pairing_open: true }, power, ..Goals::OFF }
}

/// Whether the chip takes `op` beside what runs (HARDWARE.md).
fn takes(r: &Radio, op: &Op) -> bool {
    match op {
        Op::Advertise(Some(_)) => r.advertising.is_none(),
        Op::Advertise(None) => r.advertising.is_some(),
        _ => op.link().is_none_or(|l| l == phone() && r.phone),
    }
}

impl Model for Reconciler {
    type State = S;
    type Action = Act;

    fn init_states(&self) -> Vec<S> {
        let mut s = S {
            core: Core::new(&CAPABILITIES, &Scheduler, &MENU),
            now: Instant(0),
            radio: Radio::OFF,
            asked: None,
            snapshot: false,
            start: None,
            next: None,
            faults: 0,
            acts: 0,
            unsynced: false,
            broke: None,
        };
        s.feed(Input::Goals(goals(Power::Cable, true)));
        vec![s]
    }

    fn actions(&self, s: &S, out: &mut Vec<Act>) {
        if s.now >= HORIZON || s.broke.is_some() {
            return;
        }
        // What the core is owed comes first, one answer at a time.
        if s.start.is_some() {
            out.push(Act::Started);
            return;
        }
        if let Some(op) = s.asked {
            if takes(&s.radio, &op) {
                out.push(Act::Accept);
                if s.may_fault(*self) {
                    out.extend([Act::Refuse(FailureKind::Busy), Act::Refuse(FailureKind::Timeout)]);
                    if op.kind() == OpKind::Advertise {
                        out.push(Act::Refuse(FailureKind::Schedule(SchedKind::NewIntervalFailed)));
                    }
                }
            } else {
                out.push(Act::Refuse(if op.link().is_some() { FailureKind::NotConnected } else { FailureKind::ModeExclusion }));
            }
            return;
        }
        if s.snapshot {
            out.push(Act::Snapshot);
            return;
        }
        let on_air = s.radio.on_air.iter().enumerate().filter_map(|(i, o)| o.map(|_| i));
        for i in on_air {
            out.push(Act::Complete(i));
            if s.may_fault(*self) {
                out.extend([Act::Fail(i), Act::Lose(i)]);
            }
        }
        // The radio gives every outcome before time moves; an outcome never given is a fault.
        if s.radio.on_air.iter().all(Option::is_none) {
            out.push(Act::Wait);
        }
        if s.radio.phone {
            if !s.radio.encrypted {
                out.push(Act::Encrypted);
            }
            if s.may_fault(*self) {
                out.push(Act::PhoneDown);
            }
        } else if s.radio.advertising.is_some_and(|a| a.connectable) {
            out.push(Act::PhoneUp);
        }
        if s.may_fault(*self) {
            out.extend([Act::EventsLost, Act::Reset]);
        }
        if s.acts < self.acts {
            out.extend([Act::Power, Act::Hide]);
            if s.radio.phone {
                out.push(Act::PhoneMoves);
            }
        }
    }

    fn next_state(&self, s: &S, act: Act) -> Option<S> {
        let mut n = s.clone();
        let fault = matches!(act, Act::Refuse(FailureKind::Busy | FailureKind::Timeout | FailureKind::Schedule(_)) | Act::Fail(_) | Act::Lose(_) | Act::PhoneDown | Act::EventsLost | Act::Reset);
        n.faults += u8::from(fault);
        n.acts += u8::from(matches!(act, Act::PhoneMoves | Act::Power | Act::Hide));
        match act {
            Act::Started => {
                let id = n.start.take()?;
                n.feed(Input::Started(id, Ok(INFO)));
            }
            Act::Snapshot => {
                n.snapshot = false;
                n.unsynced = false;
                // The snapshot shows the advertising operations' outcomes: none fails after it.
                for o in &mut n.radio.on_air {
                    if o.is_some_and(|(_, op)| op.kind() == OpKind::Advertise) {
                        *o = None;
                    }
                }
                let snap = n.snapshot();
                n.feed(Input::Snapshot(Ok(snap)));
            }
            Act::Accept => {
                let op = n.asked.take()?;
                n.radio.next_id += 1;
                let id = OpId(n.radio.next_id);
                let slot = n.radio.on_air.iter_mut().find(|o| o.is_none())?;
                *slot = Some((id, op));
                if let Op::Advertise(p) = op {
                    n.radio.advertising = p;
                    n.radio.advertised_by = Some(id);
                }
                n.feed(Input::Submitted(Ok(id)));
            }
            Act::Refuse(kind) => {
                n.asked = None;
                n.feed(Input::Submitted(Err(Failure { kind, native: 0x0C })));
            }
            Act::Complete(i) => n.complete(i)?,
            Act::Fail(i) => {
                let (id, op) = n.radio.on_air[i].take()?;
                // A start that failed leaves nothing advertising, unless something replaced it.
                if matches!(op, Op::Advertise(Some(_))) && n.radio.advertised_by == Some(id) {
                    n.radio.advertising = None;
                }
                n.feed(Input::Radio(RadioEvent::Done(id, Err(Failure { kind: FailureKind::Timeout, native: 0 }))));
            }
            Act::Lose(i) => {
                n.radio.on_air[i] = None;
                n.unsynced = true;
            }
            Act::PhoneUp => {
                n.radio.phone = true;
                n.radio.advertising = None;
                n.feed(Input::Radio(RadioEvent::LinkUp { link: phone(), role: LeConnRole::Peripheral, peer: s.snapshot().links[0].map_or(PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9; 6]) }, |l| l.peer), params: Some(OPENED) }));
            }
            Act::PhoneDown => {
                n.radio.phone = false;
                n.radio.encrypted = false;
                for o in &mut n.radio.on_air {
                    if o.is_some_and(|(_, op)| op.link() == Some(phone())) {
                        *o = None;
                    }
                }
                n.feed(Input::Radio(RadioEvent::LinkDown { link: phone(), reason: Status::CONN_TIMEOUT }));
            }
            Act::Encrypted => {
                n.radio.encrypted = true;
                n.feed(Input::Radio(RadioEvent::Encryption { link: phone(), on: true }));
            }
            Act::PhoneMoves => {
                let own = LinkParams { interval: Duration::from_u16(36), ..OPENED };
                n.feed(Input::Radio(RadioEvent::Params { link: phone(), result: Ok(own) }));
            }
            Act::EventsLost => {
                n.unsynced = true;
                n.feed(Input::Radio(RadioEvent::EventsLost));
            }
            Act::Reset => {
                n.radio = Radio { next_id: n.radio.next_id, ..Radio::OFF };
                n.asked = None;
                n.feed(Input::Radio(RadioEvent::ChipReset(ResetCause::Watchdog)));
            }
            Act::Power | Act::Hide => {
                let g = *n.core.goals();
                let power = if act == Act::Power { if g.power == Power::Cable { Power::Battery } else { Power::Cable } } else { g.power };
                n.feed(Input::Goals(goals(power, g.phone.discoverable != (act == Act::Hide))));
            }
            Act::Wait => {
                n.now = n.next.filter(|&t| t > s.now).unwrap_or(s.now + WAIT).min(s.now + WAIT).min(HORIZON);
                n.feed(Input::Tick);
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("the core keeps its promises at every step", |_, s: &S| s.broke.is_none()),
            Property::always("converged by the horizon, once the radio behaves (I3)", |_, s: &S| s.now < HORIZON || s.converged()),
            Property::sometimes("a phone link asked for its parameters", |_, s: &S| s.radio.phone && s.core.phone_params().is_some()),
            Property::sometimes("every fault allowed", |m: &Reconciler, s: &S| s.faults == m.faults),
        ]
    }
}

#[test]
fn the_reconciler_keeps_its_promises_against_any_radio() {
    let bounds = std::env::var("RECONCILER_MODEL").ok().and_then(|v| {
        let n: Vec<u8> = v.split(',').filter_map(|x| x.parse().ok()).collect();
        (n.len() == 2).then(|| Reconciler { faults: n[0], acts: n[1] })
    });
    let m = bounds.unwrap_or(Reconciler { faults: 1, acts: 1 });
    let t = std::time::Instant::now();
    let c = m.checker().spawn_bfs().join();
    let m = c.model();
    for p in m.properties() {
        if let (stateright::Expectation::Always, Some(path)) = (p.expectation, c.discovery(p.name)) {
            let acts: Vec<String> = path.clone().into_vec().iter().map(|(s, a)| format!("{:?} at {:?}", a, s.now)).collect();
            panic!("{}: {}", p.name, acts.join("\n"));
        }
    }
    c.assert_properties();
    println!("{} states, {} unique, {:?}", c.state_count(), c.unique_state_count(), t.elapsed());
}

