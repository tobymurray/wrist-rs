//! The BLE loop's deadlines: idle costs no wake, and everything it does by time has one.

use wrist_wake::ble::{due, timing, Console, Power, State, Transfer};
use wrist_wake::earliest;

const NOW: u64 = 1_000;

fn idle() -> State {
    State { now: NOW, work: false, confirm_by: None, transfer: Transfer::None, console: None, abandon_at: None }
}

#[test]
fn earliest_ignores_the_missing() {
    assert_eq!(earliest([None, None]), None);
    assert_eq!(earliest([None, Some(5), Some(3), None]), Some(3));
    assert_eq!(earliest::<[Option<u64>; 0]>([]), None);
}

#[test]
fn idle_and_advertising_waits_for_events_only() {
    assert_eq!(due(&idle()), None);
    // No console subscriber, or nothing to send: still nothing.
    let s = State { console: Some(Console { pending: false, next_at: 0 }), ..idle() };
    assert_eq!(due(&s), None);
}

#[test]
fn work_left_by_an_event_runs_on_the_next_pass() {
    let s = State { work: true, confirm_by: Some(61_000), ..idle() };
    assert_eq!(due(&s), Some(NOW));
}

#[test]
fn a_comparison_is_rejected_at_its_timeout() {
    let s = State { confirm_by: Some(61_000), ..idle() };
    assert_eq!(due(&s), Some(61_000));
}

#[test]
fn console_output_is_paced_and_quiet_during_a_transfer() {
    let c = Console { pending: true, next_at: 1_010 };
    assert_eq!(due(&State { console: Some(c), ..idle() }), Some(1_010));
    let late = Console { pending: true, next_at: 500 };
    assert_eq!(due(&State { console: Some(late), ..idle() }), Some(NOW), "never in the past");
    let s = State { console: Some(c), transfer: Transfer::Started, ..idle() };
    assert_eq!(due(&s), None, "the mirror waits while image bytes arrive");
    let s = State { console: Some(c), transfer: Transfer::Claimed, ..idle() };
    assert_eq!(due(&s), Some(1_010));
}

#[test]
fn the_earliest_wins() {
    let s = State { confirm_by: Some(61_000), console: Some(Console { pending: true, next_at: 1_010 }), ..idle() };
    assert_eq!(due(&s), Some(1_010));
}

#[test]
fn a_quiet_transfer_is_abandoned_on_time() {
    let s = State { transfer: Transfer::Started, abandon_at: Some(6_000), ..idle() };
    assert_eq!(due(&s), Some(6_000));
}

#[test]
fn every_timing_is_inside_the_specification() {
    for power in [Power::Cable, Power::Battery] {
        for transfer in [false, true] {
            let t = timing(power, transfer);
            assert!((32..=16_384).contains(&t.advertise), "{t:?}");
            assert!((6..=t.interval_max).contains(&t.interval_min) && t.interval_max <= 3_200, "{t:?}");
            assert!(t.latency <= 499 && (10..=3_200).contains(&t.timeout), "{t:?}");
            // Core 6.B 4.5.2: the timeout must exceed (1 + latency) x interval x 2.
            let least_ms = (1 + u32::from(t.latency)) * u32::from(t.interval_max) * 125 / 100 * 2;
            assert!(u32::from(t.timeout) * 10 > least_ms, "{t:?}");
        }
    }
}

#[test]
fn battery_is_slow_until_a_transfer_and_a_cable_is_fast() {
    let idle = timing(Power::Battery, false);
    let cable = timing(Power::Cable, false);
    assert!(idle.advertise >= 1_600, "about a second on battery");
    // An idle link wakes the radio at most about once a second.
    assert!(u32::from(idle.interval_min) * 125 / 100 * (1 + u32::from(idle.latency)) >= 500);
    assert_eq!(cable, timing(Power::Battery, true), "a transfer is as fast as a cable");
    assert_eq!(cable, timing(Power::Cable, true));
    assert!(cable.advertise <= 160 && cable.interval_max <= 24 && cable.latency == 0);
}

#[test]
fn interval_now_keeps_to_its_supervision_timeout() {
    use wrist_wake::ble::{interval_now, supervised, NOW_LATENCY, NOW_TIMEOUT};
    assert!(interval_now(6, 6) && interval_now(12, 12) && interval_now(6, 1_999));
    // 2000 x 1.25 ms x 2 is the 5 s timeout itself.
    assert!(!interval_now(6, 2_000) && !interval_now(2_400, 3_200));
    assert!(!interval_now(5, 12) && !interval_now(24, 12));
    for max in 6..=3_200 {
        assert_eq!(interval_now(6, max), supervised(max, NOW_LATENCY, NOW_TIMEOUT), "{max}");
    }
}

#[test]
fn a_start_that_never_advertised_failed() {
    use wrist_wake::ble::Radio;
    assert!(Radio::Starting.start_failed() && Radio::Failed.start_failed());
    for up in [Radio::Idle, Radio::Advertising, Radio::Connected] {
        assert!(!up.start_failed(), "{up:?}");
    }
    for r in [Radio::Off, Radio::Starting, Radio::Idle, Radio::Advertising, Radio::Connected, Radio::Failed] {
        assert_eq!(Radio::from_u8(r as u8), r);
    }
}

#[test]
fn a_chunk_refused_for_want_of_buffers_goes_again_first() {
    use wrist_wake::ble::Outbox;
    let mut out = Outbox::<8>::default();
    let (chunk, n) = out.next(4, |b| {
        b.copy_from_slice(b"pong");
        4
    });
    assert_eq!(out.sent(chunk, n, 0x64), Ok(()));
    assert!(out.holding());
    let (again, m) = out.next(4, |_| panic!("the held chunk goes first"));
    assert_eq!(&again[..m], b"pong");
    assert_eq!(out.sent(again, m, 0), Ok(()));
    assert!(!out.holding());
    assert_eq!(out.next(4, |_| 0).1, 0);
}

#[test]
fn any_other_refusal_loses_the_chunk_and_a_detach_drops_the_held_one() {
    use wrist_wake::ble::Outbox;
    let mut out = Outbox::<8>::default();
    let (chunk, n) = out.next(8, |b| {
        b[..2].copy_from_slice(b"ok");
        2
    });
    assert_eq!(out.sent(chunk, n, 0x41), Err(0x41));
    assert!(!out.holding());
    out.sent(chunk, n, 0x64).unwrap();
    out.clear();
    assert_eq!(out.next(8, |_| 0).1, 0);
}
