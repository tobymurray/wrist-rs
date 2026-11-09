//! The reconciler's rules for what goes out past an operation on air, asking the phone again,
//! and a chip that keeps saying busy (F46).

use wrist_ble::{
    AddrKind, BdAddr, ConnHandle, Duration, Failure, FailureKind, Instant, LeConnRole, LinkParams, Op, OpId, Outcome, PeerAddr,
    RadioEvent, Snapshot, MAX_LINKS,
};
use wrist_ble_core::engine::{Core, Input, Outputs, ASKS_PER_WANT, BUSY_BUDGET};
use wrist_ble_core::goals::{Goals, PhoneGoal, Power};
use wrist_ble_core::phone::SETTLE;
use wrist_ble_sim::fake::INFO;
use wrist_ble_sim::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn link() -> ConnHandle {
    ConnHandle::new(0x801)
}

/// What `BlueZ` opens a link with: 30 ms, no latency.
const OPENED: LinkParams = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };
/// The battery ask, as a phone that takes it reports it.
const BATTERY: LinkParams = LinkParams { interval: Duration::from_u16(156), latency: 4, timeout: Duration::from_u16(600) };
/// An Android's own update after the watch's, and what macOS answers an ask with: 45 ms.
const OWN: LinkParams = LinkParams { interval: Duration::from_u16(36), latency: 0, timeout: Duration::from_u16(500) };

fn phone() -> PeerAddr {
    PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) }
}

/// A core driven by hand: every operation is answered as the test says.
struct Bench {
    core: Core<'static>,
    now: Instant,
    id: u16,
    /// The operation last asked for, not yet answered.
    asked: Option<Op>,
}

impl Bench {
    /// The radio on with `goals`, up, and advertising.
    fn new(goals: Goals) -> Self {
        let mut b = Self { core: Core::new(&CAPABILITIES, &Scheduler, &MENU), now: Instant(0), id: 0, asked: None };
        b.feed(Input::Goals(goals));
        let start = b.core.state.chip.starting().expect("a start");
        b.feed(Input::Started(start, Ok(INFO)));
        let id = b.accept();
        b.feed(Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
        b
    }

    fn feed(&mut self, input: Input) -> Outputs {
        let out = self.core.step(self.now, input);
        if out.op.is_some() {
            self.asked = out.op;
        }
        out
    }

    fn at(&mut self, ms: u64, input: Input) -> Outputs {
        self.now = Instant(ms);
        self.feed(input)
    }

    /// The operation asked for, accepted.
    fn accept(&mut self) -> OpId {
        assert!(self.asked.take().is_some(), "nothing asked for");
        self.id += 1;
        let id = OpId(self.id);
        self.feed(Input::Submitted(Ok(id)));
        id
    }

    fn refuse(&mut self, kind: FailureKind) -> Outputs {
        assert!(self.asked.take().is_some(), "nothing asked for");
        self.feed(Input::Submitted(Err(Failure { kind, native: 0x0C })))
    }

    /// Ticks until `until`, at the times the core asks to run; the operations asked for meanwhile,
    /// each accepted.
    fn run_to(&mut self, until: u64) -> Vec<Op> {
        let mut ops = Vec::new();
        loop {
            if let Some(op) = self.asked {
                ops.push(op);
                self.accept();
            }
            let next = self.core.step(self.now, Input::Tick).next.map_or(until, |t| t.0.min(until));
            if next <= self.now.0 && next == until {
                break;
            }
            self.at(next.max(self.now.0 + 1).min(until), Input::Tick);
            if self.now.0 >= until {
                if let Some(op) = self.asked {
                    ops.push(op);
                    self.accept();
                }
                break;
            }
        }
        ops
    }

    fn phone_up(&mut self, ms: u64) {
        self.at(ms, Input::Radio(RadioEvent::LinkUp { link: link(), role: LeConnRole::Peripheral, peer: phone(), params: OPENED }));
    }

    fn params(&mut self, ms: u64, p: LinkParams) -> Outputs {
        self.at(ms, Input::Radio(RadioEvent::Params { link: link(), result: Ok(p) }))
    }
}

fn battery() -> Goals {
    Goals { radio_on: true, power: Power::Battery, ..Goals::OFF }
}

fn asks(ops: &[Op]) -> usize {
    ops.iter().filter(|op| matches!(op, Op::UpdateParams(..))).count()
}

/// An encrypted battery link, asked for the battery's parameters once.
fn asked_on_battery() -> Bench {
    let mut b = Bench::new(battery());
    b.phone_up(1_000);
    let out = b.at(1_300, Input::Radio(RadioEvent::Encryption { link: link(), on: true }));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
    b
}

/// R1-4: an ask refused before it reached the air is asked again once the backoff ends.
#[test]
fn an_ask_refused_at_submit_is_asked_again_after_the_backoff() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    let out = b.at(1_301, Input::Tick);
    assert_eq!(out.op, None);
    let out = b.at(2_300, Input::Tick);
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// Refusals at submit count against the want's asks: no more than [`ASKS_PER_WANT`].
#[test]
fn asks_refused_at_submit_are_bounded() {
    let mut b = asked_on_battery();
    let mut asked = 1;
    b.refuse(FailureKind::NoResources);
    for t in (2_000..200_000).step_by(1_000) {
        let out = b.at(t, Input::Tick);
        if out.op.is_some() {
            asked += 1;
            b.refuse(FailureKind::NoResources);
        }
    }
    assert_eq!(asked, usize::from(ASKS_PER_WANT));
}

/// B-1: a phone that took the ask and then moved off it by its own update (an Android) is asked
/// again once it has settled, and at most [`ASKS_PER_WANT`] times for one want.
#[test]
fn a_phone_that_undoes_the_ask_is_asked_again_within_the_budget() {
    let mut b = asked_on_battery();
    b.accept();
    let mut at = 1_500;
    let mut total = 1;
    for _ in 0..5 {
        b.params(at, BATTERY);
        b.params(at + 2_000, OWN);
        let ops = b.run_to(at + 2_000 + u64::from(SETTLE.0) + 10);
        total += asks(&ops);
        at += 20_000;
        b.run_to(at - 1);
    }
    assert_eq!(total, usize::from(ASKS_PER_WANT));
    assert_eq!(b.core.counters.reasks, u32::from(ASKS_PER_WANT) - 1);
}

/// The re-ask waits for the phone's own update to settle.
#[test]
fn the_ask_again_waits_for_the_phone_to_settle() {
    let mut b = asked_on_battery();
    b.accept();
    b.params(1_500, BATTERY);
    b.params(3_000, OWN);
    assert_eq!(asks(&b.run_to(3_000 + u64::from(SETTLE.0) - 1)), 0);
    assert_eq!(asks(&b.run_to(3_000 + u64::from(SETTLE.0) + 1)), 1);
}

/// macOS answers an ask with its own parameters: that is its answer, and it is not asked again.
#[test]
fn a_phone_that_answers_the_ask_with_its_own_is_not_asked_again() {
    let mut b = asked_on_battery();
    b.accept();
    b.params(1_500, OWN);
    assert_eq!(asks(&b.run_to(120_000)), 0);
}

/// R1-3: an answer to the chip's own procedure goes out past an operation on air: the rebond a
/// lost bond needs passes the security request waiting on it (check 8's P1 path), and nothing
/// asks for parameters before the link is encrypted (F62).
#[test]
fn an_answer_passes_a_pending_security_request() {
    let mut b = Bench::new(battery());
    b.phone_up(1_000);
    assert_eq!(b.at(2_600, Input::Tick).op, None);
    let out = b.at(6_000, Input::Tick);
    assert_eq!(out.op, Some(Op::RequestSecurity(link())), "{out:?}");
    b.accept();
    let out = b.at(6_100, Input::Radio(RadioEvent::BondLost(link())));
    assert_eq!(out.op, Some(Op::AllowRebond(link())), "{out:?}");
    b.accept();
    let out = b.at(6_400, Input::Radio(RadioEvent::Encryption { link: link(), on: true }));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

fn stop_advertising() -> Goals {
    Goals { phone: PhoneGoal { discoverable: false, pairing_open: false }, ..battery() }
}

/// I2: a chip that says busy to everything is restarted once its budget is spent, about a
/// minute and a half in, not retried every 32 s for ever.
#[test]
fn a_chip_that_stays_busy_is_restarted() {
    let mut b = Bench::new(battery());
    let out = b.at(1_000, Input::Goals(stop_advertising()));
    assert_eq!(out.op, Some(Op::Advertise(None)), "{out:?}");
    let mut refusals = 0;
    let mut restarted = None;
    while b.now.0 < 600_000 && restarted.is_none() {
        if b.asked.is_some() {
            b.refuse(FailureKind::Busy);
            refusals += 1;
        }
        let next = b.core.step(b.now, Input::Tick).next.expect("a wake");
        let out = b.at(next.0, Input::Tick);
        if matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))) {
            restarted = Some(b.now);
        }
    }
    assert_eq!(refusals, usize::from(BUSY_BUDGET));
    assert_eq!(b.core.counters.busy_spent, 1);
    assert!(restarted.is_some_and(|t| t.0 < 150_000), "{restarted:?}");
}

/// An occasional busy refusal, one every 15 minutes, never restarts the chip.
#[test]
fn an_occasional_busy_refusal_is_only_retried() {
    let mut b = Bench::new(battery());
    for i in 0..40u64 {
        let t = 1_000 + i * 900_000;
        let goals = if i % 2 == 0 { stop_advertising() } else { battery() };
        let out = b.at(t, Input::Goals(goals));
        assert!(matches!(out.op, Some(Op::Advertise(_))), "{i}: {out:?}");
        b.refuse(FailureKind::Busy);
        let out = b.at(t + 1_000, Input::Tick);
        assert!(matches!(out.op, Some(Op::Advertise(_))), "{i}: {out:?}");
        let id = b.accept();
        b.at(t + 1_001, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
    }
    assert_eq!(b.core.counters.busy_spent, 0);
}

/// A snapshot cannot show the same handle ended and made again behind lost events: what was known
/// of the link's parameters is let go, and it is judged afresh.
#[test]
fn a_link_a_resync_finds_again_is_judged_afresh() {
    let mut b = Bench::new(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF });
    b.phone_up(1_000);
    let out = b.at(1_300, Input::Radio(RadioEvent::Encryption { link: link(), on: true }));
    assert_eq!(out.op, None, "30 ms fits the cable's ask: {out:?}");
    let out = b.at(2_000, Input::Radio(RadioEvent::EventsLost));
    assert!(out.snapshot);
    let mut links = [None; MAX_LINKS];
    links[0] = Some(wrist_ble::LinkStatus { link: link(), role: LeConnRole::Peripheral, peer: phone() });
    let snap = Snapshot { at: Instant(2_001), links, advertising: false, scanning: false, initiating: false, anchor: None };
    let out = b.at(2_001, Input::Snapshot(Ok(snap)));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// The restart when the busy budget runs out ends the phone's link too: a chip that answers
/// nothing but busy is restarted whoever is connected (owner, 2026-11-09).
#[test]
fn a_chip_that_stays_busy_is_restarted_under_the_phones_link() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    let mut restarted = 0;
    for i in 1..u16::from(BUSY_BUDGET) {
        // A new want each time (the console's ask), so each is asked at once and refused busy.
        let now = wrist_ble_sim::menu::interval_now(8 + i, 8 + i);
        let t = 2_000 + u64::from(i) * 40_000;
        let out = b.at(t, Input::Goals(Goals { phone_params: Some(now), ..battery() }));
        let out = if out.op.is_some() { out } else { b.at(t + 33_000, Input::Tick) };
        assert!(matches!(out.op, Some(Op::UpdateParams(..))), "{i}: {out:?}");
        let out = b.refuse(FailureKind::Busy);
        restarted += usize::from(matches!(out.chip, Some(wrist_ble_core::chip::Action::Start(_))));
    }
    assert_eq!(restarted, 1, "{:?}", b.core.counters);
    assert_eq!(b.core.counters.busy_spent, 1);
    assert_eq!(b.core.state.phone.link(), None);
}
