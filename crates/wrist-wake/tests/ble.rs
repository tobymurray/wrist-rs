//! The BLE loop's deadlines: idle costs no wake, and everything it does by time has one.

use wrist_wake::ble::{
    advertising_ladder, due, fits, reconnect_after_ms, scan, sensor, timing, Console, Power, State, Transfer, ADVERTISE_FASTEST,
    GIVE_UP_MS,
};
use wrist_wake::earliest;

const NOW: u64 = 1_000;

fn idle() -> State {
    State { now: NOW, work: false, confirm_by: None, transfer: Transfer::None, console: None, abandon_at: None, central_at: None }
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
    assert!(idle.advertise >= 1_590, "about a second on battery");
    // An idle link wakes the radio at most about once a second.
    assert!(u32::from(idle.interval_min) * 125 / 100 * (1 + u32::from(idle.latency)) >= 500);
    assert_eq!(cable, timing(Power::Battery, true), "a transfer is as fast as a cable");
    assert_eq!(cable, timing(Power::Cable, true));
    assert!(cable.advertise <= 160 && cable.interval_max <= 24 && cable.latency == 0);
}

#[test]
fn central_work_by_time_wakes_the_loop() {
    let s = State { central_at: Some(NOW + 10_000), ..idle() };
    assert_eq!(due(&s), Some(NOW + 10_000));
    let s = State { central_at: Some(NOW + 10_000), abandon_at: Some(NOW + 5_000), ..idle() };
    assert_eq!(due(&s), Some(NOW + 5_000));
}

#[test]
fn scanning_listens_less_on_battery() {
    for power in [Power::Cable, Power::Battery] {
        let s = scan(power);
        // Core Specification limits: 2.5 ms to 10.24 s, window no longer than interval.
        assert!((4..=0x4000).contains(&s.interval) && (4..=s.interval).contains(&s.window));
    }
    let duty = |s: wrist_wake::ble::Scan| u32::from(s.window) * 100 / u32::from(s.interval);
    assert_eq!(duty(scan(Power::Cable)), 60);
    assert_eq!(duty(scan(Power::Battery)), 20);
}

#[test]
fn the_sensor_link_is_slow_on_battery_and_within_the_spec() {
    for power in [Power::Cable, Power::Battery] {
        let t = sensor(power);
        assert!((6..=t.interval_max).contains(&t.interval_min) && t.interval_max <= 3200);
        assert_eq!(t.latency, 0);
        // The timeout (10 ms) exceeds twice the longest interval (1.25 ms): (1 + 0) * 2 * max.
        assert!(u32::from(t.timeout) * 10 * 100 > 2 * u32::from(t.interval_max) * 125);
        assert!((10..=3200).contains(&t.timeout));
    }
    assert!(sensor(Power::Battery).interval_min >= 8 * sensor(Power::Cable).interval_max);
}

#[test]
fn a_lost_sensor_is_retried_less_often_on_battery_and_not_for_ever() {
    assert!(reconnect_after_ms(Power::Battery) > reconnect_after_ms(Power::Cable));
    assert!(GIVE_UP_MS >= 10 * reconnect_after_ms(Power::Battery));
}

#[test]
fn advertising_without_a_sensor_link_is_as_wanted_then_the_cable_interval() {
    let battery = timing(Power::Battery, false).advertise;
    assert_eq!(advertising_ladder(battery, None), [Some(battery), Some(ADVERTISE_FASTEST), None, None]);
    // On a cable the wanted interval is the fastest already: nothing else to try.
    assert_eq!(advertising_ladder(ADVERTISE_FASTEST, None), [Some(ADVERTISE_FASTEST), None, None, None]);
}

#[test]
fn beside_a_sensor_link_advertising_tries_intervals_that_fit() {
    // Install 151: a 420 ms link (336 units) and 1022.5 ms (1636 units) refused.
    let ladder = advertising_ladder(1636, Some(336));
    // 2 x 420 - 5 = 835 ms (1336 units), then 415 ms (664), then the cable's 100 ms.
    assert_eq!(ladder, [Some(1636), Some(1336), Some(664), Some(ADVERTISE_FASTEST)]);
    for v in ladder.into_iter().take(3).skip(1).flatten() {
        assert!(fits(v, 336));
    }
    // A link slower than the wanted interval: none fits, only what was wanted.
    assert_eq!(advertising_ladder(160, Some(400)), [Some(160), None, None, None]);
}

#[test]
fn each_power_sources_advertising_fits_beside_its_sensor_link() {
    for power in [Power::Cable, Power::Battery] {
        let adv = timing(power, false).advertise;
        let s = sensor(power);
        // Some interval the stack may pick within the range fits.
        assert!((s.interval_min..=s.interval_max).any(|iv| fits(adv, iv)), "{power:?}");
        // So the ladder's first rung is the interval wanted.
        assert_eq!(advertising_ladder(adv, Some(s.interval_min.max(28).min(s.interval_max)))[0], Some(adv));
    }
    // Install 152's figures: 100 ms beside 35 ms fits; 1022.5 ms beside 400 to 500 ms never does.
    assert!(fits(160, 28));
    assert!(!(320..=400).any(|iv| fits(1636, iv)));
    assert!(fits(1592, 400));
}
