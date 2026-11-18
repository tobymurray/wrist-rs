//! The watch's own driver, captured with `ble-trace`, replayed through the core.

use std::fmt::Write as _;
use std::path::Path;

use wrist_ble::{Instant, Op, OpKind};
use wrist_ble_core::engine::Omitted;
use wrist_ble_shadow::{trace, Excuse};
use wrist_ble_shadow::DriverOp;
use wrist_ble_sim::replay::{replay, Report, Who};

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

/// Where the core is known to act otherwise than the driver did, by fixture.
const KNOWN: &[(&str, &[&str])] = &[];

#[test]
fn the_core_acts_where_and_as_the_driver_did() {
    for (name, capture) in traces() {
        let report = replay(&capture);
        assert!(!report.core_ops.is_empty() && !report.driver_ops.is_empty(), "{name}: nothing replayed");
        let known = KNOWN.iter().find(|(f, _)| name.ends_with(f)).map_or(&[][..], |(_, d)| *d);
        assert_eq!(report.disagreements(), known, "{name}\n{}", report.render(false));
    }
}

/// Asks the core leaves out because the link is not yet open to them: the older driver's before
/// encryption, and one on a link the capture began inside, whose encryption the core never saw.
const NOT_OPEN: &[(&str, usize)] = &[("android14-comparison-unanswered.log", 3), ("android17-bond-not-recognised.log", 3), ("bluez-suite.log", 1)];

/// Each ask the driver made alone is one the core's rules leave out, and those that wait for the
/// link to open are the ones listed.
#[test]
fn the_asks_the_core_leaves_out_are_the_ones_its_rules_name() {
    let mut fits = 0;
    for (name, capture) in traces() {
        let report = replay(&capture);
        let not_open = report.excused.iter().filter(|(_, why)| *why == Excuse::Core(Omitted::NotOpen)).count();
        let listed = NOT_OPEN.iter().find(|(f, _)| name.ends_with(f)).map_or(0, |(_, n)| *n);
        assert_eq!(not_open, listed, "{name}\n{}", report.render(false));
        fits += report.excused.len() - not_open;
    }
    assert!(fits > 0, "a link that already fits is asked nothing");
}

#[test]
fn a_capture_that_begins_with_the_chip_up_is_taken_as_up() {
    let r = replay(&fixture("bluez-suite.log"));
    assert_eq!(r.disagreements(), Vec::<String>::new(), "{}", r.render(false));
    let (at, ops) = r.seeded.clone().expect("not seeded");
    let first = fixture("bluez-suite.log").lines().next().and_then(|l| l.split_whitespace().nth(2)?.parse::<u64>().ok());
    assert_eq!(Some(at), first);
    assert!(ops.iter().all(|op| op.kind() == OpKind::Advertise), "{ops:?}");
    assert_eq!(r.resets, 1, "the update's reset");
    let kinds = |k: OpKind| (r.driver_ops.iter().filter(|(_, d)| d.kind() == k).count(), r.core_ops.iter().filter(|(_, op)| op.kind() == k).count());
    let (driver, core) = kinds(OpKind::Advertise);
    assert_eq!(driver, core);
    assert!(driver > 40, "{driver}");
    assert_eq!(kinds(OpKind::RequestSecurity), (1, 1));
}

#[test]
fn a_capture_that_begins_as_the_chip_comes_up_is_a_start() {
    let from = |name: &str, first: &str| fixture(name).lines().skip_while(|l| !l.ends_with(first)).flat_map(|l| [l, "\n"]).collect::<String>();
    for capture in [from("bluez-cable.log", " r"), from("bluez-suite.log", " e 04FF03010001")] {
        let r = replay(&capture);
        assert_eq!(r.seeded, None, "{}", r.render(false));
        assert_eq!(r.disagreements(), Vec::<String>::new(), "{}", r.render(false));
    }
}

#[test]
fn only_a_disconnect_the_operator_asked_for_is_the_drivers_alone() {
    let with = fixture("bluez-cable.log");
    let without = with.lines().filter(|l| !l.ends_with(">> usb ble disconnect")).collect::<Vec<_>>().join("\n");
    assert_ne!(with, without);
    assert_eq!(replay(&with).disagreements(), Vec::<String>::new());
    assert_eq!(replay(&without).disagreements().len(), 1);
}

#[test]
fn the_cores_disagreements_are_listed_before_the_drivers() {
    let capture = fixture("android14-comparison-unanswered.log");
    // The driver's ask for parameters gone, so the core asks alone.
    let mut lines: Vec<&str> = capture.lines().filter(|l| !l.ends_with(" c 0181FD0A05080C0018000000F401")).collect();
    assert_eq!(lines.len() + 1, capture.lines().count());
    let at = lines.iter().position(|l| host_ms(l).is_some_and(|ms| ms > 355_211)).unwrap();
    lines.insert(at, "355.211 [trace] 945700 c 0195FC020408");
    let d = replay(&lines.join("\n")).disagreements();
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d[0].starts_with("10625") && d[0].contains(": the core asked") && d[1].starts_with("945700: the driver"), "{d:?}");
}

#[test]
fn an_ask_the_console_refuses_is_no_goal() {
    let capture = fixture("bluez-cable.log");
    let lines: Vec<&str> = capture.lines().take(64).collect();
    let last = lines.iter().filter_map(|l| host_ms(l)).max().unwrap();
    for ask in ["5 5", "24 12", "2000 2000", "6 x 6"] {
        let more = [format!("{}.{:03} >> usb ble interval now {ask}", (last + 1_000) / 1_000, (last + 1_000) % 1_000), format!("{}.000 == done", last / 1_000 + 10)];
        let all: Vec<&str> = lines.iter().copied().chain(more.iter().map(String::as_str)).collect();
        assert_eq!(replay(&all.join("\n")).disagreements(), Vec::<String>::new(), "{ask}");
    }
}

#[test]
fn a_core_that_acts_alone_is_a_disagreement() {
    let (name, capture) = &traces()[0];
    let without_driver = capture.lines().filter(|l| !l.contains(" c ")).collect::<Vec<_>>().join("\n");
    assert!(!replay(&without_driver).disagreements().is_empty(), "{name}");
}

fn hex(h: &str) -> Vec<u8> {
    (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect()
}

/// Kept lines only: what `tools/ble-trace-fixture.py` writes, so no fixture holds an address.
#[test]
fn traces_hold_only_what_the_scrub_keeps() {
    for (name, capture) in traces() {
        for line in capture.lines() {
            let (_, text) = line.split_once(' ').unwrap();
            if text.starts_with("== ") || text.starts_with(">> usb ble ") || text.starts_with(">> usb update ") || text.starts_with("[update] ") {
                let joined: String = text.chars().filter(|c| !matches!(c, ':' | '-' | '_')).collect();
                assert!(!joined.split(|c: char| !c.is_ascii_hexdigit()).any(|w| w.len() >= 12), "{name}: an address in {line}");
                continue;
            }
            let f: Vec<&str> = text.split(' ').collect();
            assert_eq!(f[0], "[trace]", "{name}: {line}");
            match f[2] {
                "r" | "l" => assert_eq!(f.len(), 3, "{name}: {line}"),
                "m" => assert!(f.len() == 4 && matches!(f[3], "00" | "01"), "{name}: {line}"),
                "s" => assert!(f.len() == 3 || trace::decode(Instant(0), &hex(f[3])).is_some(), "{name}: {line}"),
                "c" | "e" => {
                    let p = hex(f[3]);
                    assert!(p.len() >= 3, "{name}: {line}");
                    if f[2] == "e" {
                        assert_eq!(p[0], 0x04, "{name}: {line}");
                        if matches!(p[1], 0x0E | 0x0F) {
                            assert_eq!((p.len(), p[2]), (7, 4), "{name}: more than a reply's status in {line}");
                        }
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

/// The chip and every central end an unanswered comparison at that timeout (HARDWARE.md).
#[test]
fn the_comparison_counts_down_from_the_security_managers_timeout() {
    let r = replay(&fixture("bluez-comparison-unanswered.log"));
    let shown = r.timeline.iter().find(|e| e.who == Who::Status && e.what.starts_with("pairing(")).expect("no comparison shown");
    assert!(shown.what.ends_with(",30s)"), "{}", shown.what);
    let countdown: Vec<u64> = r.timeline.iter().filter(|e| e.who == Who::Status && e.what.starts_with("pairing(")).map(|e| e.at).collect();
    assert!(countdown.len() == 30 && countdown.windows(2).all(|w| w[1] - w[0] == 1_000), "{countdown:?}");
}

/// A security request waits on the rebond a lost bond needs, so the rebond cannot wait on it.
#[test]
fn the_rebond_a_lost_bond_needs_goes_out_past_the_security_request() {
    let r = replay(&fixture("android17-bond-not-recognised.log"));
    let core = r.core_ops.iter().find(|(_, op)| op.kind() == OpKind::AllowRebond).expect("the core never allowed the rebond").0;
    let driver = r.driver_ops.iter().find(|(_, d)| *d == DriverOp::Other(OpKind::AllowRebond)).expect("the driver never did").0;
    assert!(core.abs_diff(driver) <= 5, "core {core}, driver {driver}");
}

/// A capture stamped by the wall clock replays as one stamped in seconds.
#[test]
fn wall_clock_stamps_replay_as_seconds_do() {
    let on_the_clock = |c: &str| c.lines().next().is_some_and(|l| l.as_bytes().get(2) == Some(&b':'));
    assert!(on_the_clock(&fixture("bluez-suite.log")));
    for (name, capture) in traces().into_iter().filter(|(_, c)| !on_the_clock(c)) {
        let restamped: String = capture
            .lines()
            .map(|line| match line.trim_start().split_once(' ') {
                Some((t, rest)) if t.contains('.') && t.split('.').next().is_some_and(|s| s.parse::<u64>().is_ok()) => {
                    let (s, ms) = t.split_once('.').unwrap();
                    let s = 9 * 3600 + 27 * 60 + s.parse::<u64>().unwrap();
                    format!("{:02}:{:02}:{:02}.{ms} {rest}\n", s / 3600, s / 60 % 60, s % 60)
                }
                _ => format!("{line}\n"),
            })
            .collect();
        assert_ne!(restamped, capture, "{name}: nothing restamped");
        let (a, b) = (replay(&capture), replay(&restamped));
        assert_eq!(format!("{:?}", a.core_ops), format!("{:?}", b.core_ops), "{name}");
        assert_eq!(format!("{:?}", a.driver_ops), format!("{:?}", b.driver_ops), "{name}");
        assert_eq!(a.disagreements(), b.disagreements(), "{name}");
    }
}

/// An image over the air is a transfer from its `[update] send` line to the line that ends it,
/// which ends the console's `interval now` ask on the watch as in the core: without the lines,
/// the core keeps the ask, makes it again after the settle, and never makes the battery's.
#[test]
fn an_update_over_the_air_is_a_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    assert_eq!(replay(&capture).disagreements(), Vec::<String>::new());
    let without = capture.lines().filter(|l| !l.contains(" [update] ")).collect::<Vec<_>>().join("\n");
    let d = replay(&without).disagreements();
    assert!(d.len() == 2 && d[0].contains("the core asked for 12-12 units") && d[1].contains("the driver asked for 80-160 units"), "{d:?}");
}

/// A USB `update` sends its image over the cable, not the phone's link, so it is no transfer.
#[test]
fn an_update_over_usb_is_no_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let send = capture.lines().find(|l| l.contains(" [update] send ")).unwrap();
    let at = send.split_once(' ').unwrap().0;
    let over_usb = capture.replace(send, &format!("{at} >> usb update 345624 FE7A7220\n{send}"));
    let d = replay(&over_usb).disagreements();
    assert!(d.len() == 2 && d[0].contains("the core asked for 12-12 units") && d[1].contains("the driver asked for 80-160 units"), "{d:?}");
}

/// A USB `update` the watch refused before sending leaves no mark on a later image over the air.
#[test]
fn a_refused_usb_update_does_not_hide_a_later_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    lines.insert(1, "2500.000 >> usb update 999999999 0");
    lines.insert(2, "2500.050 [update] error: length out of range for this target");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// A profile changed during a transfer keeps the transfer's timing: the replay's goal for it is
/// the transfer, not the profile alone.
#[test]
fn a_profile_changed_during_a_transfer_keeps_it() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    let at = lines.iter().position(|l| l.contains(" [update] received 16492")).unwrap();
    lines.insert(at + 1, "2530.000 >> usb ble profile battery");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// An `update` the console does not take as one marks no USB update, so the image over the air
/// after it is a transfer.
#[test]
fn an_update_the_console_rejects_does_not_hide_a_later_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    lines.insert(1, "2500.000 >> usb update 5 0x1234");
    lines.insert(2, "2500.050 [usb] unknown command: update 5 0x1234");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// A USB `update` refused while an image comes over the air leaves that transfer running.
#[test]
fn a_usb_update_refused_during_a_transfer_leaves_it() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    let at = lines.iter().position(|l| l.contains(" [update] received 16492")).unwrap();
    lines.insert(at + 1, "2530.000 >> usb update 5 1234");
    lines.insert(at + 2, "2530.050 [update] error: a transfer is already in progress");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// On the console's cable, `ble profile auto` is cable timing.
#[test]
fn a_profile_put_back_to_auto_is_cable_timing() {
    let capture = fixture("mac-battery.log");
    let auto = capture.replace(">> usb ble profile cable\n", ">> usb ble profile auto\n");
    assert_ne!(auto, capture);
    assert_eq!(replay(&auto).disagreements(), Vec::<String>::new());
}

/// A length the watch's `usize` can't hold makes no USB update.
#[test]
fn an_update_too_long_for_the_watch_is_no_usb_update() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    lines.insert(1, "2500.000 >> usb update 4294967296 1234");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// Progress over the air between the console's `update` and its refusal leaves the transfer.
#[test]
fn progress_before_a_usb_refusal_leaves_the_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    let at = lines.iter().position(|l| l.contains(" [update] received 16492")).unwrap();
    for (i, line) in ["2529.600 >> usb update 5 1234", "2529.601 [update] received 16493", "2529.602 [update] error: a transfer is already in progress"].into_iter().enumerate() {
        lines.insert(at + 1 + i, line);
    }
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// The chip's reply to the driver's security request reaches the core: refused for good, the
/// pairing fails at once and the link is asked for parameters; refused otherwise, it waits.
#[test]
fn a_security_request_the_chip_refuses_for_good_fails_the_pairing_at_once() {
    for (status, fails) in [("40", true), ("0C", false)] {
        let mut capture: String = fixture("bluez-update-on-battery.log")
            .lines()
            .filter(|l| !l.contains(" e 04FF0601") && !l.contains(" e 040804"))
            .take_while(|l| !l.contains("[update] send"))
            .flat_map(|l| {
                let reply = if l.ends_with(" c 018DFC024308") { format!("2515.300 [trace] 2462560 e 040F04{status}018DFC\n") } else { String::new() };
                [format!("{l}\n"), reply]
            })
            .collect();
        capture.push_str("2600.000 [trace] 2510000 e 04FF020404\n");
        let r = replay(&capture);
        let asked = r.core_ops.iter().any(|(_, op)| op.kind() == OpKind::UpdateParams);
        assert_eq!(asked, fails, "status {status}\n{}", r.render(false));
        assert_eq!(r.disagreements(), [] as [String; 0], "status {status}");
    }
}

/// A USB `update` the console never answered, its command lost, hides no image over the air of
/// another length.
#[test]
fn an_unanswered_usb_update_does_not_hide_a_later_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    lines.insert(1, "2500.000 >> usb update 5 1234");
    lines.insert(2, "2500.001 [usb] rx overflow: a packet was lost");
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// While a USB `update` waits, an error only an image already sent can print is the image's
/// over the air, and ends its transfer.
#[test]
fn an_error_after_a_send_ends_the_transfer_while_a_usb_update_waits() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    let verified = lines.iter().position(|l| l.contains(" [update] received and verified ")).unwrap();
    lines[verified] = "2625.693 [update] error: transfer incomplete";
    lines.remove(verified + 1);
    lines.truncate(verified + 2);
    let at = lines.iter().position(|l| l.contains(" [update] received 16492")).unwrap();
    lines.insert(at + 1, "2530.000 >> usb update 5 1234");
    let r = replay(&lines.join("\n"));
    assert_eq!(r.disagreements(), Vec::<String>::new());
    let battery = |op: &Op| matches!(op, Op::UpdateParams(_, p) if p.interval_min.as_u16() == 80);
    assert!(r.core_ops.last().is_some_and(|(_, op)| battery(op)), "the core asks the battery's timing once the transfer ends\n{}", r.render(false));
}

/// A USB `update` sent and answered leaves the next image over the air its transfer, even one of
/// the same length.
#[test]
fn a_usb_update_answered_leaves_the_next_transfer() {
    let capture = fixture("bluez-update-on-battery.log");
    let mut lines: Vec<&str> = capture.lines().collect();
    for (i, line) in ["2500.000 >> usb update 345624 1234", "2500.001 [update] send 345624 bytes", "2500.002 [update] error: crc mismatch; nothing written"].into_iter().enumerate() {
        lines.insert(1 + i, line);
    }
    assert_eq!(replay(&lines.join("\n")).disagreements(), Vec::<String>::new());
}

/// What a trace line is: `c`, `e`, `l`, `r` or `s`.
fn kind(line: &str) -> Option<&str> {
    let (_, text) = line.split_once(" [trace] ")?;
    text.split_whitespace().nth(1)
}

/// The core told a lost read of the driver's on the watch (`ble probe lose link`, the connection
/// withheld from it) resyncs from the driver's snapshot in the replay too; without them it never
/// learns of the link.
#[test]
fn the_drivers_lost_read_and_snapshot_are_the_cores() {
    let with = fixture("bluez-lost-read.log");
    let r = replay(&with);
    assert_eq!(r.disagreements(), Vec::<String>::new(), "{}", r.render(false));
    assert_eq!(r.core_ops.iter().filter(|(_, op)| op.kind() == OpKind::UpdateParams).count(), 1);
    let without: String = with.lines().filter(|l| !matches!(kind(l), Some("l" | "s"))).flat_map(|l| [l, "\n"]).collect();
    assert_eq!(with.lines().count(), without.lines().count() + 2);
    assert_eq!(replay(&without).disagreements().len(), 2);
}

/// A snapshot traced as failed leaves the core without the link, and one it did not ask for is
/// not its; one the replay can't read is an event the core could not.
#[test]
fn a_failed_snapshot_of_the_drivers_is_none() {
    let with = fixture("bluez-lost-read.log");
    let taken = with.lines().find(|l| kind(l) == Some("s")).unwrap();
    let failed = taken.rsplit_once(' ').unwrap().0;
    let r = replay(&with.replace(taken, failed));
    assert_eq!((r.disagreements().len(), r.unknown_events), (2, 0));
    // The core asks again only after its retry, so a snapshot straight after is none of its.
    let r = replay(&with.replace(taken, &format!("{failed}\n{taken}")));
    assert_eq!(r.disagreements().len(), 2);    assert_eq!(replay(&with.replace(taken, &format!("{failed} 08"))).unknown_events, 1);
}

/// The chip's own report of lost events in the driver's lost read's place.
fn chip_lost(capture: &str) -> String {
    let lost = capture.lines().find(|l| kind(l) == Some("l")).unwrap();
    capture.replace(lost, &format!("{} e 04FF0A02000100000000000000", lost.strip_suffix(" l").unwrap()))
}

fn snapshots(r: &Report) -> usize {
    r.timeline.iter().filter(|e| e.who == Who::Core && e.what == "snapshot").count()
}

/// A driven core resyncs from the driver's snapshot whoever saw the events lost, the chip or the
/// driver; a core the driver doesn't drive is answered by the shadow, once.
#[test]
fn a_driven_cores_snapshots_are_the_drivers_however_the_loss_was_seen() {
    let driven = chip_lost(&fixture("bluez-lost-read.log"));
    let r = replay(&driven);
    assert_eq!((r.disagreements(), snapshots(&r)), (Vec::<String>::new(), 1), "{}", r.render(false));
    let taken = driven.lines().find(|l| kind(l) == Some("s")).unwrap();
    assert_eq!(snapshots(&replay(&driven.replace(taken, ""))), 0);
    let legacy = driven.replace(" m 01", " m 00");
    assert_ne!(legacy, driven);
    let r = replay(&legacy);
    assert_eq!((r.disagreements().len(), snapshots(&r)), (2, 1));
}
