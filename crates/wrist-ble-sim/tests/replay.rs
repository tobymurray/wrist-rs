//! The watch's own driver, captured with `ble-trace`, replayed through the core.

use std::fmt::Write as _;
use std::path::Path;

use wrist_ble::{AdvParams, ConnHandle, ConnParams, Interval, Op, Slots, Timeout};
use wrist_ble::OpKind;
use wrist_ble_sim::replay::{replay, DriverOp, Report, Who};

fn traces() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces");
    let mut all: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| (p.display().to_string(), std::fs::read_to_string(&p).unwrap()))
        .collect();
    all.sort();
    assert_ne!(all.len(), 0);
    all
}

#[test]
fn the_core_acts_where_and_as_the_driver_did() {
    for (name, capture) in traces() {
        let report = replay(&capture);
        assert!(!report.core_ops.is_empty() && !report.driver_ops.is_empty(), "{name}: nothing replayed");
        assert_eq!(report.disagreements(), Vec::<String>::new(), "{name}\n{}", report.render(false));
    }
}

#[test]
fn a_capture_timed_by_the_wall_clock_replays_as_one_timed_in_seconds() {
    let (name, capture) = &traces()[0];
    let clock = capture
        .lines()
        .map(|l| {
            let (t, rest) = l.split_once(' ').unwrap();
            let (s, ms) = t.split_once('.').unwrap();
            let s: u64 = s.parse::<u64>().unwrap() + 9 * 3_600 + 27 * 60;
            format!("{:02}:{:02}:{:02}.{ms} {rest}", s / 3_600, s / 60 % 60, s % 60)
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(replay(&clock).core_ops, replay(capture).core_ops, "{name}");
}

#[test]
fn a_core_that_acts_alone_is_a_disagreement() {
    let (name, capture) = &traces()[0];
    let without_driver = capture.lines().filter(|l| !l.contains(" c ")).collect::<Vec<_>>().join("\n");
    assert!(!replay(&without_driver).disagreements().is_empty(), "{name}");
}

/// Kept lines only: what `tools/ble-trace-fixture.py` writes, so no fixture holds an address.
#[test]
fn traces_hold_only_what_the_scrub_keeps() {
    for (name, capture) in traces() {
        for line in capture.lines() {
            let (_, text) = line.split_once(' ').unwrap();
            if text.starts_with("== ") || text.starts_with(">> usb ble ") {
                let joined: String = text.chars().filter(|c| !matches!(c, ':' | '-' | '_')).collect();
                assert!(!joined.split(|c: char| !c.is_ascii_hexdigit()).any(|w| w.len() >= 12), "{name}: an address in {line}");
                continue;
            }
            let f: Vec<&str> = text.split(' ').collect();
            assert_eq!(f[0], "[trace]", "{name}: {line}");
            match f[2] {
                "r" => assert_eq!(f.len(), 3, "{name}: {line}"),
                "c" | "e" => {
                    let p: Vec<u8> = (0..f[3].len() / 2).map(|i| u8::from_str_radix(&f[3][2 * i..2 * i + 2], 16).unwrap()).collect();
                    assert!(p.len() >= 3, "{name}: {line}");
                    if f[2] == "e" {
                        assert!(p[0] == 0x04 && !matches!(p[1], 0x0E | 0x0F), "{name}: a reply in {line}");
                        if p[1] == 0x3E {
                            assert_ne!(p[3], 0x02, "{name}: a report in {line}");
                            let addresses = match p[3] {
                                0x01 => 9..15,
                                0x0A => 9..27,
                                _ => 0..0,
                            };
                            assert!(p[addresses].iter().all(|&b| b == 0), "{name}: an address in {line}");
                        }
                        if p[1] == 0xFF {
                            assert_ne!(u16::from_le_bytes([p[3], p[4]]), 0x0C01, "{name}: console data in {line}");
                        }
                    }
                }
                _ => panic!("{name}: {line}"),
            }
        }
    }
}

fn ask(min: u16, max: u16, latency: u16) -> Op {
    let params = ConnParams {
        interval_min: Interval::from_u16(min),
        interval_max: Interval::from_u16(max),
        latency,
        timeout: Timeout::from_u16(600),
        ce_len: Slots::from_u16(0),
    };
    Op::UpdateParams(ConnHandle::new(1), params)
}

fn advertise() -> Op {
    Op::Advertise(Some(AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(160), connectable: true }))
}

fn report(core: &[(u64, Op)], driver: &[(u64, DriverOp)]) -> Report {
    Report { core_ops: core.to_vec(), driver_ops: driver.to_vec(), ..Report::default() }
}

#[test]
fn the_same_ask_within_the_window_agrees_and_any_other_does_not() {
    let core = [(1_000, ask(80, 160, 4))];
    assert_eq!(report(&core, &[(1_300, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 0);
    assert_eq!(report(&core, &[(700, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 0);
    for driver in [
        DriverOp::AskParams(80, 160, 0),
        DriverOp::AskParams(12, 160, 4),
        DriverOp::AskParams(80, 24, 4),
    ] {
        assert_eq!(report(&core, &[(1_000, driver)]).disagreements().len(), 1, "{driver:?}");
    }
    assert_eq!(report(&core, &[(1_301, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 1);
    assert_eq!(report(&core, &[(699, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 1);
    assert_eq!(report(&[], &[(1_000, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 0, "the driver asks at every connection");
}

#[test]
fn advertising_on_one_side_alone_is_a_disagreement() {
    assert_eq!(report(&[(1_000, advertise())], &[(1_000, DriverOp::Advertise(160, 160))]).disagreements().len(), 0);
    assert_eq!(report(&[(1_000, advertise())], &[(1_301, DriverOp::Advertise(160, 160))]).disagreements().len(), 2);
    assert_eq!(report(&[(1_000, advertise())], &[(1_000, DriverOp::StopAdvertising)]).disagreements().len(), 1);
    assert_eq!(report(&[(1_000, advertise())], &[(1_000, DriverOp::AskParams(80, 160, 4))]).disagreements().len(), 1);
    assert_eq!(report(&[(1_000, Op::Advertise(None))], &[(1_000, DriverOp::Advertise(160, 160))]).disagreements().len(), 1);
    assert_eq!(report(&[], &[(1_000, DriverOp::Advertise(160, 160))]).disagreements().len(), 1);
}

#[test]
fn an_event_the_core_could_not_read_is_a_disagreement() {
    let r = Report { unknown_events: 1, ..Report::default() };
    assert_eq!(r.disagreements().len(), 1);
}

/// `first`, then `then` as if the watch had reset `gap_ms` after it: `then`'s host times carry
/// on from `first`'s, and its chip's milliseconds are its own.
fn after_a_reset(first: &str, between: &[&str], then: &str, gap_ms: u64) -> String {
    let stamp = |ms: u64| format!("{}.{:03}", ms / 1_000, ms % 1_000);
    let last = first.lines().filter_map(host_ms).max().unwrap();
    let mut out = first.to_string();
    for line in between {
        let _ = writeln!(out, "{} {line}", stamp(last + 1));
    }
    for line in then.lines() {
        let (_, rest) = line.trim_start().split_once(' ').unwrap();
        let _ = writeln!(out, "{} {rest}", stamp(last + gap_ms + host_ms(line).unwrap()));
    }
    out
}

/// A fixture line's host time in milliseconds.
fn host_ms(line: &str) -> Option<u64> {
    let (s, ms) = line.trim_start().split_once(' ')?.0.split_once('.')?;
    Some(s.parse::<u64>().ok()? * 1_000 + ms.parse::<u64>().ok()?)
}

/// (chip ms, host ms) of each trace line.
fn clock(capture: &str) -> Vec<(u64, u64)> {
    capture.lines().filter_map(|l| Some((l.split_whitespace().nth(2)?.parse().ok()?, host_ms(l)?))).collect()
}

/// A capture that ends forced to battery timing, which a reset puts back to auto, on the cable.
fn on_battery() -> String {
    let capture = fixture("bluez-battery.log");
    let end = capture.find("86.648 >> usb ble profile cable").unwrap();
    capture[..end].to_string()
}

/// A watch reset is a new epoch: the core starts again, with the goals the watch boots with, and
/// the timeline carries on where the host's clock puts it.
#[test]
fn a_watch_reset_starts_the_core_again() {
    let (first, then) = (on_battery(), fixture("mac-cable.log"));
    let capture = after_a_reset(&first, &[], &then, 60_000);
    let r = replay(&capture);
    assert_eq!(r.disagreements(), Vec::<String>::new(), "{}", r.render(false));
    assert_eq!(r.resets, 1);
    assert!(r.timeline.windows(2).all(|w| w[0].at <= w[1].at), "the timeline ran back");
    let (a, b) = (replay(&first), replay(&then));
    assert_eq!(r.core_ops.len(), a.core_ops.len() + b.core_ops.len());
    assert_eq!(r.driver_ops.len(), a.driver_ops.len() + b.driver_ops.len());
    // `then` falls 60 s after `first`'s last line, as the host saw it.
    let shift = r.driver_ops[a.driver_ops.len()].0 - b.driver_ops[0].0;
    for (whole, alone) in r.driver_ops[a.driver_ops.len()..].iter().zip(&b.driver_ops) {
        assert_eq!((whole.0 - shift, whole.1), *alone);
    }
    let (end, start) = (clock(&first)[clock(&first).len() - 1], clock(&then)[0]);
    let expected = end.0 + 60_000 + start.1 - start.0;
    assert!(shift.abs_diff(expected) <= 2, "shift {shift}, expected {expected}");
}

/// A heartbeat's uptime shows a reset that the chip's milliseconds alone would not: here the
/// capture after it starts later than the one before it ended.
#[test]
fn a_heartbeat_that_went_back_is_a_reset() {
    let (first, then) = (on_battery(), fixture("android14-pairing.log"));
    assert!(clock(&then)[0].0 > clock(&first).iter().map(|c| c.0).max().unwrap());
    let without = replay(&after_a_reset(&first, &[], &then, 60_000));
    assert_eq!(without.resets, 0);
    assert_ne!(without.disagreements(), Vec::<String>::new(), "battery timing kept, unnoticed");
    for heartbeat in ["[hb] 17  uptime 18 s  buttons 0  dropped 0", "[usb] uptime 18 s"] {
        let r = replay(&after_a_reset(&first, &[heartbeat], &then, 60_000));
        assert_eq!(r.resets, 1, "{heartbeat}");
        assert_eq!(r.disagreements(), Vec::<String>::new(), "{heartbeat}\n{}", r.render(false));
    }
}

/// A heartbeat a moment behind the trace line before it is not a reset.
#[test]
fn a_heartbeat_behind_the_trace_by_its_rounding_is_not_a_reset() {
    let capture = fixture("mac-cable.log");
    let last = clock(&capture).iter().map(|c| c.0).max().unwrap();
    let r = replay(&after_a_reset(&capture, &[&format!("[hb] 9  uptime {} s  buttons 0  dropped 0", last / 1_000)], "", 0));
    assert_eq!(r.resets, 0);
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces").join(name)).unwrap()
}

/// The chip and every central measured end an unanswered comparison 30 s after it shows.
#[test]
fn the_comparison_counts_down_from_the_security_managers_timeout() {
    let r = replay(&fixture("bluez-comparison-unanswered.log"));
    let shown = r.timeline.iter().find(|e| e.who == Who::Status && e.what.starts_with("pairing(")).expect("no comparison shown");
    assert!(shown.what.ends_with(",30s)"), "{}", shown.what);
}

/// A security request waits on the rebond a lost bond needs, so the rebond cannot wait on it.
#[test]
fn the_rebond_a_lost_bond_needs_goes_out_past_the_security_request() {
    let r = replay(&fixture("android17-bond-not-recognised.log"));
    let core = r.core_ops.iter().find(|(_, op)| op.kind() == OpKind::AllowRebond).expect("the core never allowed the rebond").0;
    let driver = r.driver_ops.iter().find(|(_, d)| *d == DriverOp::Other(OpKind::AllowRebond)).expect("the driver never did").0;
    assert!(core.abs_diff(driver) <= 5, "core {core}, driver {driver}");
}
