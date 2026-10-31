//! Events a real controller sent, decoded.

use wrist_aci::{parse, Event};

fn packets() -> Vec<Vec<u8>> {
    include_str!("captures/bluez-reconnect.hex")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let hex = l.split(' ').nth(1).unwrap();
            let mut p = vec![0x04];
            p.extend((0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()));
            p
        })
        .collect()
}

#[test]
fn a_captured_connection_decodes_whole() {
    let packets = packets();
    let events: Vec<Event<'_>> = packets.iter().map(|p| parse(p)).collect();
    assert!(!events.iter().any(|e| matches!(e, Event::Malformed(_) | Event::Unknown { .. })), "{events:?}");
    let handle = 0x0E01;
    assert!(matches!(events[0], Event::LeConnectionComplete { handle: h, central: true, conn_interval: Some((24, 0)), .. } if h == handle));
    assert_eq!(events[1], Event::LeUnknown { subevent_code: 0x14 });
    assert_eq!(events[2], Event::LeUnknown { subevent_code: 0x04 });
    assert_eq!(events[3], Event::LeDataLengthChange { handle, max_tx_octets: 251, max_tx_time: 2120, max_rx_octets: 27, max_rx_time: 328 });
    assert_eq!(events[4], Event::EncryptionChange { handle, status: 0, enabled: 1 });
    assert_eq!(events[5], Event::LeConnectionUpdateComplete { handle, conn_interval: 24, peripheral_latency: 0 });
    assert_eq!(events[6], Event::DisconnectionComplete { handle, status: 0, reason: 0x16 });
    assert!(matches!(events[7], Event::LeConnectionComplete { .. }));
    assert_eq!(events.last(), Some(&Event::DisconnectionComplete { handle, status: 0, reason: 0x3E }));
}
