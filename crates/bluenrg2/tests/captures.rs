//! Events a real controller sent, decoded.

use std::fmt::Write;

mod common;

use bluenrg2::{BleStatus, ConnHandle, Duration, EncryptionEnabledLevel, LeConnRole};
use common::{parse, Ev, Timing};

fn packets() -> Vec<Vec<u8>> {
    decode(include_str!("captures/bluez-reconnect.hex"))
}

fn decode(capture: &str) -> Vec<Vec<u8>> {
    capture
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let [ms, hex] = l.split(' ').collect::<Vec<_>>()[..] else { panic!("not `ms hex`: {l}") };
            assert!(ms.parse::<u32>().is_ok(), "not `ms hex`: {l}");
            let mut p = vec![0x04];
            p.extend((0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()));
            p
        })
        .collect()
}

#[test]
fn a_captured_connection_decodes_whole() {
    let packets = packets();
    let events: Vec<Ev<'_>> = packets.iter().map(|p| parse(p)).collect();
    assert!(!events.iter().any(|e| matches!(e, Ev::Malformed(_) | Ev::Unknown { .. })), "{events:?}");
    let handle = ConnHandle::new(0x0E01);
    assert!(matches!(events[0], Ev::LeConnectionComplete { handle: h, role: LeConnRole::Central, timing: Some(Timing { peripheral_latency: 0, conn_interval, .. }), .. } if h == handle && conn_interval.as_micros() == 30_000));
    assert_eq!(events[1], Ev::LeUnknown { subevent_code: 0x14 });
    assert_eq!(events[2], Ev::LeUnknown { subevent_code: 0x04 });
    assert_eq!(events[3], Ev::LeDataLengthChange { handle, max_tx_octets: 251, max_tx_time: 2120, max_rx_octets: 27, max_rx_time: 328 });
    assert_eq!(events[4], Ev::EncryptionChange { handle, status: BleStatus::SUCCESS, enabled: EncryptionEnabledLevel::OnE0OrAesCcm });
    assert_eq!(events[5], Ev::LeConnectionUpdateComplete { handle, conn_interval: Duration::from_millis(30), peripheral_latency: 0, supervision_timeout: Duration::from_secs(5) });
    assert_eq!(events[6], Ev::DisconnectionComplete { handle, status: BleStatus::SUCCESS, reason: BleStatus::new(0x16) });
    assert!(matches!(events[7], Ev::LeConnectionComplete { .. }));
    assert_eq!(events.last(), Some(&Ev::DisconnectionComplete { handle, status: BleStatus::SUCCESS, reason: BleStatus::new(0x3E) }));
}

/// A kept event's parameter length, and where it holds an address as ranges of its parameters
/// after the subevent code; `None` for an event a capture may not hold.
fn kept(event: &[u8]) -> Option<(u8, &'static [(usize, usize)])> {
    match (event[1], event.get(3)) {
        (0x05 | 0x08, _) | (0x3E, Some(0x14)) => Some((4, &[])),
        (0x30, _) => Some((3, &[])),
        (0x3E, Some(0x01)) => Some((19, &[(5, 11)])),
        (0x3E, Some(0x03)) => Some((10, &[])),
        (0x3E, Some(0x04)) => Some((12, &[])),
        (0x3E, Some(0x06 | 0x07)) => Some((11, &[])),
        (0x3E, Some(0x0A)) => Some((31, &[(5, 23)])),
        (0x3E, Some(0x0C)) => Some((6, &[])),
        (0x3E, Some(0x29)) => Some((34, &[(5, 23)])),
        _ => None,
    }
}

/// Whether a comment could hold an address.
fn holds_address(comment: &str) -> bool {
    let b = comment.as_bytes();
    let separated = b.windows(17).any(|w| {
        w.chunks(3).all(|c| c[0].is_ascii_hexdigit() && c[1].is_ascii_hexdigit() && c.get(2).is_none_or(|&s| s == b':' || s == b'-'))
    });
    separated || b.split(|c| !c.is_ascii_hexdigit()).any(|run| run.len() >= 12)
}

fn check(name: &str, capture: &str) {
    for comment in capture.lines().filter(|l| l.starts_with('#')) {
        assert!(!holds_address(comment), "{name}: an address in {comment}");
    }
    for p in decode(capture) {
        assert_eq!(usize::from(p[2]), p.len() - 3, "{name}: bytes beyond the event in {p:02x?}");
        let (len, ranges) = kept(&p).unwrap_or_else(|| panic!("{name}: an event the scrub does not keep: {p:02x?}"));
        assert_eq!(p[2], len, "{name}: an event of the wrong length in {p:02x?}");
        for &(lo, hi) in ranges {
            assert!(p[4 + lo..4 + hi].iter().all(|&b| b == 0), "{name}: an address in {p:02x?}");
        }
    }
}

#[test]
fn captures_hold_no_keys_and_no_addresses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/captures");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        check(&path.display().to_string(), &std::fs::read_to_string(&path).unwrap());
    }
}

#[test]
#[should_panic(expected = "an address")]
fn an_address_in_a_comment_is_refused() {
    check("t", "# peer AA:BB:CC:DD:EE:FF\n");
}

#[test]
#[should_panic(expected = "an address")]
fn a_dash_separated_address_in_a_comment_is_refused() {
    check("t", "# peer aa-bb-cc-dd-ee-ff\n");
}

#[test]
#[should_panic(expected = "the wrong length")]
fn an_event_longer_than_its_kind_is_refused() {
    check("t", "0 050a00010e16aabbccddeeff\n");
}

#[test]
#[should_panic(expected = "an address")]
fn a_commented_out_event_is_refused() {
    check("t", "# 0 3e1f0a00010e0001aabbccddeeff112233445566778899aabb18000000f40100\n");
}

#[test]
#[should_panic(expected = "not `ms hex`")]
fn a_line_with_more_than_one_packet_is_refused() {
    check("t", "0 050400010e16 3e1f0a00010e0001aabbccddeeff\n");
}

#[test]
#[should_panic(expected = "not `ms hex`")]
fn a_first_field_that_is_not_a_time_is_refused() {
    check("t", "AA:BB:CC:DD:EE:FF 050400010e16\n");
}

#[test]
fn every_kind_the_scrub_keeps_passes_at_its_length() {
    let kinds: [&[u8]; 12] = [&[0x05, 4], &[0x08, 4], &[0x30, 3], &[0x3E, 19, 0x01], &[0x3E, 10, 0x03], &[0x3E, 12, 0x04],
        &[0x3E, 11, 0x06], &[0x3E, 11, 0x07], &[0x3E, 31, 0x0A], &[0x3E, 6, 0x0C], &[0x3E, 4, 0x14], &[0x3E, 34, 0x29]];
    for kind in kinds {
        let mut event = kind.to_vec();
        event.resize(2 + usize::from(kind[1]), 0);
        let mut line = String::from("0 ");
        for b in &event {
            write!(line, "{b:02x}").unwrap();
        }
        check("t", &line);
    }
}

#[test]
#[should_panic(expected = "bytes beyond")]
fn bytes_past_an_event_are_refused() {
    check("t", "0 050400010e16ff\n");
}
