//! Each check fails when the truth is broken behind the core's back.

use wrist_ble::{AdvParams, Duration, Instant, Millis, Op, OpId, ScanParams, Status};
use wrist_ble_core::chip::Chip;
use wrist_ble_core::engine::{Core, Input};
use wrist_ble_core::goals::Power;
use wrist_ble_core::invariants::{COMMANDS_PER_MINUTE, CONVERGE_AFTER, REACHABLE_GAP, REACHABLE_SHARE_PERCENT, REACHABLE_WINDOW, STATUS_WITHIN};
use wrist_ble_core::phone::{self, Security, SETTLE};
use wrist_ble_core::State;
use wrist_ble_sim::checks::Checks;
use wrist_ble_sim::fake::{Fake, Note, INFO};
use wrist_ble_sim::menu::MENU;
use wrist_ble_sim::peers::{Os, Phone};
use wrist_ble_sim::sim::{on, Sim};
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn noted(note: Note) {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.notes.push((s.now, note));
    s.run_for(Millis(100));
}

/// Runs in steps of 10 ms until the phone's link is up, failing if a minute of steps does not
/// bring it up.
fn linked(s: &mut Sim) {
    for _ in 0..6_000 {
        if s.fake.phone_link().is_some() {
            return;
        }
        s.run_for(Millis(10));
    }
    panic!("the phone never connected\n{}", s.report());
}

fn phone_up() -> Sim {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    s.run_for(Millis::secs(5));
    s
}

#[test]
#[should_panic(expected = "security order: a procedure crossed encryption")]
fn a_procedure_across_encryption() {
    let s = phone_up();
    let l = s.fake.phone_link().unwrap().0;
    noted(Note::CrossedEncryption(l));
}

#[test]
#[should_panic(expected = "phone first: a central start during the update")]
fn a_central_start_during_an_update() {
    let s = phone_up();
    noted(Note::CentralInUpdate(s.fake.phone_link().unwrap().0));
}

#[test]
#[should_panic(expected = "security order: MIC failure")]
fn a_mic_failure() {
    let s = phone_up();
    noted(Note::LinkDown { link: s.fake.phone_link().unwrap().0, reason: Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE });
}

#[test]
#[should_panic(expected = "phone first: instant passed")]
fn an_instant_passed() {
    let s = phone_up();
    noted(Note::LinkDown { link: s.fake.phone_link().unwrap().0, reason: Status::INSTANT_PASSED });
}

#[test]
#[should_panic(expected = "bounded effort")]
fn too_many_commands() {
    let mut s = Sim::started(1, on(Power::Battery));
    for _ in 0..=60 {
        s.fake.notes.push((s.now, Note::Submitted { op: Op::Advertise(None), accepted: true }));
    }
    s.run_for(Millis(100));
}

#[test]
#[should_panic(expected = "phone first: Scan")]
fn a_scan_as_the_phone_connects() {
    let mut s = phone_up();
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(3));
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    linked(&mut s);
    let scan = ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(32), active: false, duration: Millis::secs(10) };
    s.fake.notes.push((s.now, Note::Submitted { op: Op::Scan(Some(scan)), accepted: true }));
    s.run_for(Millis(10));
}

#[test]
#[should_panic(expected = "reachable: a gap")]
fn advertising_stopped() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.submit(Op::Advertise(None)).unwrap();
    s.run_for(Millis::secs(5));
}

#[test]
#[should_panic(expected = "truthful status: discoverable")]
fn advertising_stopped_while_the_status_says_discoverable() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.submit(Op::Advertise(None)).unwrap();
    for _ in 0..50 {
        s.run_for(Millis(100));
    }
}

#[test]
#[should_panic(expected = "converges")]
fn advertising_retimed() {
    let mut s = Sim::started(1, on(Power::Battery));
    let fast = AdvParams { interval_min: Duration::from_u16(160), interval_max: Duration::from_u16(160), connectable: true };
    s.fake.submit(Op::Advertise(Some(fast))).unwrap();
    s.run_for(Millis::secs(20));
}

const MS: Millis = Millis(1);

fn scan() -> Op {
    Op::Scan(Some(ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(32), active: false, duration: Millis::secs(10) }))
}

/// A started watch, advertising, with checks of its own that the test calls at times it picks.
struct Bench {
    s: Sim,
    c: Checks,
    /// The advertising on air, and as the core sees it.
    air: AdvParams,
    seen: AdvParams,
}

impl Bench {
    fn new() -> Self {
        let s = Sim::started(1, on(Power::Battery));
        let air = s.fake.advertising().unwrap();
        let seen = s.core.state.radio.advertising.unwrap();
        Self { s, c: Checks::default(), air, seen }
    }

    fn at(&self, ms: u32) -> Instant {
        self.s.now + Millis(ms)
    }

    fn check(&mut self, t: Instant) -> Result<(), String> {
        self.c.after(&self.s.core, &self.s.fake, t)
    }

    /// Advertising stops on air and in the core's view, as for a planned gap.
    fn silence(&mut self, t: Instant) {
        self.s.fake.submit(Op::Advertise(None)).unwrap();
        self.s.core.state.radio.advertising = None;
        self.s.core.state.silent_since = t;
        self.c.disturbed(t);
    }

    fn resume(&mut self, t: Instant) {
        self.s.fake.submit(Op::Advertise(Some(self.air))).unwrap();
        self.s.core.state.radio.advertising = Some(self.seen);
        self.c.disturbed(t);
    }

    /// A gap from `from` for `len`, checked as it opens and as it closes.
    fn gap(&mut self, from: Instant, len: Millis) -> Result<(), String> {
        self.silence(from);
        self.check(from)?;
        self.resume(from + len);
        self.check(from + len)
    }
}

/// The state of a ready chip with no phone and no advertising since `silent_since`.
fn silent(silent_since: Instant) -> State {
    let mut s = State::OFF;
    s.chip.state = Chip::Ready(INFO);
    s.phone = phone::Phone::Discoverable;
    s.silent_since = silent_since;
    s
}

/// The status of [`silent`] turns from discoverable to unreachable at this instant.
fn turns(silent_since: Instant) -> Instant {
    silent_since + REACHABLE_GAP + MS
}

#[test]
fn the_status_may_change_class_a_second_before_the_core_runs() {
    let mut c = Checks::default();
    let st = silent(Instant(0));
    c.before(&st, turns(Instant(0)) - Millis::secs(1)).unwrap();
    c.before(&st, turns(Instant(0)) + STATUS_WITHIN).unwrap();
}

#[test]
fn the_status_may_not_change_class_more_than_a_second_before_the_core_runs() {
    let mut c = Checks::default();
    let st = silent(Instant(0));
    c.before(&st, turns(Instant(0)) - Millis::secs(1)).unwrap();
    let e = c.before(&st, turns(Instant(0)) + STATUS_WITHIN + MS).unwrap_err();
    assert!(e.starts_with("truthful status: it became Unreachable"), "{e}");
}

#[test]
fn a_status_that_holds_its_class_may_wait_any_time_for_the_core() {
    let mut c = Checks::default();
    let mut st = silent(Instant(0));
    st.radio.advertising = Some(AdvParams { interval_min: Duration::from_u16(160), interval_max: Duration::from_u16(160), connectable: true });
    c.before(&st, Instant(0)).unwrap();
    c.before(&st, Instant(0) + Millis::mins(60)).unwrap();
}

/// The phone's link comes up at `up` in the checks' view, then the watch starts a scan at `scan_at`.
fn scan_after_the_phone(up: Instant, scan_at: Instant) -> Result<(), String> {
    let mut s = phone_up();
    let mut c = Checks::default();
    c.after(&s.core, &s.fake, up)?;
    s.fake.notes.push((scan_at, Note::Submitted { op: scan(), accepted: true }));
    c.after(&s.core, &s.fake, scan_at)
}

#[test]
fn a_central_start_may_come_once_the_phone_has_settled() {
    let up = phone_up().now;
    scan_after_the_phone(up, up + SETTLE).unwrap();
}

#[test]
fn a_central_start_may_not_come_before_the_phone_has_settled() {
    let up = phone_up().now;
    let e = scan_after_the_phone(up, up + SETTLE - MS).unwrap_err();
    assert!(e.starts_with("phone first: Scan"), "{e}");
}

/// Checks at `t`, once every note so far is a minute old, after `n` commands at `t`.
fn commands(n: u32) -> Result<(), String> {
    let mut b = Bench::new();
    let t = b.at(0) + Millis::mins(1);
    b.check(t)?;
    for _ in 0..n {
        b.s.fake.notes.push((t, Note::Submitted { op: Op::Advertise(None), accepted: true }));
    }
    b.check(t)
}

#[test]
fn the_commands_a_minute_may_reach_the_limit() {
    commands(COMMANDS_PER_MINUTE).unwrap();
}

#[test]
fn the_commands_a_minute_may_not_pass_the_limit() {
    let e = commands(COMMANDS_PER_MINUTE + 1).unwrap_err();
    assert_eq!(e, format!("bounded effort: {} commands in a minute", COMMANDS_PER_MINUTE + 1));
}

#[test]
fn a_command_a_minute_old_no_longer_counts() {
    let mut b = Bench::new();
    let t = b.at(0) + Millis::mins(1);
    for _ in 0..COMMANDS_PER_MINUTE {
        b.s.fake.notes.push((t, Note::Submitted { op: Op::Advertise(None), accepted: true }));
    }
    b.check(t).unwrap();
    b.s.fake.notes.push((t + Millis::mins(1), Note::Submitted { op: Op::Advertise(None), accepted: true }));
    b.check(t + Millis::mins(1)).unwrap();
}

/// A core whose first operation is in flight towards a chip that is off.
fn waiting() -> (Core<'static>, Fake, Instant) {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let now = Instant(0);
    core.step(now, Input::Goals(on(Power::Battery)));
    let mut out = core.step(now, Input::Started(Ok(INFO)));
    for _ in 0..10 {
        if out.op.is_some() {
            break;
        }
        out = core.step(now, Input::Tick);
    }
    assert!(out.op.is_some(), "the core asked for nothing");
    core.step(now, Input::Submitted(Ok(OpId(1))));
    let deadline = core.pending().unwrap().deadline;
    (core, Fake::new(1), deadline)
}

#[test]
fn an_operation_may_run_a_second_past_its_deadline() {
    let (core, fake, deadline) = waiting();
    Checks::default().after(&core, &fake, deadline + Millis::secs(1)).unwrap();
}

#[test]
fn an_operation_may_not_run_longer_past_its_deadline() {
    let (core, fake, deadline) = waiting();
    let e = Checks::default().after(&core, &fake, deadline + Millis::secs(1) + MS).unwrap_err();
    assert!(e.starts_with("nothing silent: Advertise"), "{e}");
}

#[test]
fn a_gap_may_last_the_longest_planned() {
    let mut b = Bench::new();
    b.gap(b.at(0), REACHABLE_GAP).unwrap();
}

#[test]
fn a_gap_may_not_outlast_the_longest_planned() {
    let mut b = Bench::new();
    let e = b.gap(b.at(0), REACHABLE_GAP + MS).unwrap_err();
    assert!(e.starts_with("reachable: a gap"), "{e}");
}

/// The share of a window that gaps may take, in ms.
fn share() -> Millis {
    Millis(REACHABLE_WINDOW.0 * REACHABLE_SHARE_PERCENT / 100)
}

#[test]
fn gaps_may_add_up_to_their_share_of_a_minute() {
    let mut b = Bench::new();
    let first = Millis(share().0 / 2);
    b.gap(b.at(0), first).unwrap();
    b.gap(b.at(10_000), Millis(share().0 - first.0)).unwrap();
}

#[test]
fn gaps_may_not_add_up_to_more_than_their_share_of_a_minute() {
    let mut b = Bench::new();
    let first = Millis(share().0 / 2);
    b.gap(b.at(0), first).unwrap();
    let e = b.gap(b.at(10_000), Millis(share().0 - first.0 + 1)).unwrap_err();
    assert_eq!(e, format!("reachable: {} ms of gaps in a minute", share().0 + 1));
}

/// A gap of half the share, then the rest of the share and a ms more in a second gap that ends
/// `since` after the first ended.
fn gaps_apart(since: Millis) -> Result<(), String> {
    let mut b = Bench::new();
    let first = Millis(share().0 / 2);
    let second = Millis(share().0 - first.0 + 1);
    b.gap(b.at(0), first)?;
    let end = b.at(first.0) + since;
    b.gap(Instant(end.0 - u64::from(second.0)), second)
}

#[test]
fn a_gap_counts_while_it_is_less_than_a_window_old() {
    let e = gaps_apart(Millis(REACHABLE_WINDOW.0 - MS.0)).unwrap_err();
    assert!(e.ends_with("ms of gaps in a minute"), "{e}");
}

#[test]
fn a_gap_no_longer_counts_once_it_is_a_window_old() {
    gaps_apart(REACHABLE_WINDOW).unwrap();
}

/// Advertising re-timed on air behind the core's back, checked `still` after the last disturbance.
fn retimed(still: Millis) -> Result<(), String> {
    let mut b = Bench::new();
    let fast = AdvParams { interval_min: Duration::from_u16(160), interval_max: Duration::from_u16(160), connectable: true };
    b.s.fake.submit(Op::Advertise(Some(fast))).unwrap();
    let t = b.at(0);
    b.c.disturbed(t);
    b.check(t + still)
}

#[test]
fn the_radio_need_not_match_the_plan_before_it_has_been_left_alone_long_enough() {
    retimed(Millis(CONVERGE_AFTER.0 - 1)).unwrap();
}

#[test]
fn the_radio_must_match_the_plan_once_it_has_been_left_alone_long_enough() {
    let e = retimed(CONVERGE_AFTER).unwrap_err();
    assert!(e.starts_with("converges"), "{e}");
}

#[test]
fn a_chip_that_is_not_ready_is_not_held_to_the_plan() {
    let mut b = Bench::new();
    b.s.fake.stop();
    let t = b.at(0);
    b.c.disturbed(t);
    b.check(t + CONVERGE_AFTER).unwrap();
}

/// Checks at `t` and once more `wrong_for` later.
fn twice(s: &Sim, t: Instant, wrong_for: Millis) -> Result<(), String> {
    let mut c = Checks::default();
    c.after(&s.core, &s.fake, t)?;
    c.after(&s.core, &s.fake, t + wrong_for)
}

#[test]
fn connected_holds_while_the_link_is_encrypted() {
    let s = phone_up();
    assert_eq!(s.status().phone, wrist_ble_core::status::PhoneStatus::Connected);
    twice(&s, s.now, Millis::secs(5)).unwrap();
}

#[test]
fn a_wrong_status_may_last_a_second() {
    let mut s = phone_up();
    s.fake.stop();
    twice(&s, s.now, STATUS_WITHIN).unwrap();
}

#[test]
fn connected_with_no_link_on_air_may_not_last_more_than_a_second() {
    let mut s = phone_up();
    s.fake.stop();
    let e = twice(&s, s.now, STATUS_WITHIN + MS).unwrap_err();
    assert!(e.starts_with("truthful status: connected"), "{e}");
}

/// The phone's link, up and encrypted on air, as the core sees it before it is encrypted.
fn securing() -> Sim {
    let mut s = phone_up();
    let now = s.now;
    if let phone::Phone::Up(l) = &mut s.core.state.phone {
        l.security = Security::Unsecured;
        l.since = now;
    }
    assert_eq!(s.status().phone, wrist_ble_core::status::PhoneStatus::Securing);
    s
}

#[test]
fn securing_holds_while_the_link_is_up() {
    let s = securing();
    twice(&s, s.now, Millis::secs(2)).unwrap();
}

#[test]
fn securing_with_no_link_on_air_may_not_last_more_than_a_second() {
    let mut s = securing();
    s.fake.stop();
    let e = twice(&s, s.now, STATUS_WITHIN + MS).unwrap_err();
    assert!(e.starts_with("truthful status: a phone link"), "{e}");
}

/// The core sees no advertising for longer than a gap may last, so its status says unreachable.
fn unreachable(b: &mut Bench, t: Instant) {
    b.s.core.state.radio.advertising = None;
    b.s.core.state.silent_since = t - REACHABLE_GAP - Millis::secs(1);
    b.c.disturbed(t);
    assert!(matches!(status_at(&b.s, t), wrist_ble_core::status::PhoneStatus::Unreachable { .. }));
}

fn status_at(s: &Sim, t: Instant) -> wrist_ble_core::status::PhoneStatus {
    wrist_ble_core::status::status(&s.core.state, t).phone
}

#[test]
fn unreachable_holds_while_nothing_is_on_air() {
    let mut b = Bench::new();
    let t = b.at(10_000);
    b.s.fake.submit(Op::Advertise(None)).unwrap();
    unreachable(&mut b, t);
    b.check(t).unwrap();
    b.check(t + Millis::secs(2)).unwrap();
}

#[test]
fn unreachable_while_advertising_may_not_last_more_than_a_second() {
    let mut b = Bench::new();
    let t = b.at(10_000);
    unreachable(&mut b, t);
    b.check(t).unwrap();
    let e = b.check(t + STATUS_WITHIN + MS).unwrap_err();
    assert!(e.starts_with("truthful status: unreachable"), "{e}");
}

#[test]
fn unreachable_while_the_chip_starts_holds_whatever_is_on_air() {
    let mut b = Bench::new();
    let t = b.at(0);
    b.s.core.state.chip.state = Chip::Starting { since: t };
    b.c.disturbed(t);
    b.check(t).unwrap();
    b.check(t + Millis::secs(2)).unwrap();
}

#[test]
fn each_wrong_claim_has_its_own_second() {
    let mut b = Bench::new();
    let t = b.at(10_000);
    b.s.fake.submit(Op::Advertise(None)).unwrap();
    b.c.disturbed(t);
    b.check(t).unwrap();
    let then = t + Millis(STATUS_WITHIN.0 * 9 / 10);
    b.s.fake.submit(Op::Advertise(Some(b.air))).unwrap();
    unreachable(&mut b, then);
    b.check(then).unwrap();
    b.check(then + Millis(STATUS_WITHIN.0 * 9 / 10)).unwrap();
}
