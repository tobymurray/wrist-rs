//! The reconciler's rules for what goes out past an operation on air, asking the phone,
//! and a chip that keeps saying busy.

use wrist_ble::{
    AddrKind, BdAddr, ConnHandle, Duration, Failure, FailureKind, Instant, LeConnRole, LinkParams, Op, OpId, Outcome, PeerAddr,
    RadioEvent, Snapshot, Status, MAX_LINKS,
};
use wrist_ble::policy::engine::{Core, Input, Outputs, ASKS_PER_WANT, BUSY_BUDGET};
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::phone::SETTLE;
use wrist_bluenrg2::INFO;
use wrist_bluenrg2::shadow::menu::{interval_now_ask, MENU};
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn link() -> ConnHandle {
    ConnHandle::new(0x801)
}

/// What `BlueZ` opens a link with: 30 ms, no latency.
const OPENED: LinkParams = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };
/// The battery ask, as a phone that takes it reports it.
const BATTERY: LinkParams = LinkParams { interval: Duration::from_u16(156), latency: 4, timeout: Duration::from_u16(600) };
/// A phone's own parameters after it moves off the watch's ask, or answers with its own.
const OWN: LinkParams = LinkParams { interval: Duration::from_u16(36), latency: 0, timeout: Duration::from_u16(500) };
/// What macOS opens a link with (`fixtures/traces/mac-*.log`).
const MAC: LinkParams = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(72) };
/// What Android 17 opens a link with (`fixtures/traces/android17-*.log`).
const ANDROID_17: LinkParams = LinkParams { interval: Duration::from_u16(15), latency: 0, timeout: Duration::from_u16(500) };
/// Where Android 17 moves that link by itself.
const ANDROID_17_OWN: LinkParams = LinkParams { interval: Duration::from_u16(39), latency: 0, timeout: Duration::from_u16(500) };

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
        self.phone_up_at(ms, OPENED);
    }

    fn phone_up_at(&mut self, ms: u64, params: LinkParams) {
        self.at(ms, Input::Radio(RadioEvent::LinkUp { link: link(), role: LeConnRole::Peripheral, peer: phone(), params: Some(params) }));
    }

    fn encrypted(&mut self, ms: u64) -> Outputs {
        self.at(ms, Input::Radio(RadioEvent::Encryption { link: link(), on: true }))
    }

    fn params(&mut self, ms: u64, p: LinkParams) -> Outputs {
        self.at(ms, Input::Radio(RadioEvent::Params { link: link(), result: Ok(p) }))
    }

    /// The central's acceptance of the ask on air, then its update to `p`.
    fn granted(&mut self, ms: u64, p: LinkParams) -> Outputs {
        self.at(ms, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: true }));
        self.params(ms, p)
    }
}

fn battery() -> Goals {
    Goals { radio_on: true, power: Power::Battery, ..Goals::OFF }
}

fn cable() -> Goals {
    Goals { radio_on: true, power: Power::Cable, ..Goals::OFF }
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

/// The chip's two key-missing outcomes reach the user: the phone to forget this device, or its
/// bond lost.
#[test]
fn a_missing_key_is_told() {
    use wrist_ble::{PairingEvent, PairingStep};
    use wrist_ble::policy::engine::Tell;
    for (step, tell) in [(PairingStep::LocalKeyMissing, Tell::OldKey(link())), (PairingStep::PeerKeyMissing, Tell::BondLost(link()))] {
        let mut b = Bench::new(battery());
        b.phone_up(1_000);
        let out = b.at(1_200, Input::Radio(RadioEvent::Pairing(PairingEvent { link: link(), step })));
        assert_eq!(out.tell, [Some(tell), None], "{step:?}");
    }
}

/// The core's rules for leaving an ask out, as the shadow reads them: no link, a link not yet open
/// to asks or pairing, a link that already fits, and an ask already made that may not go again yet.
#[test]
fn an_ask_is_left_out_by_the_cores_rules() {
    use wrist_ble::{ConnParams, PairingEvent, PairingStep};
    use wrist_ble::policy::engine::Omitted;
    let fits = ConnParams { interval_min: OPENED.interval, interval_max: OPENED.interval, latency: OPENED.latency, timeout: OPENED.timeout, ce_len: Duration::from_u16(0) };
    let other = ConnParams { latency: OPENED.latency + 1, ..fits };
    let mut b = Bench::new(battery());
    assert_eq!(b.core.omits_ask(b.now, &fits), None, "no link");
    b.phone_up(1_000);
    assert_eq!(b.core.omits_ask(b.now, &other), Some(Omitted::NotOpen));
    b.encrypted(1_100);
    assert_eq!((b.core.omits_ask(b.now, &fits), b.core.omits_ask(b.now, &other)), (Some(Omitted::Fits), None));
    b.at(1_200, Input::Radio(RadioEvent::Pairing(PairingEvent { link: link(), step: PairingStep::Numeric(1) })));
    assert_eq!(b.core.omits_ask(b.now, &other), Some(Omitted::NotOpen), "pairing");
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_100);
    let now = interval_now_ask(6, 6);
    assert_eq!(b.at(1_150, Input::IntervalNow(now)).op, Some(Op::UpdateParams(link(), now)));
    b.accept();
    let driver = ConnParams { ce_len: Duration::from_u16(0), ..now };
    assert_eq!(b.core.omits_ask(b.now, &driver), Some(Omitted::Asked), "the core's own ask, made");
    b.at(1_180, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: false }));
    assert_eq!(b.core.omits_ask(b.now, &driver), Some(Omitted::Asked), "refused, the same ask again");
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_100);
    assert!(b.at(1_150, Input::IntervalNow(now)).op.is_some());
    b.refuse(FailureKind::ModeExclusion);
    assert_eq!(b.at(1_160, Input::IntervalNow(interval_now_ask(8, 8))).op, None, "backing off");
    assert_eq!(b.core.omits_ask(b.now, &driver), None, "a want since replaced");
    let mut b = Bench::new(battery());
    b.phone_up(1_000);
    let Some(Op::UpdateParams(_, menu)) = b.encrypted(1_100).op else { panic!("the menu's ask") };
    b.accept();
    b.at(1_180, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: false }));
    assert_eq!(b.core.omits_ask(b.now, &menu), None, "the menu's ask, which only the console's is excused");
}

/// An ask refused before it reached the air is asked again once the backoff ends.
#[test]
fn an_ask_refused_at_submit_is_asked_again_after_the_backoff() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::NoResources);
    let out = b.at(1_301, Input::Tick);
    assert_eq!(out.op, None);
    let out = b.at(2_300, Input::Tick);
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// An ask refused busy waits for the central to answer the one before, however long that takes;
/// an update completing meanwhile, the central's own or a failed one, is no answer.
#[test]
fn an_ask_refused_busy_waits_for_the_centrals_answer() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    for t in [2_300, 5_000, 20_000] {
        assert_eq!(b.at(t, Input::Tick).op, None, "at {t} ms");
    }
    let params = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };
    let failed = Err(Failure { kind: FailureKind::PeerRejected, native: 0x3B });
    for result in [Ok(params), failed] {
        let out = b.at(20_500, Input::Radio(RadioEvent::Params { link: link(), result }));
        assert_eq!(out.op, None, "{result:?}");
    }
    let out = b.at(21_000, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: false }));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// The central's answer ends the ask; a refusal counts as a failed update.
#[test]
fn the_centrals_answer_ends_the_ask() {
    for accepted in [false, true] {
        let mut b = asked_on_battery();
        b.accept();
        b.at(1_400, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted }));
        assert_eq!(b.core.counters.update_failures, u32::from(!accepted), "accepted {accepted}");
        assert!(b.core.pending().is_none(), "accepted {accepted}");
    }
}

/// An update the central makes before it answers is its own: the ask stays on air until the answer.
#[test]
fn the_centrals_own_update_leaves_the_ask_on_air() {
    let mut b = asked_on_battery();
    b.accept();
    b.params(1_400, OWN);
    assert!(b.core.pending().is_some());
    b.at(1_500, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: true }));
    assert!(b.core.pending().is_none());
}

/// The central's answer ends only the ask: a security request on air stays.
#[test]
fn the_centrals_answer_ends_nothing_but_the_ask() {
    let mut b = Bench::new(battery());
    b.phone_up(1_000);
    let out = b.at(6_100, Input::Tick);
    assert_eq!(out.op, Some(Op::RequestSecurity(link())));
    b.accept();
    b.at(6_200, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: false }));
    assert!(b.core.pending().is_some(), "the security request still on air");
}

/// A failed update counts once; an update of another link is not the phone's.
#[test]
fn a_failed_update_counts_and_another_links_update_is_not_the_phones() {
    let mut b = asked_on_battery();
    b.accept();
    b.granted(1_400, BATTERY);
    let other = wrist_ble::ConnHandle::new(0x0802);
    b.at(2_000, Input::Radio(RadioEvent::Params { link: other, result: Ok(OWN) }));
    let ops = b.run_to(2_000 + u64::from(SETTLE.0) + 10);
    assert_eq!(asks(&ops), 0, "the phone's link still fits: {ops:?}");
    let before = b.core.counters.update_failures;
    b.at(9_000, Input::Radio(RadioEvent::Params { link: link(), result: Err(Failure { kind: FailureKind::PeerRejected, native: 0x3B }) }));
    assert_eq!(b.core.counters.update_failures, before + 1);
}

/// An accepted ask clears the backoff of a refusal before it, so a later refusal waits a second again.
#[test]
fn an_accepted_ask_clears_the_backoff() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::NoResources);
    let first = (2_000..10_000).step_by(100).find(|&t| b.at(t, Input::Tick).op.is_some()).expect("asked again");
    b.accept();
    b.granted(first + 100, BATTERY);
    let plugged = first + 200;
    assert!(b.at(plugged, Input::Goals(cable())).op.is_some(), "the cable's want asked at once");
    b.refuse(FailureKind::NoResources);
    let next = (plugged + 100..plugged + 10_000).step_by(100).find(|&t| b.at(t, Input::Tick).op.is_some()).expect("asked again");
    assert_eq!(next - plugged, 1_000, "a second's backoff, not two");
}

/// A busy ask goes again as soon as the central accepts the one before, before its update lands.
#[test]
fn an_ask_refused_busy_goes_when_the_central_accepts_the_one_before() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    assert_eq!(b.at(2_300, Input::Tick).op, None);
    let out = b.at(2_400, Input::Radio(RadioEvent::ParamsAnswered { link: link(), accepted: true }));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// An ask the chip never took is not held for an answer: it backs off as any refusal does.
#[test]
fn an_ask_the_chip_never_took_is_not_held() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::NotTaken);
    let out = b.at(2_300, Input::Tick);
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// A busy ask whose central never answers is asked again at the operation's deadline.
#[test]
fn an_ask_refused_busy_is_asked_again_if_the_central_never_answers() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    assert_eq!(b.at(31_000, Input::Tick).op, None);
    let out = b.at(31_301, Input::Tick);
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

/// A phone whose own update moves a link that fitted the want off it is asked
/// again once it has settled, and at most [`ASKS_PER_WANT`] times for one want.
#[test]
fn a_phone_that_moves_off_a_fitting_link_is_asked_again_within_the_budget() {
    let mut b = asked_on_battery();
    b.accept();
    let mut at = 1_500;
    let mut total = 1;
    for _ in 0..5 {
        b.granted(at, BATTERY);
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
    b.granted(1_500, BATTERY);
    b.params(3_000, OWN);
    assert_eq!(asks(&b.run_to(3_000 + u64::from(SETTLE.0) - 1)), 0);
    assert_eq!(asks(&b.run_to(3_000 + u64::from(SETTLE.0) + 1)), 1);
}

#[test]
fn a_link_that_has_the_ask_already_is_not_asked() {
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_300);
    assert_eq!(asks(&b.run_to(120_000)), 0);
}

#[test]
fn a_link_with_another_supervision_timeout_is_asked() {
    let mut b = Bench::new(cable());
    b.phone_up_at(1_000, MAC);
    let out = b.encrypted(1_300);
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// A link that fitted before any ask and then moved off by the phone's own update is asked once
/// that has settled.
#[test]
fn a_link_the_phone_moves_off_the_want_by_itself_is_asked() {
    let mut b = Bench::new(cable());
    b.phone_up_at(1_000, ANDROID_17);
    assert_eq!(b.encrypted(1_170).op, None);
    b.params(1_320, ANDROID_17_OWN);
    assert_eq!(asks(&b.run_to(1_320 + u64::from(SETTLE.0) - 1)), 0);
    assert_eq!(asks(&b.run_to(1_320 + u64::from(SETTLE.0) + 1)), 1);
    assert_eq!(b.core.counters.reasks, 0);
}

/// macOS answers an ask with its own parameters (`fixtures/traces/mac-cable.log`).
#[test]
fn a_phone_that_answers_the_ask_with_its_own_is_not_asked_again() {
    let mut b = asked_on_battery();
    b.accept();
    b.params(1_500, OWN);
    assert_eq!(asks(&b.run_to(120_000)), 0);
}

/// An answer to the chip's own procedure goes out past an operation on air: the rebond a lost
/// bond needs passes the security request waiting on it, and nothing asks for parameters before
/// the link is encrypted.
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

/// I2: a chip that says busy to everything, or takes nothing, is restarted once its budget is
/// spent, not retried for ever.
#[test]
fn a_chip_that_stays_busy_is_restarted() {
    for kind in [FailureKind::Busy, FailureKind::NotTaken] {
        let mut b = Bench::new(battery());
        let out = b.at(1_000, Input::Goals(stop_advertising()));
        assert_eq!(out.op, Some(Op::Advertise(None)), "{out:?}");
        let mut refusals = 0;
        let mut restarted = None;
        while b.now.0 < 600_000 && restarted.is_none() {
            if b.asked.is_some() {
                b.refuse(kind);
                refusals += 1;
            }
            let next = b.core.step(b.now, Input::Tick).next.expect("a wake");
            let out = b.at(next.0, Input::Tick);
            if matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))) {
                restarted = Some(b.now);
            }
        }
        assert_eq!(refusals, usize::from(BUSY_BUDGET), "{kind:?}");
        assert_eq!(b.core.counters.busy_spent, 1, "{kind:?}");
        assert_eq!(b.core.tally.restarts.rows[0], Some((wrist_ble::policy::tally::Restart::Busy, 1)), "{kind:?}");
        assert!(restarted.is_some_and(|t| t.0 < 150_000), "{kind:?}: {restarted:?}");
    }
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
/// of the encrypted link's parameters is let go, and it is judged afresh.
#[test]
fn a_link_a_resync_finds_again_is_judged_afresh() {
    let mut b = Bench::new(Goals { radio_on: true, power: Power::Cable, ..Goals::OFF });
    b.phone_up(1_000);
    let out = b.at(1_300, Input::Radio(RadioEvent::Encryption { link: link(), on: true }));
    assert_eq!(out.op, None, "30 ms fits the cable's ask: {out:?}");
    let out = b.at(2_000, Input::Radio(RadioEvent::EventsLost));
    assert!(out.snapshot);
    let mut links = [None; MAX_LINKS];
    links[0] = Some(wrist_ble::LinkStatus { link: link(), role: LeConnRole::Peripheral, peer: phone(), encrypted: Some(true) });
    let snap = Snapshot { at: Instant(2_001), links, advertising: false, scanning: false, initiating: false, anchor: None };
    let out = b.at(2_001, Input::Snapshot(Ok(snap)));
    assert!(matches!(out.op, Some(Op::UpdateParams(l, _)) if l == link()), "{out:?}");
}

/// A chip that answers nothing but busy is restarted whoever is connected, ending the phone's link.
#[test]
fn a_chip_that_stays_busy_is_restarted_under_the_phones_link() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    let mut restarted = 0;
    for i in 1..u16::from(BUSY_BUDGET) {
        // A new want each time (the console's ask), so each is asked at once and refused busy.
        let now = wrist_bluenrg2::shadow::menu::interval_now_ask(8 + i, 8 + i);
        let t = 2_000 + u64::from(i) * 40_000;
        let out = b.at(t, Input::IntervalNow(now));
        let out = if out.op.is_some() { out } else { b.at(t + 33_000, Input::Tick) };
        assert!(matches!(out.op, Some(Op::UpdateParams(..))), "{i}: {out:?}");
        let out = b.refuse(FailureKind::Busy);
        restarted += usize::from(matches!(out.chip, Some(wrist_ble::policy::chip::Action::Start(_))));
    }
    assert_eq!(restarted, 1, "{:?}", b.core.counters);
    assert_eq!(b.core.counters.busy_spent, 1);
    assert_eq!(b.core.state.phone.link(), None);
}

/// Busy refusals slower than the refill stay within the budget.
#[test]
fn a_busy_refusal_every_nine_minutes_keeps_its_refills() {
    let mut b = Bench::new(battery());
    for i in 0..40u64 {
        let t = 1_000 + i * 540_000;
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

/// A chip restarted for another cause starts with the whole budget, whatever the last one spent.
#[test]
fn a_restarted_chip_has_the_whole_busy_budget() {
    let mut b = Bench::new(battery());
    b.at(1_000, Input::Goals(stop_advertising()));
    for _ in 1..BUSY_BUDGET {
        b.refuse(FailureKind::Busy);
        let next = b.core.step(b.now, Input::Tick).next.expect("a retry");
        b.at(next.0, Input::Tick);
    }
    assert_eq!(b.core.counters.busy_spent, 0);
    let out = b.at(b.now.0 + 1, Input::Radio(RadioEvent::ChipReset(wrist_ble::ResetCause::Watchdog)));
    let Some(wrist_ble::policy::chip::Action::Start(start)) = out.chip else { panic!("{out:?}") };
    b.at(b.now.0 + 1, Input::Started(start, Ok(INFO)));
    let id = b.accept();
    b.at(b.now.0 + 1, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
    let out = b.at(b.now.0 + 1_000, Input::Goals(battery()));
    assert!(matches!(out.op, Some(Op::Advertise(_))), "{out:?}");
    b.refuse(FailureKind::Busy);
    assert_eq!(b.core.counters.busy_spent, 0);
}

/// `n` busy refusals from `ms`, each retried after its backoff, then the retry accepted.
fn busy(b: &mut Bench, ms: u64, goals: Goals, n: u32) {
    let out = b.at(ms, Input::Goals(goals));
    assert!(out.op.is_some(), "{out:?}");
    for _ in 0..n {
        b.refuse(FailureKind::Busy);
        let next = b.core.step(b.now, Input::Tick).next.expect("a retry");
        b.at(next.0, Input::Tick);
    }
    let id = b.accept();
    b.at(b.now.0 + 1, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
}

/// A token given back starts the next one's period where its own ended, not when it was taken.
#[test]
fn a_token_given_back_is_not_given_back_again() {
    let mut b = Bench::new(battery());
    busy(&mut b, 1_000, stop_advertising(), 6);
    busy(&mut b, 660_000, battery(), 1);
    b.at(720_000, Input::Goals(stop_advertising()));
    b.refuse(FailureKind::Busy);
    assert_eq!(b.core.counters.busy_spent, 1);
}

/// A full bucket earns nothing while it stays full.
#[test]
fn a_full_bucket_earns_no_credit() {
    let mut b = Bench::new(battery());
    busy(&mut b, 1_000, stop_advertising(), 1);
    busy(&mut b, 900_000, battery(), 6);
    b.at(1_260_000, Input::Goals(stop_advertising()));
    b.refuse(FailureKind::Busy);
    assert_eq!(b.core.counters.busy_spent, 1);
}

/// The console's ask is held to the link up when it came, so a link that connects later is not
/// asked it.
#[test]
fn the_consoles_ask_with_no_link_is_ignored() {
    let mut b = Bench::new(cable());
    let now = interval_now_ask(6, 6);
    assert_eq!(b.at(500, Input::IntervalNow(now)).op, None);
    b.phone_up(1_000);
    b.encrypted(1_300);
    assert_eq!(asks(&b.run_to(60_000)), 0, "30 ms fits the cable's ask");
    assert_eq!(b.core.phone_want(), Some(MENU.phone[0].params));
}

/// An ask no request may carry is ignored, 2000 being past what `interval_now_ask`'s supervision
/// timeout covers (Core 6.0 Vol 6 Part B §4.5.2).
#[test]
fn the_consoles_ask_no_request_may_carry_is_ignored() {
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_300);
    for (i, (min, max)) in [(5, 5), (7, 6), (2_000, 2_000), (3_201, 3_201)].into_iter().enumerate() {
        let out = b.at(2_000 + 1_000 * i as u64, Input::IntervalNow(interval_now_ask(min, max)));
        assert_eq!(out.op, None, "{min}-{max}");
        assert_eq!(b.core.phone_want(), Some(MENU.phone[0].params), "{min}-{max}");
    }
}

/// The console's ask waits for encryption as the menu's does.
#[test]
fn the_consoles_ask_waits_for_encryption() {
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    let now = interval_now_ask(6, 6);
    assert_eq!(b.at(1_100, Input::IntervalNow(now)).op, None);
    assert_eq!(asks(&b.run_to(4_000)), 0);
    assert_eq!(b.encrypted(4_100).op, Some(Op::UpdateParams(link(), now)));
}

/// The console's ask is made only where the link doesn't fit it, and, repeated or not, no more
/// than [`ASKS_PER_WANT`] times on one link.
#[test]
fn the_consoles_ask_is_made_only_where_the_link_does_not_fit_it() {
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_300);
    assert_eq!(b.at(2_000, Input::IntervalNow(interval_now_ask(20, 30))).op, None, "30 ms fits 20 to 30");
    let now = interval_now_ask(12, 12);
    assert_eq!(b.at(3_000, Input::IntervalNow(now)).op, Some(Op::UpdateParams(link(), now)));
    b.accept();
    let granted = LinkParams { interval: Duration::from_u16(12), latency: 0, timeout: Duration::from_u16(500) };
    let mut at = 3_200;
    let mut total = 1;
    for _ in 0..5 {
        b.granted(at, granted);
        assert_eq!(b.at(at + 100, Input::IntervalNow(now)).op, None, "fits");
        b.params(at + 2_000, OWN);
        assert_eq!(b.at(at + 2_100, Input::IntervalNow(now)).op, None, "the same ask, not yet settled");
        total += asks(&b.run_to(at + 2_000 + u64::from(SETTLE.0) + 10));
        at += 20_000;
        b.run_to(at - 1);
    }
    assert_eq!(total, usize::from(ASKS_PER_WANT));
}

/// The console's ask lasts until its link ends or the menu's own ask changes.
#[test]
fn the_consoles_ask_ends_with_its_link_or_the_menus_ask() {
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_300);
    let now = interval_now_ask(12, 12);
    b.at(2_000, Input::IntervalNow(now));
    b.accept();
    b.at(3_000, Input::Goals(Goals { transfer: true, ..cable() }));
    assert_eq!(b.core.phone_want(), Some(now), "a transfer on the cable");
    b.at(4_000, Input::Goals(battery()));
    assert_eq!(b.core.phone_want(), Some(MENU.phone[1].params), "the battery");
    b.at(5_000, Input::IntervalNow(now));
    assert_eq!(b.core.phone_want(), Some(now));
    b.at(6_000, Input::Radio(RadioEvent::LinkDown { link: link(), reason: Status::REMOTE_USER_TERMINATED_CONN }));
    assert_eq!(b.core.phone_want(), Some(MENU.phone[1].params), "the link ended");
    b.phone_up(7_000);
    assert_eq!(b.core.phone_want(), Some(MENU.phone[1].params), "nor the next link");
}

/// A snapshot can't show that the link on the phone's handle is the one the console asked for, so
/// a resync ends the ask, whatever it finds of the link's encryption.
#[test]
fn the_consoles_ask_ends_with_a_resync() {
    let resync = |b: &mut Bench, ms: u64, encrypted: bool| {
        b.at(ms, Input::Radio(RadioEvent::EventsLost));
        let mut links = [None; MAX_LINKS];
        links[0] = Some(wrist_ble::LinkStatus { link: link(), role: LeConnRole::Peripheral, peer: phone(), encrypted: Some(encrypted) });
        b.at(ms + 1, Input::Snapshot(Ok(Snapshot { at: Instant(ms + 1), links, advertising: false, scanning: false, initiating: false, anchor: None })))
    };
    let mut b = Bench::new(cable());
    b.phone_up(1_000);
    b.encrypted(1_300);
    let now = interval_now_ask(12, 12);
    for (at, encrypted) in [(2_000, true), (5_000, false)] {
        b.at(at, Input::IntervalNow(now));
        assert_eq!(b.core.phone_want(), Some(now));
        resync(&mut b, at + 1_000, encrypted);
        assert_eq!(b.core.phone_want(), Some(MENU.phone[0].params), "found encrypted: {encrypted}");
    }
}

fn pending(b: &Bench) -> Option<Op> {
    b.core.pending().map(|p| p.op)
}

/// A central's link ends a connection on air, and nothing else.
#[test]
fn a_central_link_ends_only_a_connection() {
    let mut b = asked_on_battery();
    b.accept();
    let strap = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([1, 2, 3, 4, 5, 6]) };
    b.at(1_400, Input::Radio(RadioEvent::LinkUp { link: ConnHandle::new(0x802), role: LeConnRole::Central, peer: strap, params: Some(OPENED) }));
    assert!(matches!(pending(&b), Some(Op::UpdateParams(..))), "{:?}", pending(&b));
}

/// The link's security events end its security request, and leave its parameter ask on air.
#[test]
fn security_events_end_the_security_request_alone() {
    use wrist_ble::{PairingEvent, PairingStep};
    let mut b = asked_on_battery();
    b.accept();
    b.at(1_400, Input::Radio(RadioEvent::Encryption { link: link(), on: true }));
    b.at(1_500, Input::Radio(RadioEvent::Pairing(PairingEvent { link: link(), step: PairingStep::Complete })));
    assert!(matches!(pending(&b), Some(Op::UpdateParams(..))), "{:?}", pending(&b));
    for end in [RadioEvent::Encryption { link: link(), on: true }, RadioEvent::Pairing(PairingEvent { link: link(), step: PairingStep::Complete })] {
        let mut b = Bench::new(battery());
        b.phone_up(1_000);
        assert_eq!(b.at(6_000, Input::Tick).op, Some(Op::RequestSecurity(link())));
        b.accept();
        assert_eq!(pending(&b), Some(Op::RequestSecurity(link())));
        b.at(6_500, Input::Radio(end));
        assert_eq!(pending(&b), None, "{end:?}");
    }
}

/// A security request waits behind its own link's parameter ask.
#[test]
fn a_security_request_waits_for_its_links_ask() {
    let mut b = asked_on_battery();
    let id = b.accept();
    let mut links = [None; MAX_LINKS];
    links[0] = Some(wrist_ble::LinkStatus { link: link(), role: LeConnRole::Peripheral, peer: phone(), encrypted: Some(false) });
    let s = Snapshot { at: Instant(2_000), links, advertising: false, scanning: false, initiating: false, anchor: None };
    b.at(2_000, Input::Snapshot(Ok(s)));
    assert_eq!(b.at(7_000, Input::Tick).op, None);
    assert!(matches!(pending(&b), Some(Op::UpdateParams(..))));
    let out = b.at(7_100, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
    assert_eq!(out.op, Some(Op::RequestSecurity(link())), "{out:?}");
}

#[test]
fn link_losses_and_unknown_events_are_counted() {
    let mut b = Bench::new(battery());
    b.phone_up(1_000);
    b.at(2_000, Input::Radio(RadioEvent::LinkDown { link: link(), reason: Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE }));
    let c = b.core.counters;
    assert_eq!((c.mic_failures, c.instant_passed), (1, 0));
    b.phone_up(3_000);
    b.at(4_000, Input::Radio(RadioEvent::LinkDown { link: link(), reason: Status::INSTANT_PASSED }));
    let c = b.core.counters;
    assert_eq!((c.mic_failures, c.instant_passed), (1, 1));
    b.at(5_000, Input::Radio(RadioEvent::Unknown { raw_code: 0x0C99 }));
    assert_eq!(b.core.counters.unknown_events, 1);
}

#[test]
fn failed_operations_are_counted_and_their_timeouts_apart() {
    let mut b = asked_on_battery();
    let id = b.accept();
    b.at(1_400, Input::Radio(RadioEvent::Done(id, Err(Failure { kind: FailureKind::Timeout, native: 0 }))));
    let c = b.core.counters;
    assert_eq!((c.failed, c.timed_out), (1, 1));
    let mut b = Bench::new(battery());
    assert_eq!(b.at(1_000, Input::Goals(stop_advertising())).op, Some(Op::Advertise(None)));
    let id = b.accept();
    b.at(1_100, Input::Radio(RadioEvent::Done(id, Err(Failure { kind: FailureKind::NoResources, native: 0x07 }))));
    let c = b.core.counters;
    assert_eq!((c.failed, c.timed_out, c.submitted), (1, 0, 2));
}

/// An operation past its deadline has a grace for the driver to report it, then the core ends it.
#[test]
fn an_operation_past_its_deadline_is_ended_after_its_grace() {
    let mut b = asked_on_battery();
    b.accept();
    let deadline = b.core.pending().unwrap().deadline;
    b.at(deadline.0 + 999, Input::Tick);
    assert!(pending(&b).is_some());
    assert_eq!(b.core.counters.timed_out, 0);
    b.at(deadline.0 + 1_000, Input::Tick);
    assert_eq!(pending(&b), None);
    assert_eq!(b.core.counters.timed_out, 1);
}

/// A snapshot puts right the record it disagrees with, and counts a disagreement on advertising.
#[test]
fn a_snapshot_puts_the_record_right_and_counts_it() {
    let scan = wrist_ble::ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(160), active: false, duration: wrist_ble::Millis::secs(5) };
    let snapshot = |scanning| Snapshot { at: Instant(0), links: [None; MAX_LINKS], advertising: true, scanning, initiating: false, anchor: None };
    let mut b = Bench::new(battery());
    b.core.state.radio.scanning = Some(scan);
    b.at(1_000, Input::Snapshot(Ok(snapshot(true))));
    assert_eq!((b.core.state.radio.scanning, b.core.counters.audit_mismatches), (Some(scan), 0));
    b.at(2_000, Input::Snapshot(Ok(snapshot(false))));
    assert_eq!(b.core.state.radio.scanning, None);
    let before = b.core.counters.audit_mismatches;
    b.at(3_000, Input::Snapshot(Ok(Snapshot { advertising: false, ..snapshot(false) })));
    assert_eq!(b.core.counters.audit_mismatches, before + 1);
}

/// The drift is timed from when the radio left the plan, and ends when it is back.
#[test]
fn drift_runs_while_the_radio_is_off_the_plan() {
    let mut b = Bench::new(battery());
    assert_eq!(b.core.state.drift_since, None);
    assert_eq!(b.at(2_000, Input::Goals(stop_advertising())).op, Some(Op::Advertise(None)));
    let id = b.accept();
    assert_eq!(wrist_ble::policy::status::status(&b.core.state, Instant(5_000)).drift, wrist_ble::Millis::secs(3));
    b.at(5_000, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
    assert_eq!(b.core.state.drift_since, None);
}

/// After a tick, the core's next wake is later than now: a wake at or before it would spin.
fn tick(b: &mut Bench, ms: u64) -> Outputs {
    let out = b.at(ms, Input::Tick);
    assert!(out.next.is_none_or(|t| t > b.now), "a wake at {:?} named at {:?}", out.next, b.now);
    out
}

/// The core wakes when a busy ask may go again, and asks then.
#[test]
fn the_core_wakes_when_a_busy_ask_may_go_again() {
    let mut b = asked_on_battery();
    b.refuse(FailureKind::Busy);
    let due = Instant(1_300) + (CAPABILITIES.op_deadline)(wrist_ble::OpKind::UpdateParams);
    assert_eq!(tick(&mut b, 1_400).next, Some(due));
    assert_eq!(tick(&mut b, due.0 - 1).op, None);
    assert!(matches!(tick(&mut b, due.0).op, Some(Op::UpdateParams(l, _)) if l == link()));
}

/// The core wakes when a phone that moved off the ask has settled, and asks then.
#[test]
fn the_core_wakes_when_the_phone_has_settled_off_the_ask() {
    let mut b = asked_on_battery();
    b.accept();
    b.granted(1_500, BATTERY);
    b.params(3_000, OWN);
    let due = Instant(3_000) + SETTLE;
    assert_eq!(tick(&mut b, 3_001).next, Some(due));
    assert!(matches!(tick(&mut b, due.0).op, Some(Op::UpdateParams(l, _)) if l == link()));
}

/// A link judged to fit and moved off it within the same millisecond is asked once it settles.
#[test]
fn a_link_moved_off_the_want_as_it_is_judged_is_asked() {
    let mut b = Bench::new(cable());
    b.phone_up_at(1_000, ANDROID_17);
    assert_eq!(b.encrypted(1_170).op, None);
    b.params(1_170, ANDROID_17_OWN);
    assert_eq!(asks(&b.run_to(1_170 + u64::from(SETTLE.0) + 1)), 1);
}

/// No wake is named for an ask that cannot go: its budget spent, or the link pairing again.
#[test]
fn no_wake_is_named_for_an_ask_that_cannot_go() {
    let mut b = asked_on_battery();
    let mut next = b.refuse(FailureKind::Busy).next;
    let mut asked = 1;
    while let Some(due) = next.filter(|t| t.0 < 200_000) {
        next = tick(&mut b, due.0).next;
        if b.asked.is_some() {
            asked += 1;
            next = b.refuse(FailureKind::Busy).next;
        }
    }
    assert_eq!(asked, usize::from(ASKS_PER_WANT));

    let mut b = asked_on_battery();
    b.accept();
    b.granted(1_500, BATTERY);
    b.params(3_000, OWN);
    b.at(3_500, Input::Radio(RadioEvent::Pairing(wrist_ble::PairingEvent { link: link(), step: wrist_ble::PairingStep::Numeric(123_456) })));
    for ms in [3_000 + u64::from(SETTLE.0), 7_000, 8_000] {
        assert_eq!(tick(&mut b, ms).op, None);
    }

    let mut b = asked_on_battery();
    b.accept();
    let mut at = 1_500;
    for _ in 0..ASKS_PER_WANT {
        b.granted(at, BATTERY);
        b.params(at + 2_000, OWN);
        b.run_to(at + 2_000 + u64::from(SETTLE.0) + 10);
        at += 20_000;
    }
    b.granted(at, BATTERY);
    b.params(at + 2_000, OWN);
    assert_eq!(tick(&mut b, at + 2_001).next, None);
}
