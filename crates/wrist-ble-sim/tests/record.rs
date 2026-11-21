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
    loop {
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
