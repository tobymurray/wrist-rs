//! The console's counts of what went wrong: kept as the core judges it, and the line.

mod common;

use common::*;
use wrist_ble::policy::engine::{Core, Input};
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::phone;
use wrist_ble::policy::plan::{Conflict, Verdict};
use wrist_ble::policy::tally::{Counts, Restart, Tally, COMMAND_WINDOW, EARLY_END, PEAK_KEPT};
use wrist_ble::policy::State;
use wrist_ble::{
    AddrKind, BdAddr, Duration, Failure, FailureKind, LeConnRole, LinkParams, Op, OpId, Outcome, PairingEvent, PairingStep, PeerAddr,
    RadioEvent, ResetCause, Status,
};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn quiet() -> State {
    let mut s = ready(t(0));
    s.radio.advertising = Some(ADV_995);
    s
}

fn with_phone(secure: bool) -> State {
    let mut s = quiet();
    phone_up(&mut s, t(0));
    if secure {
        s.phone = s.phone.step(t(0), phone::Event::Encryption(phone(), true)).0;
    }
    s
}

fn line(tally: &Tally, now: u64) -> String {
    let mut w = String::new();
    tally.write_line(&mut w, t(now)).unwrap();
    w
}

fn down(reason: u8) -> Input {
    Input::Radio(RadioEvent::LinkDown { link: phone(), reason: Status::new(reason) })
}

#[test]
fn nothing_counted_says_so() {
    assert_eq!(
        line(&Tally::NEW, 0),
        "[ble] counts: failures none; phone hang-ups none; pairings and bonded reconnects ok 0, pairings failed none; hung up within 10 s of encrypting none; \
         chip restarts none; operations in a minute at most 0; unreachable now 0 s, worst 0 s; plan drift now 0 s, worst 0 s"
    );
}

#[test]
fn failures_are_counted_by_kind_and_code() {
    let mut tally = Tally::NEW;
    tally.failure(Failure { kind: FailureKind::Busy, native: 0x0C });
    tally.failure(Failure { kind: FailureKind::Busy, native: 0x0C });
    tally.failure(Failure { kind: FailureKind::Schedule(wrist_ble::SchedKind::NoSlot), native: 0x85 });
    assert!(line(&tally, 0).starts_with("[ble] counts: failures busy/0C x2 schedule-no-slot/85 x1; "));
}

#[test]
fn counts_past_their_rows_go_to_other() {
    let mut c = Counts::<u8, 2>::NEW;
    for k in [1, 2, 1, 3, 4] {
        c.add(k);
    }
    assert_eq!(c.rows, [Some((1, 2)), Some((2, 1))]);
    assert_eq!((c.other, c.total()), (2, 5));
    let mut tally = Tally::NEW;
    for native in 0..9 {
        tally.failure(Failure { kind: FailureKind::Busy, native });
    }
    assert!(line(&tally, 0).contains(" busy/07 x1 other x1; phone hang-ups none; "));
}

/// The core's phone link, however it came: a link a resync adopted is counted like any other.
#[test]
fn the_phones_hang_ups_follow_the_cores_link() {
    let mut tally = Tally::NEW;
    tally.step(t(0), &Input::Tick, &with_phone(false), false);
    tally.step(t(100), &down(0x13), &quiet(), false);
    tally.step(t(200), &down(0x13), &quiet(), false);
    tally.step(t(300), &Input::Radio(RadioEvent::LinkDown { link: sensor(), reason: Status::new(0x08) }), &quiet(), false);
    assert_eq!(tally.hang_ups.rows[0], Some((0x13, 1)));
    assert_eq!(tally.hang_ups.total(), 1);
}

#[test]
fn a_hang_up_soon_after_encrypting_is_early() {
    let mut tally = Tally::NEW;
    let edge = u64::from(EARLY_END.0);
    for (start, after, reason) in [(0, edge, 0x08), (100_000, edge + 1, 0x16)] {
        tally.step(t(start), &Input::Tick, &with_phone(false), false);
        tally.step(t(start + 100), &Input::Tick, &with_phone(true), false);
        tally.step(t(start + 100 + after), &down(reason), &quiet(), false);
    }
    tally.step(t(200_000), &Input::Tick, &with_phone(false), false);
    tally.step(t(200_100), &down(0x3E), &quiet(), false);
    assert_eq!(tally.early_ends.rows, [Some((0x08, 1)), None, None, None]);
    assert_eq!(tally.hang_ups.total(), 3);
}

#[test]
fn a_new_link_on_the_same_handle_is_not_the_old_ones_encryption() {
    let mut tally = Tally::NEW;
    tally.step(t(0), &Input::Tick, &with_phone(true), false);
    tally.step(t(100), &Input::Tick, &quiet(), false);
    tally.step(t(200), &Input::Tick, &with_phone(false), false);
    tally.step(t(300), &down(0x08), &quiet(), false);
    assert_eq!(tally.early_ends.total(), 0);
}

#[test]
fn pairings_on_the_phones_link_are_counted_with_the_last_failure() {
    let mut tally = Tally::NEW;
    let pair = |step| Input::Radio(RadioEvent::Pairing(PairingEvent { link: phone(), step }));
    tally.step(t(0), &pair(PairingStep::Complete), &quiet(), false);
    let up = Input::Radio(RadioEvent::LinkUp { link: phone(), role: LeConnRole::Peripheral, peer: peer(), params: None });
    tally.step(t(0), &up, &with_phone(false), false);
    for (i, step) in [PairingStep::Numeric(1), PairingStep::Complete, PairingStep::Failed, PairingStep::LocalKeyMissing, PairingStep::PeerKeyMissing]
        .into_iter()
        .enumerate()
    {
        tally.step(t(i as u64 + 1), &pair(step), &with_phone(false), false);
    }
    assert_eq!(tally.pairings_ok, 1);
    assert!(line(&tally, 10).contains(
        "; pairings and bonded reconnects ok 1, pairings failed failed x1 local-key-missing x1 peer-key-missing x1 (last peer-key-missing from 06:05:04:03:02:01); "
    ));
}

#[test]
fn a_failure_on_a_link_whose_start_went_unseen_has_no_peer() {
    let mut tally = Tally::NEW;
    let failed = Input::Radio(RadioEvent::Pairing(PairingEvent { link: phone(), step: PairingStep::Failed }));
    tally.step(t(0), &Input::Tick, &with_phone(false), false);
    tally.step(t(1), &failed, &with_phone(false), false);
    assert!(line(&tally, 2).contains("failed failed x1 (last failed); "));
}

#[test]
fn restarts_are_named() {
    let mut tally = Tally::NEW;
    tally.restart(Restart::Reset(ResetCause::Watchdog));
    tally.restart(Restart::StartFailed);
    assert!(line(&tally, 0).contains("; chip restarts reset-watchdog x1 start-failed x1; "));
}

#[test]
fn the_peak_is_the_most_operations_in_any_window() {
    let mut tally = Tally::NEW;
    let window = u64::from(COMMAND_WINDOW.0);
    for i in 0..20 {
        tally.step(t(30_000 + i * 1_000), &Input::Tick, &quiet(), true);
    }
    for i in 0..20 {
        tally.step(t(window + i * 1_000), &Input::Tick, &quiet(), true);
    }
    assert_eq!(tally.commands_peak, 40);
    for i in 0..PEAK_KEPT as u64 + 5 {
        tally.step(t(10 * window + i), &Input::Tick, &quiet(), true);
    }
    assert_eq!(tally.commands_peak as usize, PEAK_KEPT);
    assert!(line(&tally, 0).contains(&format!("at most at least {PEAK_KEPT};")));
}

#[test]
fn an_unreachable_spell_is_measured_to_now_and_kept_at_its_worst() {
    let mut tally = Tally::NEW;
    let mut gap = ready(t(0));
    gap.phone_verdict = Verdict::Busy(Conflict::Scan);
    tally.step(t(4_000), &Input::Tick, &gap, false);
    assert!(line(&tally, 60_000).contains("unreachable now 60 s, worst 60 s"));
    tally.step(t(9_000), &Input::Tick, &quiet(), false);
    assert!(line(&tally, 60_000).contains("unreachable now 0 s, worst 9 s"));
    gap.phone_verdict = Verdict::NotWanted;
    tally.step(t(100_000), &Input::Tick, &gap, false);
    assert!(line(&tally, 200_000).contains("unreachable now 0 s, worst 9 s"));
}

#[test]
fn drift_is_measured_to_now_and_kept_at_its_worst() {
    let mut tally = Tally::NEW;
    let mut drifting = quiet();
    drifting.drift_since = Some(t(1_000));
    tally.step(t(2_000), &Input::Tick, &drifting, false);
    assert!(line(&tally, 30_000).ends_with("plan drift now 29 s, worst 29 s"));
    tally.step(t(6_000), &Input::Tick, &quiet(), false);
    assert!(line(&tally, 30_000).ends_with("plan drift now 0 s, worst 5 s"));
}

fn core_with_phone() -> Core<'static> {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let g = Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Battery, ..Goals::OFF };
    core.step(t(0), Input::Goals(g));
    let start = core.state.chip.starting().expect("a start");
    assert!(matches!(core.step(t(0), Input::Started(start, Ok(INFO))).op, Some(Op::Advertise(_))));
    core.step(t(0), Input::Submitted(Ok(OpId(1))));
    core.step(t(0), Input::Radio(RadioEvent::Done(OpId(1), Ok(Outcome::Done))));
    let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) };
    let params = LinkParams { interval: Duration::from_u16(24), latency: 0, timeout: Duration::from_u16(500) };
    core.step(t(100), Input::Radio(RadioEvent::LinkUp { link: phone(), role: LeConnRole::Peripheral, peer, params: Some(params) }));
    core
}

/// The driver's end of an operation the core already let go with its link is not a failure.
#[test]
fn an_operation_ended_by_its_links_end_is_no_failure() {
    let mut core = core_with_phone();
    assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
    core.step(t(10_000), Input::Submitted(Ok(OpId(2))));
    core.step(t(10_100), down(0x13));
    core.step(t(10_200), Input::Radio(RadioEvent::Done(OpId(2), Err(Failure { kind: FailureKind::NotConnected, native: 0 }))));
    assert_eq!((core.tally.failures.total(), core.tally.hang_ups.total()), (0, 1));
}

/// An ask the chip refuses busy is held for the central's answer: ordinary work, not a failure.
#[test]
fn an_ask_held_for_the_centrals_answer_is_no_failure() {
    let mut core = core_with_phone();
    let asked = core.step(t(1_000), Input::Radio(RadioEvent::Encryption { link: phone(), on: true })).op;
    assert!(matches!(asked, Some(Op::UpdateParams { .. })), "{asked:?}");
    let ms = 1_000;
    core.step(t(ms), Input::Submitted(Err(Failure { kind: FailureKind::Busy, native: 0x46 })));
    assert_eq!((core.counters.refused, core.tally.failures.total()), (1, 0));
}

/// A refusal the core counts as a failure is in the tally too.
#[test]
fn a_refusal_the_core_backs_off_from_is_a_failure() {
    let mut core = core_with_phone();
    assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
    core.step(t(10_000), Input::Submitted(Err(Failure { kind: FailureKind::Busy, native: 0x0C })));
    assert_eq!(core.tally.failures.rows[0], Some(((FailureKind::Busy, 0x0C), 1)));
}

/// A chip that comes up again by itself is a restart the core counts, and the tally with it.
#[test]
fn a_restart_is_counted_where_the_core_counts_it() {
    let mut core = core_with_phone();
    core.step(t(20_000), Input::Radio(RadioEvent::Ready(INFO)));
    assert_eq!((core.counters.resets, core.tally.restarts.total()), (1, 1));
}

#[test]
fn restarts_are_named_by_what_the_core_was_told() {
    use wrist_ble::policy::chip::Event;
    assert_eq!(Restart::of(Event::Reset(ResetCause::Crash)), Restart::Reset(ResetCause::Crash));
    assert_eq!(Restart::of(Event::StartFailed), Restart::StartFailed);
    assert_eq!(Restart::of(Event::BaselineRefused), Restart::BaselineRefused);
    assert_eq!(Restart::of(Event::Tick), Restart::Deadline);
}

/// The peak can show I2 broken: more operations than I2 allows are told apart.
#[test]
fn the_peak_tells_a_breach_of_i2_apart() {
    use wrist_ble::policy::invariants::COMMANDS_PER_MINUTE;
    assert!(PEAK_KEPT > COMMANDS_PER_MINUTE as usize);
    let mut tally = Tally::NEW;
    for i in 0..=u64::from(COMMANDS_PER_MINUTE) {
        tally.step(t(i * 100), &Input::Tick, &quiet(), true);
    }
    assert_eq!(tally.commands_peak, COMMANDS_PER_MINUTE + 1);
}

#[test]
fn operations_a_whole_window_apart_are_not_in_one_window() {
    let mut tally = Tally::NEW;
    tally.step(t(0), &Input::Tick, &quiet(), true);
    tally.step(t(u64::from(COMMAND_WINDOW.0)), &Input::Tick, &quiet(), true);
    assert_eq!(tally.commands_peak, 1);
}

#[test]
fn another_links_start_is_not_the_phones_peer() {
    let mut tally = Tally::NEW;
    let up = |link, last| {
        let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([last, 0, 0, 0, 0, 0]) };
        Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer, params: None })
    };
    tally.step(t(0), &up(phone(), 0xAA), &with_phone(false), false);
    tally.step(t(1), &up(sensor(), 0xBB), &with_phone(false), false);
    tally.step(t(2), &Input::Radio(RadioEvent::Pairing(PairingEvent { link: phone(), step: PairingStep::Failed })), &with_phone(false), false);
    assert!(line(&tally, 3).contains("(last failed from 00:00:00:00:00:AA)"));
}

/// The core's own timeout of an operation is a failure.
#[test]
fn an_operation_the_core_times_out_is_a_failure() {
    let mut core = core_with_phone();
    assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
    core.step(t(10_000), Input::Submitted(Ok(OpId(2))));
    core.step(t(400_000), Input::Tick);
    assert_eq!(core.counters.timed_out, 1);
    assert_eq!(core.tally.failures.rows[0], Some(((FailureKind::Timeout, 0), 1)));
}

/// A refusal for a link that ended before the answer came is the link's end, not a failure.
#[test]
fn a_refusal_for_a_link_already_gone_is_no_failure() {
    let mut core = core_with_phone();
    assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
    core.step(t(10_050), down(0x13));
    core.step(t(10_100), Input::Submitted(Err(Failure { kind: FailureKind::NotConnected, native: 0x02 })));
    assert_eq!((core.tally.failures.total(), core.tally.hang_ups.total()), (0, 1));
}

/// Time hidden is chosen: an unreachable spell after it starts where hiding ended.
#[test]
fn a_spell_after_hiding_starts_where_hiding_ended() {
    let mut tally = Tally::NEW;
    let mut gap = ready(t(0));
    gap.phone_verdict = Verdict::NotWanted;
    tally.step(t(3_600_000), &Input::Tick, &gap, false);
    gap.phone_verdict = Verdict::Fits;
    tally.step(t(3_601_000), &Input::Tick, &gap, false);
    assert!(line(&tally, 3_602_000).contains("unreachable now 1 s, worst 1 s"));
}

/// Asks a whole wrap of a 32-bit millisecond count apart are not in one window.
#[test]
fn asks_days_apart_are_not_in_one_window() {
    let mut tally = Tally::NEW;
    for i in 0..30 {
        tally.step(t(i), &Input::Tick, &quiet(), true);
    }
    for i in 0..30 {
        tally.step(t((1 << 32) + i), &Input::Tick, &quiet(), true);
    }
    assert_eq!(tally.commands_peak, 30);
}

/// A link a resync puts in the old one's place has neither its peer nor its encryption time.
#[test]
fn a_link_swapped_by_a_resync_starts_afresh() {
    let mut tally = Tally::NEW;
    let up = Input::Radio(RadioEvent::LinkUp { link: phone(), role: LeConnRole::Peripheral, peer: peer(), params: None });
    tally.step(t(0), &up, &with_phone(false), false);
    tally.step(t(100), &Input::Tick, &with_phone(true), false);
    let mut swapped = quiet();
    swapped.phone = swapped.phone.step(t(200), phone::Event::LinkUp(sensor())).0;
    tally.step(t(200), &Input::Tick, &swapped, false);
    let failed = Input::Radio(RadioEvent::Pairing(PairingEvent { link: sensor(), step: PairingStep::Failed }));
    tally.step(t(300), &failed, &swapped, false);
    tally.step(t(400), &Input::Radio(RadioEvent::LinkDown { link: sensor(), reason: Status::new(0x08) }), &quiet(), false);
    assert!(line(&tally, 500).contains("(last failed); "));
    assert_eq!((tally.hang_ups.total(), tally.early_ends.total()), (1, 0));
}

/// An operation that ends in failure, and a refusal the model did not predict, are failures.
#[test]
fn a_failed_operation_and_a_divergence_are_failures() {
    let mut core = core_with_phone();
    assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
    core.step(t(10_000), Input::Submitted(Ok(OpId(2))));
    core.step(t(10_100), Input::Radio(RadioEvent::Done(OpId(2), Err(Failure { kind: FailureKind::PeerRejected, native: 0x05 }))));
    assert_eq!(core.tally.failures.rows[0], Some(((FailureKind::PeerRejected, 0x05), 1)));
    let mut core = core_with_phone();
    let advertise = core.step(t(100), Input::Radio(RadioEvent::LinkDown { link: phone(), reason: Status::new(0x13) })).op;
    assert!(matches!(advertise, Some(Op::Advertise(Some(_)))), "{advertise:?}");
    core.step(t(100), Input::Submitted(Err(Failure { kind: FailureKind::ModeExclusion, native: 0x0C })));
    assert_eq!((core.counters.divergences, core.tally.failures.rows[0]), (1, Some(((FailureKind::ModeExclusion, 0x0C), 1))));
}

/// An operation the chip took after its link had gone ends in failure, but that is the link's end.
#[test]
fn an_operation_taken_after_its_link_went_is_no_failure() {
    for (end, timed_out) in [(Some(Failure { kind: FailureKind::NotConnected, native: 0 }), 0), (None, 1)] {
        let mut core = core_with_phone();
        assert_eq!(core.step(t(10_000), Input::Tick).op, Some(Op::RequestSecurity(phone())));
        core.step(t(10_050), down(0x13));
        core.step(t(10_100), Input::Submitted(Ok(OpId(2))));
        match end {
            Some(f) => core.step(t(10_200), Input::Radio(RadioEvent::Done(OpId(2), Err(f)))),
            None => core.step(t(400_000), Input::Tick),
        };
        assert_eq!((core.counters.timed_out, core.tally.failures.total()), (timed_out, 0));
    }
}
