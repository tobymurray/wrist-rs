//! The console's counts of what went wrong: what each input adds, and the line.

mod common;

use common::*;
use wrist_ble::policy::engine::Input;
use wrist_ble::policy::plan::{Conflict, Verdict};
use wrist_ble::policy::tally::{Counts, Tally, COMMAND_WINDOW, EARLY_END};
use wrist_ble::policy::State;
use wrist_ble::{
    ConnHandle, Failure, FailureKind, LeConnRole, Millis, OpId, PairingEvent, PairingStep, RadioEvent, ResetCause, SchedKind,
    Status,
};

fn quiet() -> State {
    let mut s = ready(t(0));
    s.radio.advertising = Some(ADV_995);
    s
}

fn feed(tally: &mut Tally, at: u64, e: RadioEvent) {
    tally.step(t(at), &Input::Radio(e), &quiet(), false);
}

fn up(link: ConnHandle, role: LeConnRole) -> RadioEvent {
    RadioEvent::LinkUp { link, role, peer: peer(), params: None }
}

fn down(link: ConnHandle, reason: u8) -> RadioEvent {
    RadioEvent::LinkDown { link, reason: Status::new(reason) }
}

fn line(tally: &Tally) -> String {
    let mut w = String::new();
    tally.write_line(&mut w).unwrap();
    w
}

#[test]
fn nothing_counted_says_so() {
    assert_eq!(
        line(&Tally::NEW),
        "[ble] counts: failures none; phone hang-ups none; pairings ok 0 failed none; hung up within 10 s of encrypting none; \
         chip resets none; operations a minute at most 0; unreachable now 0 s, worst 0 s; plan drift worst 0 s"
    );
}

#[test]
fn refusals_and_failures_are_counted_by_kind_and_code() {
    let mut tally = Tally::NEW;
    let busy = Failure { kind: FailureKind::Busy, native: 0x0C };
    let slot = Failure { kind: FailureKind::Schedule(SchedKind::NoSlot), native: 0x85 };
    tally.step(t(0), &Input::Submitted(Err(busy)), &quiet(), false);
    feed(&mut tally, 1, RadioEvent::Done(OpId(1), Err(busy)));
    feed(&mut tally, 2, RadioEvent::Params { link: phone(), result: Err(slot) });
    feed(&mut tally, 3, RadioEvent::Done(OpId(2), Ok(wrist_ble::Outcome::Done)));
    assert_eq!(tally.failures.total(), 3);
    assert!(line(&tally).starts_with("[ble] counts: failures busy/0C x2 schedule-no-slot/85 x1; "));
}

#[test]
fn counts_past_their_rows_go_to_other() {
    let mut c = Counts::<u8, 2>::NEW;
    for k in [1, 2, 1, 3, 4] {
        c.add(k);
    }
    assert_eq!(c.rows, [Some((1, 2)), Some((2, 1))]);
    assert_eq!((c.other, c.total()), (2, 5));
}

#[test]
fn the_line_counts_what_found_no_row() {
    let mut tally = Tally::NEW;
    for native in 0..9 {
        tally.step(t(0), &Input::Submitted(Err(Failure { kind: FailureKind::Busy, native })), &quiet(), false);
    }
    assert!(line(&tally).contains(" busy/07 x1 other x1; phone hang-ups none; "));
}

#[test]
fn only_the_phones_hang_ups_count() {
    let mut tally = Tally::NEW;
    feed(&mut tally, 0, up(sensor(), LeConnRole::Central));
    feed(&mut tally, 1, down(sensor(), 0x08));
    feed(&mut tally, 2, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 3, down(phone(), 0x13));
    feed(&mut tally, 4, down(phone(), 0x13));
    assert_eq!(tally.hang_ups.rows[0], Some((0x13, 1)));
    assert_eq!(tally.hang_ups.total(), 1);
}

#[test]
fn a_hang_up_soon_after_encrypting_is_early() {
    let mut tally = Tally::NEW;
    let edge = EARLY_END.0 as u64;
    for (start, after, reason) in [(0, edge, 0x08), (100_000, edge + 1, 0x16)] {
        feed(&mut tally, start, up(phone(), LeConnRole::Peripheral));
        feed(&mut tally, start + 100, RadioEvent::Encryption { link: phone(), on: true });
        feed(&mut tally, start + 100 + after, down(phone(), reason));
    }
    feed(&mut tally, 200_000, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 200_100, down(phone(), 0x3E));
    assert_eq!(tally.early_ends.rows[0], Some((0x08, 1)));
    assert_eq!(tally.early_ends.total(), 1);
    assert_eq!(tally.hang_ups.total(), 3);
}

#[test]
fn encryption_turned_off_is_no_longer_encrypted() {
    let mut tally = Tally::NEW;
    feed(&mut tally, 0, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 100, RadioEvent::Encryption { link: phone(), on: true });
    feed(&mut tally, 200, RadioEvent::Encryption { link: phone(), on: false });
    feed(&mut tally, 300, down(phone(), 0x08));
    assert_eq!(tally.early_ends.total(), 0);
}

#[test]
fn pairings_are_counted_with_the_last_failure_and_its_peer() {
    let mut tally = Tally::NEW;
    let pair = |step| RadioEvent::Pairing(PairingEvent { link: phone(), step });
    feed(&mut tally, 0, pair(PairingStep::Complete));
    feed(&mut tally, 0, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 1, pair(PairingStep::Numeric(1)));
    feed(&mut tally, 2, pair(PairingStep::Complete));
    feed(&mut tally, 3, pair(PairingStep::Failed));
    feed(&mut tally, 4, pair(PairingStep::LocalKeyMissing));
    feed(&mut tally, 5, pair(PairingStep::PeerKeyMissing));
    assert_eq!(tally.pairings_ok, 1);
    assert_eq!(tally.pairing_failures.total(), 3);
    assert!(line(&tally).contains(
        "; pairings ok 1 failed failed x1 local-key-missing x1 peer-key-missing x1 (last peer-key-missing from 06:05:04:03:02:01); "
    ));
}

#[test]
fn a_chip_reset_is_counted_and_ends_the_link() {
    let mut tally = Tally::NEW;
    feed(&mut tally, 0, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 1, RadioEvent::ChipReset(ResetCause::Watchdog));
    feed(&mut tally, 2, down(phone(), 0x08));
    feed(&mut tally, 3, up(phone(), LeConnRole::Peripheral));
    feed(&mut tally, 4, RadioEvent::Ready(INFO));
    feed(&mut tally, 5, down(phone(), 0x08));
    assert_eq!(tally.chip_resets.rows[0], Some((ResetCause::Watchdog, 1)));
    assert_eq!(tally.hang_ups.total(), 0);
    assert!(line(&tally).contains("; chip resets watchdog x1; "));
}

#[test]
fn the_peak_is_the_most_operations_in_one_window() {
    let mut tally = Tally::NEW;
    let window = COMMAND_WINDOW.0 as u64;
    for at in [0, 10, 20, window, window + 10] {
        tally.step(t(at), &Input::Tick, &quiet(), true);
    }
    tally.step(t(window + 20), &Input::Tick, &quiet(), false);
    assert_eq!(tally.commands_peak, 3);
    for at in [2 * window, 2 * window + 1, 2 * window + 2, 2 * window + 3] {
        tally.step(t(at), &Input::Tick, &quiet(), true);
    }
    assert_eq!(tally.commands_peak, 4);
}

#[test]
fn unreachable_time_is_kept_now_and_at_its_worst() {
    let mut tally = Tally::NEW;
    let mut gap = ready(t(0));
    gap.phone_verdict = Verdict::Busy(Conflict::Scan);
    tally.step(t(7_000), &Input::Tick, &gap, false);
    assert_eq!((tally.unreachable_now, tally.unreachable_worst), (Millis::secs(7), Millis::secs(7)));
    tally.step(t(8_000), &Input::Tick, &quiet(), false);
    assert_eq!((tally.unreachable_now, tally.unreachable_worst), (Millis(0), Millis::secs(7)));
    assert!(line(&tally).contains("unreachable now 0 s, worst 7 s"));
}

#[test]
fn drift_is_kept_at_its_worst() {
    let mut tally = Tally::NEW;
    let mut drifting = quiet();
    drifting.drift_since = Some(t(1_000));
    tally.step(t(5_000), &Input::Tick, &drifting, false);
    tally.step(t(6_000), &Input::Tick, &quiet(), false);
    assert_eq!(tally.drift_worst, Millis::secs(4));
    assert!(line(&tally).ends_with("plan drift worst 4 s"));
}
