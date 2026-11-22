//! The flight recorder's ring and lines: a capture's records kept and read back as they were, and
//! the goal records driving the core as the console's commands did.

use std::fmt::Write;
use std::path::Path;

use wrist_ble::policy::goals::{Goals, PhoneGoal, Power, ScanGoal};
use wrist_ble::Millis;
use wrist_ble_sim::capture;
use wrist_ble_sim::replay::replay;
use wrist_bluenrg2::shadow::record::{decode_goals, encode_goals, Next, Ring, KEPT};

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces");
    let mut all: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| (p.display().to_string(), std::fs::read_to_string(&p).unwrap()))
        .collect();
    all.sort();
    all
}

fn trace_lines(text: &str) -> Vec<&str> {
    text.lines().filter_map(|l| l.find("[trace] ").map(|i| l[i..].trim_end())).collect()
}

fn read_all<const N: usize>(ring: &Ring<N>, mut at: u64) -> (Vec<String>, u64) {
    let mut out = Vec::new();
    let mut lost = 0;
    for _ in 0..1 << 16 {
        match ring.next(at) {
            Next::Record(r, after) => {
                let mut s = String::new();
                r.write(&mut s).unwrap();
                out.push(s);
                at = after;
            }
            Next::Lost(from) => {
                lost += from - at;
                at = from;
            }
            Next::End => return (out, lost),
        }
    }
    panic!("no end to the ring");
}

/// Every capture's records, kept and read back, are the lines the capture held.
#[test]
fn a_captures_records_read_back_as_they_were() {
    for (name, text) in fixtures() {
        let lines = trace_lines(&text);
        let mut ring = Ring::<{ 1 << 20 }>::NEW;
        for line in &lines {
            let t = capture::trace(line).unwrap_or_else(|| panic!("{name}: {line}"));
            let mut bytes = t.packet.clone().unwrap_or_default();
            if t.cut {
                let rest: usize = line.rsplit(" +").next().unwrap().parse().unwrap();
                bytes.resize(bytes.len() + rest, 0);
            }
            ring.push(t.ms, t.kind.as_bytes()[0], &bytes);
        }
        let (back, lost) = read_all(&ring, 0);
        assert_eq!(lost, 0, "{name}");
        assert_eq!(back, lines, "{name}");
    }
}

#[test]
fn a_full_ring_drops_its_oldest_records_and_says_how_much() {
    let mut ring = Ring::<64>::NEW;
    for i in 0..5u8 {
        ring.push(u64::from(i), b'e', &[i; 10]);
    }
    let (back, lost) = read_all(&ring, 0);
    assert_eq!(back, ["[trace] 2 e 02020202020202020202", "[trace] 3 e 03030303030303030303", "[trace] 4 e 04040404040404040404"]);
    assert_eq!(lost, 2 * 18);
    assert_eq!(ring.end(), 5 * 18);
    assert_eq!(ring.next(ring.end()), Next::End);
}

#[test]
fn a_long_packet_is_kept_to_its_first_bytes_and_counted() {
    let mut ring = Ring::<256>::NEW;
    ring.push(7, b'e', &[0xAB; KEPT + 9]);
    let (back, _) = read_all(&ring, 0);
    assert_eq!(back, [format!("[trace] 7 e {} +9", "AB".repeat(KEPT))]);
}

/// A position inside a record is read from the next record, never from inside one.
#[test]
fn a_position_inside_a_record_is_read_from_the_next() {
    let mut ring = Ring::<256>::NEW;
    ring.push(1, b'e', &[0x61; 20]);
    ring.push(2, b'e', &[0x61; 20]);
    assert_eq!(ring.align(0), 0);
    for at in 1..=28 {
        assert_eq!(ring.align(at), 28, "{at}");
    }
    for at in 29..=60 {
        assert_eq!(ring.align(at), 56, "{at}");
    }
    let mut full = Ring::<64>::NEW;
    for i in 0..5u8 {
        full.push(u64::from(i), b'e', &[i; 10]);
    }
    assert_eq!(full.align(0), 2 * 18);
}

#[test]
fn goals_are_read_back_as_they_were_written() {
    let goals = [
        Goals::OFF,
        Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Battery, ..Goals::OFF },
        Goals { radio_on: true, transfer: true, workout: true, scan: Some(ScanGoal { duration: Millis(10_000) }), ..Goals::OFF },
    ];
    for g in goals {
        let mut bytes = [0; 5];
        let n = encode_goals(&g, &mut bytes);
        assert_eq!(format!("{:?}", decode_goals(&bytes[..n])), format!("{:?}", Some(g)));
    }
    for bad in [&[][..], &[0x80], &[0x40], &[0x00, 1]] {
        assert!(decode_goals(bad).is_none(), "{bad:?}");
    }
}

/// A capture with its console commands written as the recorder writes the core's goals replays to
/// the same operations: a dump, which holds no console lines, drives the core as a capture does.
#[test]
fn the_recorders_goal_records_drive_the_core_as_the_commands_did() {
    let boot = Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF };
    let mut checked = 0;
    for (name, text) in fixtures() {
        let mut last_ms = 0;
        let mut rewritten = String::new();
        let mut any = false;
        for line in text.lines() {
            let record = if line.contains(">> usb ble off") {
                Some("o 00".to_string())
            } else if line.contains(">> usb ble on") {
                let mut bytes = [0; 5];
                let n = encode_goals(&boot, &mut bytes);
                Some(bytes[..n].iter().fold("G ".to_string(), |mut s, b| {
                    write!(s, "{b:02X}").unwrap();
                    s
                }))
            } else {
                None
            };
            if let Some(t) = line.find("[trace] ").and_then(|i| capture::trace(&line[i..])) {
                last_ms = t.ms;
            }
            if let Some(r) = record {
                writeln!(rewritten, "[trace] {last_ms} {r}").unwrap();
                any = true;
            } else {
                rewritten.push_str(line);
                rewritten.push('\n');
            }
        }
        if !any {
            continue;
        }
        let (was, now) = (replay(&text), replay(&rewritten));
        assert_eq!(now.core_ops, was.core_ops, "{name}");
        assert_eq!(now.found, was.found, "{name}");
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn records_that_exactly_fill_the_ring_are_all_kept() {
    let mut ring = Ring::<54>::NEW;
    for i in 0..3u8 {
        ring.push(u64::from(i), b'e', &[i; 10]);
    }
    assert_eq!(read_all(&ring, 0), (vec![
        "[trace] 0 e 00000000000000000000".to_string(),
        "[trace] 1 e 01010101010101010101".to_string(),
        "[trace] 2 e 02020202020202020202".to_string(),
    ], 0));
}

/// Read from where no record starts, bytes that do not make a whole record are no record.
#[test]
fn bytes_that_are_no_whole_record_read_as_the_end() {
    // A length too short, a kind no record has, and a length past the newest byte.
    for (at_len, at_kind, bytes) in [(4u8, b'e', 0usize), (12, b'Z', 0), (40, b'e', 0)] {
        let mut ring = Ring::<256>::NEW;
        let mut payload = [0u8; 20];
        payload[0] = at_len;
        payload[5] = at_kind;
        ring.push(1, b'e', &payload[..20 - bytes]);
        assert_eq!(ring.next(8), Next::End, "{at_len} {at_kind}");
    }
    let mut ring = Ring::<256>::NEW;
    let mut payload = [0u8; 20];
    payload[0] = 12;
    payload[5] = b'e';
    ring.push(1, b'e', &payload);
    assert!(matches!(ring.next(8), Next::Record(r, 20) if r.bytes().len() == 4));
}

#[test]
fn a_length_longer_than_any_record_is_no_record() {
    let mut ring = Ring::<256>::NEW;
    let mut payload = [0u8; 20];
    payload[0] = 60;
    payload[5] = b'e';
    ring.push(1, b'e', &payload);
    for _ in 0..4 {
        ring.push(2, b'e', &[0; 30]);
    }
    assert_eq!(ring.next(8), Next::End);
}

/// A console notification keeps its command's header; every other record its first bytes.
#[test]
fn a_console_notification_keeps_only_its_header() {
    use wrist_bluenrg2::shadow::record::{kept, NOTIFY_KEPT};
    let notify = [0x01, 0x06, 0xFD, 0x20, 0x13, 0x00, 0x16, 0x00, 0x00, 0x1E, b'[', b'b'];
    assert_eq!(kept(b'c', &notify), NOTIFY_KEPT);
    assert_eq!(kept(b'e', &notify), KEPT);
    assert_eq!(kept(b'c', &[0x01, 0x83, 0xFC, 0x00]), KEPT);
    let mut ring = Ring::<256>::NEW;
    ring.push_kept(5, b'c', &notify, kept(b'c', &notify));
    assert_eq!(read_all(&ring, 0).0, ["[trace] 5 c 0106FD2013001600 +4"]);
}

/// A dump that begins mid-session with a scan's goals is no start: the core catches up as for any
/// capture that begins with the chip up.
#[test]
fn a_dump_that_begins_with_a_scans_goals_is_caught_up() {
    let fixture = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces/watch-dump-cable.log")).unwrap();
    let mut cut = String::from("33.100 [trace] 33100 G 07\n");
    for line in fixture.lines() {
        if capture::stamped(line).is_some_and(|s| s.ms >= 33_113) {
            writeln!(cut, "{line}").unwrap();
        }
    }
    let r = replay(&cut);
    assert!(r.seeded.is_some());
    assert_eq!(r.found, vec![]);
}

#[test]
fn a_held_ring_keeps_what_it_has_and_counts_what_it_refused() {
    let mut ring = Ring::<64>::NEW;
    for i in 0..3u8 {
        ring.push(u64::from(i), b'e', &[i; 10]);
    }
    assert_eq!(ring.hold(true), 0);
    ring.push(3, b'e', &[3; 10]);
    ring.push(4, b'e', &[4; 10]);
    let (back, lost) = read_all(&ring, 0);
    assert_eq!((back.len(), lost), (3, 0));
    assert_eq!(ring.hold(false), 2);
    ring.push(5, b'e', &[5; 10]);
    assert_eq!(read_all(&ring, 0).0.last().map(String::as_str), Some("[trace] 5 e 05050505050505050505"));
}

/// Held, a ring with room for a record, to its last byte, still keeps it.
#[test]
fn a_held_ring_with_room_keeps_new_records() {
    let mut ring = Ring::<54>::NEW;
    ring.hold(true);
    for i in 0..3u8 {
        ring.push(u64::from(i), b'e', &[i; 10]);
    }
    assert_eq!(read_all(&ring, 0).0.len(), 3);
    assert_eq!(ring.hold(false), 0);
}

#[test]
fn a_held_ring_past_its_first_records_measures_room_from_its_oldest() {
    let mut ring = Ring::<62>::NEW;
    for i in 0..4u8 {
        ring.push(u64::from(i), b'e', &[i; 10]);
    }
    ring.hold(true);
    ring.push(9, b'r', &[]);
    assert_eq!(read_all(&ring, 0).0.last().map(String::as_str), Some("[trace] 9 r"));
    assert_eq!(ring.hold(false), 0);
}

/// Records the watch did not keep while dumping are reported beside the verdicts.
#[test]
fn records_not_kept_while_dumping_are_reported() {
    let fixture = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces/watch-dump-cable.log")).unwrap();
    let with_gap = format!("{fixture}69.300 [trace] 69300 x 03000000\n");
    let r = replay(&with_gap);
    assert_eq!(r.not_kept, 3);
    assert!(wrist_ble_sim::fixture::keep("[trace] 69300 x 03000000").is_some());
}

/// A dump's own word on the bytes it lost, and a gap record that cannot be read, are reported.
#[test]
fn a_dumps_losses_and_a_bad_gap_record_are_reported() {
    let fixture = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces/watch-dump-cable.log")).unwrap();
    let capture = format!("{fixture}69.300 [trace] 69300 x 0300\n[ble] dump next 18751 at 125.119, 4096 bytes lost\n");
    let r = replay(&capture);
    assert_eq!((r.dump_lost, r.not_kept, r.unknown_events), (4096, 0, 1));
}

/// A capture caught up from its first dump's lost head reports only the losses after it.
#[test]
fn only_the_first_dumps_lost_head_is_caught_up_from() {
    let fixture = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/traces/watch-dump-cable.log")).unwrap();
    let mut late = String::new();
    for line in fixture.lines() {
        if capture::stamped(line).is_some_and(|s| s.ms >= 33_113) {
            writeln!(late, "{line}").unwrap();
        }
    }
    let first = format!("{late}[ble] dump next 18751 at 70.000, 4096 bytes lost\n");
    assert_eq!(replay(&first).dump_lost, 0);
    let second = format!("{first}[ble] dump next 20000 at 80.000, 100 bytes lost\n");
    assert_eq!(replay(&second).dump_lost, 100);
}
