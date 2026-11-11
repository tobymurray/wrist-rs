//! Packets as the BlueNRG-2 sends them.

mod common;

use bluenrg2::{
    ad_name, ad_uuid128s, ad_uuid16s, bonded_devices, command, header_sizes, packet_len,
    packet_whole, reports, vendor, AddrKind, Address, BdAddr, BleStatus, ConnHandle, Duration,
    EncryptionEnabledLevel, LeAdvEventKind, LeConnRole, PairingStatus,
};
use common::{parse, Ev, Timing, Vn};

fn link() -> ConnHandle {
    ConnHandle::new(0x0801)
}

fn timing(units: u16) -> Timing {
    Timing {
        conn_interval: Duration::from_u16(units),
        peripheral_latency: 0,
        supervision_timeout: Duration::from_secs(5),
    }
}

/// A vendor event packet: `04 FF len code params`.
fn vendor_event(code: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, 0xFF, u8::try_from(2 + params.len()).unwrap()];
    p.extend_from_slice(&code.to_le_bytes());
    p.extend_from_slice(params);
    p
}

#[test]
fn header_sizes_read_both_fields() {
    // A write header answered with 265 bytes of room.
    assert_eq!(header_sizes(&[0x00, 0x09, 0x01, 0x00, 0x00]), (265, 0));
    assert_eq!(header_sizes(&[0x00, 0x09, 0x01, 0xE6, 0x00]), (265, 230));
}

#[test]
fn command_packets_carry_opcode_and_length() {
    let mut out = [0u8; 16];
    let n = command(0xFD81, &[&[0x01, 0x08, 12, 0, 24, 0, 0, 0, 0xF4, 0x01]], &mut out).unwrap();
    assert_eq!(&out[..n], &[0x01, 0x81, 0xFD, 10, 0x01, 0x08, 12, 0, 24, 0, 0, 0, 0xF4, 0x01]);
    let n = command(0x1001, &[], &mut out).unwrap();
    assert_eq!(&out[..n], &[0x01, 0x01, 0x10, 0]);
}

#[test]
fn a_commands_parts_are_written_one_after_another() {
    let mut out = [0u8; 16];
    let n = command(0xFC88, &[&[0x01, 0x08], &[], &123_456u32.to_le_bytes()], &mut out).unwrap();
    assert_eq!(&out[..n], &[0x01, 0x88, 0xFC, 6, 0x01, 0x08, 0x40, 0xE2, 0x01, 0x00]);
}

#[test]
fn a_command_that_does_not_fit_is_not_written() {
    let mut small = [0xAAu8; 6];
    assert_eq!(command(0xFC0C, &[&[1, 2, 3]], &mut small), None);
    assert_eq!(command(0xFC0C, &[&[1], &[2, 3]], &mut small), None);
    assert_eq!(small, [0xAA; 6]);
    let mut tiny = [0xAAu8; 3];
    assert_eq!(command(0x1001, &[], &mut tiny), None);
    assert_eq!(tiny, [0xAA; 3]);
    let mut exact = [0xAAu8; 6];
    assert_eq!(command(0xFC0C, &[&[1, 2]], &mut exact), Some(6));
    assert_eq!(exact, [0x01, 0x0C, 0xFC, 2, 1, 2]);
}

#[test]
fn parameters_past_255_bytes_are_refused() {
    let mut out = [0xAAu8; 400];
    assert_eq!(command(0xFD06, &[&[0x5A; 256]], &mut out), None);
    assert_eq!(command(0xFD06, &[&[0x5A; 200], &[0x5A; 56]], &mut out), None);
    assert!(out.iter().all(|&b| b == 0xAA));
}

#[test]
fn the_longest_command_fills_max_command_exactly() {
    let mut out = [0u8; bluenrg2::MAX_COMMAND];
    assert_eq!(command(0xFD06, &[&[0x5A; 255]], &mut out), Some(out.len()));
    assert_eq!(out[3], 255);
}

#[test]
fn two_events_in_one_read_are_measured_apart() {
    let mut read = vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]);
    let first = read.len();
    read.extend(vendor_event(vendor::ATT_EXCHANGE_MTU_RESP, &[0x01, 0x08, 0xDC, 0x00]));
    assert_eq!(packet_len(&read), Some(first));
    assert_eq!(packet_len(&read[first..]), Some(read.len() - first));
}

#[test]
fn a_packet_cut_short_is_not_whole() {
    let p = vendor_event(vendor::GATT_ATTRIBUTE_MODIFIED, &[0u8; 225]);
    assert!(packet_whole(&p));
    assert!(!packet_whole(&p[..200]));
    assert_eq!(packet_len(&p[..200]), Some(p.len()));
}

#[test]
fn a_header_too_short_to_carry_its_length_gives_none() {
    for p in [&[][..], &[0x04], &[0x04, 0x0E], &[0x02, 0x01, 0x20, 0x02]] {
        assert_eq!(packet_len(p), None, "{p:02X?}");
        assert!(!packet_whole(p), "{p:02X?}");
        assert_eq!(parse(p), Ev::Malformed(p));
    }
    assert_eq!(packet_len(&[0x04, 0x0E, 0x00]), Some(3));
    assert_eq!(packet_len(&[0x02, 0x01, 0x20, 0x00, 0x00]), Some(5));
}

#[test]
fn bytes_that_start_no_packet_give_no_length() {
    // A command packet, which only the host sends, padding, and a stray byte.
    for p in [&[0x01, 0x01, 0x10, 0x00][..], &[0x00, 0x04, 0x05, 0x00], &[0xFF, 0x04, 0x0E, 0x00]] {
        assert_eq!(packet_len(p), None, "{p:02X?}");
        assert!(!packet_whole(p), "{p:02X?}");
        assert_eq!(parse(p), Ev::Malformed(p));
    }
}

#[test]
fn an_acl_packet_is_measured_by_its_16_bit_length() {
    // `02`, handle, then a length of 0x0102: 258 data bytes, then an event behind it.
    let mut read = vec![0x02, 0x01, 0x20, 0x02, 0x01];
    read.extend([0x5A; 258]);
    let acl = read.len();
    read.extend(vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]));
    assert_eq!(packet_len(&read), Some(acl));
    assert!(packet_whole(&read[..acl]));
    assert!(!packet_whole(&read[..acl - 1]));
    assert_eq!(packet_len(&read[..acl - 1]), Some(acl));
    assert_eq!(
        parse(&read[acl..]),
        Ev::Vendor(Vn::GattProcComplete { handle: link(), status: BleStatus::SUCCESS })
    );
}

#[test]
fn acl_data_decodes_with_its_handle_flags_and_data() {
    use bluenrg2::{AclBroadcastFlag, AclPacketBoundary};
    // Handle 0x0801, a first automatically flushable fragment, point to point, then two bytes.
    let p = [0x02, 0x01, 0x28, 0x02, 0x00, 0x5A, 0xA5, 0x04];
    assert_eq!(
        parse(&p),
        Ev::Acl {
            handle: link(),
            boundary: AclPacketBoundary::FirstFlushable,
            broadcast: AclBroadcastFlag::PointToPoint,
            data: &[0x5A, 0xA5]
        }
    );
    let continuing = [0x02, 0x01, 0x18, 0x00, 0x00];
    assert!(matches!(
        parse(&continuing),
        Ev::Acl { boundary: AclPacketBoundary::Continuing, data: [], .. }
    ));
    let complete = [0x02, 0x01, 0x78, 0x00, 0x00];
    assert!(matches!(
        parse(&complete),
        Ev::Acl {
            boundary: AclPacketBoundary::Complete,
            broadcast: AclBroadcastFlag::BrEdrBroadcast,
            ..
        }
    ));
    // A handle above 0x0EFF, whatever the flags.
    assert_eq!(
        parse(&[0x02, 0x00, 0x2F, 0x00, 0x00]),
        Ev::Malformed(&[0x02, 0x00, 0x2F, 0x00, 0x00])
    );
    assert_eq!(parse(&p[..6]), Ev::Malformed(&p[..6]));
}

/// An HCI event packet: `04 code len params`.
fn hci_event(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, code, u8::try_from(params.len()).unwrap()];
    p.extend_from_slice(params);
    p
}

/// An LE meta event packet: `04 3E len sub params`.
fn le_event(sub: u8, params: &[u8]) -> Vec<u8> {
    let mut all = vec![sub];
    all.extend_from_slice(params);
    hci_event(0x3E, &all)
}

/// The packet with its length field cut to `n` parameters, and the bytes after it dropped.
fn shortened(p: &[u8], n: usize) -> Vec<u8> {
    let mut q = p[..3 + n].to_vec();
    q[2] = u8::try_from(n).unwrap();
    q
}

#[test]
fn command_complete_and_status() {
    let p = [0x04, 0x0E, 0x05, 0x01, 0x06, 0xFD, 0x00, 0xAA];
    assert_eq!(
        parse(&p),
        Ev::CommandComplete {
            cmd_opcode: 0xFD06,
            status: Some(BleStatus::SUCCESS),
            return_param_bytes: &[0xAA]
        }
    );
    let p = [0x04, 0x0E, 0x04, 0x01, 0x01, 0x10, 0x00];
    assert_eq!(
        parse(&p),
        Ev::CommandComplete {
            cmd_opcode: 0x1001,
            status: Some(BleStatus::SUCCESS),
            return_param_bytes: &[]
        }
    );
    // No Operation (opcode 0x0000) only grants credits: no status, whatever follows.
    let p = [0x04, 0x0E, 0x03, 0x01, 0x00, 0x00];
    assert_eq!(
        parse(&p),
        Ev::CommandComplete { cmd_opcode: 0x0000, status: None, return_param_bytes: &[] }
    );
    let p = [0x04, 0x0E, 0x04, 0x01, 0x00, 0x00, 0x00];
    assert_eq!(
        parse(&p),
        Ev::CommandComplete { cmd_opcode: 0x0000, status: None, return_param_bytes: &[0x00] }
    );
    // Any other opcode without its status.
    let p = [0x04, 0x0E, 0x03, 0x01, 0x01, 0x10];
    assert_eq!(parse(&p), Ev::Malformed(&p));
    let p = [0x04, 0x0F, 0x04, 0x00, 0x01, 0x13, 0xFD];
    assert_eq!(parse(&p), Ev::CommandStatus { cmd_opcode: 0xFD13, status: BleStatus::SUCCESS });
    // A refusal: Command Disallowed.
    let p = [0x04, 0x0F, 0x04, 0x0C, 0x01, 0x9E, 0xFC];
    assert_eq!(parse(&p), Ev::CommandStatus { cmd_opcode: 0xFC9E, status: BleStatus::new(0x0C) });
    // Without the whole opcode, or a Command Status without its status.
    let short = shortened(&[0x04, 0x0E, 0x05, 0x01, 0x06, 0xFD, 0x00, 0xAA], 2);
    assert_eq!(parse(&short), Ev::Malformed(&short));
    let short = shortened(&[0x04, 0x0F, 0x04, 0x00, 0x01, 0x13, 0xFD], 3);
    assert_eq!(parse(&short), Ev::Malformed(&short));
}

/// LE Connection Complete: status 0, handle 0x0801, role peripheral, public peer
/// 11:22:33:44:55:66 (little-endian), interval 24 (30 ms), latency 0, timeout 5 s, clock accuracy.
fn connection_complete() -> Vec<u8> {
    le_event(
        0x01,
        &[
            0x00, 0x01, 0x08, 0x01, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 24, 0, 0, 0, 0xF4,
            0x01, 0x00,
        ],
    )
}

/// LE Enhanced Connection Complete: the same, with two resolvable addresses before interval 39.
fn enhanced_connection_complete() -> Vec<u8> {
    let mut params = vec![0x00, 0x01, 0x08, 0x01, 0x00];
    params.extend([0x11; 6]);
    params.extend([0; 12]);
    params.extend([39, 0, 0, 0, 0xF4, 0x01, 0x00]);
    le_event(0x0A, &params)
}

#[test]
fn connection_complete_gives_handle_role_peer_interval_latency_and_timeout() {
    let p = connection_complete();
    assert_eq!(p.len(), 3 + 19);
    let peer =
        Address { kind: AddrKind::PUBLIC, addr: BdAddr([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]) };
    assert_eq!(
        parse(&p),
        Ev::LeConnectionComplete {
            handle: link(),
            role: LeConnRole::Peripheral,
            peer,
            timing: Some(timing(24))
        }
    );
    let mut e = enhanced_connection_complete();
    assert_eq!(e.len(), 3 + 31);
    let peer = Address { kind: AddrKind::PUBLIC, addr: BdAddr([0x11; 6]) };
    assert_eq!(
        parse(&e),
        Ev::LeConnectionComplete {
            handle: link(),
            role: LeConnRole::Peripheral,
            peer,
            timing: Some(timing(39))
        }
    );
    // The local device as the central (role 0), to a random address resolved (type 3).
    e[7] = 0x00;
    e[8] = 0x03;
    let Ev::LeConnectionComplete { role, peer, .. } = parse(&e) else { panic!("not connected") };
    assert_eq!(role, LeConnRole::Central);
    assert_eq!(peer.kind, AddrKind::RANDOM);
    // Short of the latency: the handle and peer still, no interval.
    for q in [shortened(&p, 15), shortened(&e, 27)] {
        assert!(
            matches!(parse(&q), Ev::LeConnectionComplete { handle, timing: None, .. } if handle == link())
        );
    }
    // Short of the peer's address: nothing to act on.
    for q in [shortened(&p, 11), shortened(&e, 11), shortened(&p, 1)] {
        assert_eq!(parse(&q), Ev::Malformed(&q));
    }
}

#[test]
fn a_reserved_role_is_malformed_and_a_reserved_clock_accuracy_drops_only_the_timing() {
    for (mut p, accuracy_at) in [(connection_complete(), 21), (enhanced_connection_complete(), 33)]
    {
        let mut role = p.clone();
        role[7] = 0x02;
        assert_eq!(parse(&role), Ev::Malformed(&role));
        p[accuracy_at] = 0x08;
        assert!(matches!(
            parse(&p),
            Ev::LeConnectionComplete { role: LeConnRole::Peripheral, timing: None, .. }
        ));
    }
}

#[test]
fn a_connection_that_never_formed_carries_its_status() {
    // Connection Failed to be Established, and a directed advertising timeout.
    for (sub, status) in [(0x01, 0x3E), (0x0A, 0x3C)] {
        let mut p =
            if sub == 0x01 { connection_complete() } else { enhanced_connection_complete() };
        p[4] = status;
        let status = BleStatus::new(status);
        assert_eq!(parse(&p), Ev::LeConnectionFailed { status });
        // The status is all it needs.
        assert_eq!(parse(&shortened(&p, 2)), Ev::LeConnectionFailed { status });
    }
}

#[test]
fn connection_update_complete_succeeds_or_fails_with_its_handle() {
    // A move to 15 ms (12 units).
    let p = le_event(0x03, &[0x00, 0x01, 0x08, 12, 0, 0, 0, 0xF4, 0x01]);
    assert_eq!(
        parse(&p),
        Ev::LeConnectionUpdateComplete {
            handle: link(),
            conn_interval: Duration::from_micros(15_000),
            peripheral_latency: 0,
            supervision_timeout: Duration::from_millis(5_000)
        }
    );
    // Unacceptable Connection Parameters, Unsupported Remote Feature, LL Response Timeout:
    // the interval and latency fields mean nothing then.
    for status in [0x3B, 0x1A, 0x22] {
        let mut f = p.clone();
        f[4] = status;
        let status = BleStatus::new(status);
        assert_eq!(parse(&f), Ev::LeConnectionUpdateFailed { handle: link(), status });
        assert_eq!(
            parse(&shortened(&f, 4)),
            Ev::LeConnectionUpdateFailed { handle: link(), status }
        );
    }
    // A failure short of its handle, and a success short of its latency.
    let mut f = p.clone();
    f[4] = 0x3B;
    let short = shortened(&f, 3);
    assert_eq!(parse(&short), Ev::Malformed(&short));
    let short = shortened(&p, 6);
    assert_eq!(parse(&short), Ev::Malformed(&short));
}

#[test]
fn data_length_change_carries_both_directions() {
    // 251 octets out in 2120 us, the default 27 in 328 us.
    let p = le_event(0x07, &[0x01, 0x08, 0xFB, 0x00, 0x48, 0x08, 0x1B, 0x00, 0x48, 0x01]);
    assert_eq!(
        parse(&p),
        Ev::LeDataLengthChange {
            handle: link(),
            max_tx_octets: 251,
            max_tx_time: 2120,
            max_rx_octets: 27,
            max_rx_time: 328
        }
    );
    for n in 1..11 {
        let short = shortened(&p, n);
        assert_eq!(parse(&short), Ev::Malformed(&short), "{n}");
    }
}

#[test]
fn disconnection_complete_carries_handle_status_and_reason() {
    assert_eq!(
        parse(&hci_event(0x05, &[0x00, 0x01, 0x08, 0x13])),
        Ev::DisconnectionComplete {
            handle: link(),
            status: BleStatus::SUCCESS,
            reason: BleStatus::new(0x13)
        }
    );
    // Supervision timeout, and MIC failure.
    assert_eq!(
        parse(&hci_event(0x05, &[0x00, 0x02, 0x08, 0x08])),
        Ev::DisconnectionComplete {
            handle: ConnHandle::new(0x0802),
            status: BleStatus::SUCCESS,
            reason: BleStatus::new(0x08)
        }
    );
    assert!(matches!(
        parse(&hci_event(0x05, &[0x00, 0x01, 0x08, 0x3D])),
        Ev::DisconnectionComplete { reason, .. } if reason == BleStatus::new(0x3D)
    ));
    // A disconnection that failed: Command Disallowed, the link still up.
    assert_eq!(
        parse(&hci_event(0x05, &[0x0C, 0x01, 0x08, 0x00])),
        Ev::DisconnectionComplete {
            handle: link(),
            status: BleStatus::new(0x0C),
            reason: BleStatus::new(0)
        }
    );
    let short = hci_event(0x05, &[0x00, 0x01, 0x08]);
    assert_eq!(parse(&short), Ev::Malformed(&short));
}

#[test]
fn encryption_change_carries_handle_status_and_state() {
    assert_eq!(
        parse(&hci_event(0x08, &[0x00, 0x01, 0x08, 0x01])),
        Ev::EncryptionChange {
            handle: link(),
            status: BleStatus::SUCCESS,
            enabled: EncryptionEnabledLevel::OnE0OrAesCcm
        }
    );
    // The central's key was missing on the local device: PIN or Key Missing, encryption off.
    assert_eq!(
        parse(&hci_event(0x08, &[0x06, 0x01, 0x08, 0x00])),
        Ev::EncryptionChange {
            handle: link(),
            status: BleStatus::new(0x06),
            enabled: EncryptionEnabledLevel::Off
        }
    );
    let short = hci_event(0x08, &[0x00, 0x01]);
    assert_eq!(parse(&short), Ev::Malformed(&short));
}

#[test]
fn a_key_refresh_carries_handle_and_status() {
    assert_eq!(
        parse(&hci_event(0x30, &[0x00, 0x01, 0x08])),
        Ev::EncryptionKeyRefresh { handle: link(), status: BleStatus::SUCCESS }
    );
    assert_eq!(
        parse(&hci_event(0x30, &[0x06, 0x01, 0x08])),
        Ev::EncryptionKeyRefresh { handle: link(), status: BleStatus::new(0x06) }
    );
    let short = hci_event(0x30, &[0x00, 0x01]);
    assert_eq!(parse(&short), Ev::Malformed(&short));
}

#[test]
fn a_link_is_encrypted_only_after_a_change_or_refresh_that_succeeded() {
    let encryption = |code, params: &[u8]| bluenrg2::parse(&hci_event(code, params)).encryption();
    assert_eq!(encryption(0x08, &[0x00, 0x01, 0x08, 0x01]), Some((link(), true)));
    assert_eq!(encryption(0x08, &[0x06, 0x01, 0x08, 0x01]), Some((link(), false)));
    assert_eq!(encryption(0x08, &[0x00, 0x01, 0x08, 0x00]), Some((link(), false)));
    assert_eq!(encryption(0x30, &[0x00, 0x01, 0x08]), Some((link(), true)));
    assert_eq!(encryption(0x30, &[0x06, 0x01, 0x08]), Some((link(), false)));
    assert_eq!(encryption(0x05, &[0x00, 0x01, 0x08, 0x13]), None);
}

#[test]
fn events_the_crate_does_not_decode_are_named() {
    // LE: Read Remote Features Complete, PHY Update Complete.
    for sub in [0x04, 0x0C] {
        assert_eq!(
            parse(&le_event(sub, &[0x00, 0x01, 0x08])),
            Ev::LeUnknown { subevent_code: sub }
        );
    }
    // Hardware Error, Number Of Completed Packets, Data Buffer Overflow.
    for code in [0x10, 0x13, 0x1A] {
        assert_eq!(parse(&hci_event(code, &[0x00, 0x01, 0x08])), Ev::Unknown { code });
        assert_eq!(parse(&hci_event(code, &[])), Ev::Unknown { code });
    }
    // A vendor event this crate does not decode, by its code.
    assert_eq!(
        parse(&vendor_event(0x0C13, &[0x01, 0x08])),
        Ev::Vendor(Vn::Unknown { code: 0x0C13, params: &[0x01, 0x08] })
    );
}

#[test]
fn malformed_packets_are_named_not_dropped() {
    for p in [
        &[][..],
        &[0x04],
        &[0x04, 0x05],
        // An LE meta event with no subevent, a vendor event with half a code.
        &[0x04, 0x3E, 0x00],
        &[0x04, 0xFF, 0x01, 0x01],
        // Not an event: a command packet, a stray byte.
        &[0x01, 0x01, 0x10, 0x00],
        &[0xFF, 0x04, 0x05],
    ] {
        assert_eq!(parse(p), Ev::Malformed(p), "{p:02X?}");
    }
    // Its own length past the read: cut short.
    let p = hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]);
    assert_eq!(parse(&p[..6]), Ev::Malformed(&p[..6]));
}

#[test]
fn a_packet_is_read_no_further_than_its_own_length() {
    // Two events in one read: the first only.
    let mut read = hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]);
    read.extend(vendor_event(vendor::BLUE_INITIALIZED, &[0x01]));
    assert_eq!(
        parse(&read),
        Ev::DisconnectionComplete {
            handle: link(),
            status: BleStatus::SUCCESS,
            reason: BleStatus::new(0x13)
        }
    );
    let mut read = vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]);
    read.extend(hci_event(0x05, &[0x00, 0x01, 0x08, 0x13]));
    assert_eq!(
        parse(&read),
        Ev::Vendor(Vn::GattProcComplete { handle: link(), status: BleStatus::SUCCESS })
    );
}

#[test]
fn no_bytes_make_the_decoder_panic() {
    let mut x: u32 = 0x1234_5678;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x.to_le_bytes()[0]
    };
    for _ in 0..20_000 {
        let len = usize::from(next() % 40);
        let mut p: Vec<u8> = (0..len).map(|_| next()).collect();
        // Mostly event packets, with codes the decoder looks at.
        if len > 1 && next() % 4 != 0 {
            p[0] = 0x04;
            p[1] = [0x05, 0x08, 0x0E, 0x0F, 0x3E, 0xFF, 0x10][usize::from(next() % 7)];
        }
        if len > 3 && p[1] == 0x3E {
            p[3] = [0x01, 0x02, 0x03, 0x0A, 0x07][usize::from(next() % 5)];
        }
        let _ = parse(&p);
        for cut in 0..p.len() {
            let _ = parse(&p[..cut]);
        }
    }
}

#[test]
fn a_fixed_pin_goes_little_endian_after_its_flag() {
    use bluenrg2::{
        AuthenticationRequirement, IdentityAddress, KeySizes, Passkey, SecureConnections,
    };
    let r = AuthenticationRequirement {
        bonding: true,
        mitm: false,
        secure_connections: SecureConnections::Optional,
        keypress_notifications: true,
        key_size: KeySizes::new(7, 12).unwrap(),
        fixed_pin: Passkey::new(123_456),
        identity_address: IdentityAddress::Public,
    };
    assert_eq!(r.params(), [0x01, 0x00, 0x01, 0x01, 7, 12, 0x00, 0x40, 0xE2, 0x01, 0x00, 0x00]);
    let asked = AuthenticationRequirement {
        secure_connections: SecureConnections::NotSupported,
        fixed_pin: None,
        identity_address: IdentityAddress::StaticRandom,
        ..r
    };
    assert_eq!(asked.params(), [0x01, 0x00, 0x00, 0x01, 7, 12, 0x01, 0, 0, 0, 0, 0x01]);
    assert_eq!(SecureConnections::Only as u8, 2);
}

#[test]
fn only_values_the_chip_takes_make_an_authentication_requirement() {
    use bluenrg2::{KeySizes, Passkey};
    assert_eq!(Passkey::new(999_999).map(Passkey::get), Some(999_999));
    assert_eq!(Passkey::new(0).map(Passkey::get), Some(0));
    assert_eq!(Passkey::new(1_000_000), None);
    assert_eq!(Passkey::new(4_000_000_000), None);
    let k = KeySizes::new(7, 16).unwrap();
    assert_eq!((k.min(), k.max()), (7, 16));
    assert!(KeySizes::new(16, 16).is_some());
    for (min, max) in [(6, 16), (7, 17), (10, 9), (20, 3), (0, 0)] {
        assert_eq!(KeySizes::new(min, max), None, "{min} {max}");
    }
}

#[test]
fn a_static_random_address_has_its_top_bits_set_and_the_rest_never_all_zero_or_one() {
    use bluenrg2::static_random;
    assert_eq!(static_random(0), BdAddr([0x01, 0, 0, 0, 0, 0xC0]));
    assert_eq!(static_random(u64::MAX), BdAddr([0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]));
    assert_eq!(static_random(0x0000_3FFF_FFFF_FFFF), BdAddr([0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]));
    assert_eq!(static_random(0x1234_5678_9ABC), BdAddr([0xBC, 0x9A, 0x78, 0x56, 0x34, 0xD2]));
}

fn heart_rate_report() -> Vec<u8> {
    let mut ad = vec![0x02, 0x01, 0x06, 0x05, 0x03, 0x0D, 0x18, 0x0F, 0x18];
    ad.extend([0x05, 0x09, b'H', b'1', b'0', b' ']);
    let mut r =
        vec![0x00, 0x01, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xC6, u8::try_from(ad.len()).unwrap()];
    r.extend(ad);
    r.push((-67i8).cast_unsigned());
    r
}

#[test]
fn advertising_reports_decode() {
    let mut p = vec![0x04, 0x3E, 0x00, 0x02, 0x02];
    p.extend(heart_rate_report());
    // A scan response from a public address, carrying only a name.
    p.extend([0x04, 0x00, 1, 2, 3, 4, 5, 6, 4, 3, 0x08, b'P', b'M', 0xB0]);
    p[2] = u8::try_from(p.len() - 3).unwrap();
    let Ev::LeAdvertisingReport { data } = parse(&p) else { panic!("not a report") };
    let all: Vec<_> = reports(data).collect();
    assert_eq!(all.len(), 2);
    let first = all[0];
    assert_eq!(first.kind, LeAdvEventKind::AdvInd);
    assert_eq!(
        first.address,
        Address { kind: AddrKind::RANDOM, addr: BdAddr([0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xC6]) }
    );
    assert_eq!(first.rssi, -67);
    assert_eq!(ad_name(first.data), Some(&b"H10 "[..]));
    assert_eq!(ad_uuid16s(first.data).collect::<Vec<_>>(), [0x180D, 0x180F]);
    assert_eq!(all[1].kind, LeAdvEventKind::ScanRsp);
    assert_eq!(
        all[1].address,
        Address { kind: AddrKind::PUBLIC, addr: BdAddr([1, 2, 3, 4, 5, 6]) }
    );
    assert_eq!(ad_name(all[1].data), Some(&b"PM"[..]));
    assert_eq!(all[1].rssi, -80);
    // A count past what the event holds stops at the last whole report.
    let mut cut = vec![0x03];
    cut.extend(heart_rate_report());
    cut.extend(&heart_rate_report()[..5]);
    assert_eq!(reports(&cut).count(), 1);
}

#[test]
fn advertising_data_with_a_128_bit_uuid_and_junk() {
    // CE060000-43E5-11E4-916C-0800200C9A66, little-endian.
    let uuid = [
        0x66, 0x9A, 0x0C, 0x20, 0x00, 0x08, 0x6C, 0x91, 0xE4, 0x11, 0xE5, 0x43, 0x00, 0x00, 0x06,
        0xCE,
    ];
    let mut ad = vec![17, 0x07];
    ad.extend(uuid);
    assert_eq!(ad_uuid128s(&ad).collect::<Vec<_>>(), [&uuid]);
    // An incomplete list is read; a 16-byte name is not a UUID.
    let mut ad = vec![17, 0x06];
    ad.extend(uuid);
    ad.extend([17, 0x09]);
    ad.extend(*b"a 16-byte name..");
    assert_eq!(ad_uuid128s(&ad).collect::<Vec<_>>(), [&uuid]);
    // A structure longer than the data, and a zero length, end the list without a panic.
    assert_eq!(ad_name(&[0x09, 0x09, b'x']), None);
    assert_eq!(ad_uuid16s(&[0x00, 0x03, 0x0D, 0x18]).count(), 0);
}

#[test]
fn statuses_compare_with_their_constants_and_say_whether_they_succeeded() {
    use bluenrg2::BootReason;
    let p = [0x04, 0x0F, 0x04, 0x0C, 0x01, 0x9E, 0xFC];
    assert!(matches!(parse(&p), Ev::CommandStatus { status: BleStatus::COMMAND_DISALLOWED, .. }));
    assert_eq!(BleStatus::new(0x64), BleStatus::INSUFFICIENT_RESOURCES);
    assert_eq!(BleStatus::REMOTE_USER_TERMINATED.into_inner(), 0x13);
    assert!(BleStatus::SUCCESS.is_success());
    assert!(!BleStatus::new(0x01).is_success());
    assert!(PairingStatus::new(0).is_success());
    for code in 1..=3 {
        assert!(!PairingStatus::new(code).is_success());
    }
    assert_eq!(
        [PairingStatus::TIMEOUT, PairingStatus::FAILED].map(PairingStatus::into_inner),
        [1, 2]
    );
    assert_eq!(BootReason::new(0x05), BootReason::WATCHDOG);
}

#[test]
fn a_status_is_an_error_written_with_its_code() {
    assert_eq!(BleStatus::COMMAND_DISALLOWED.to_string(), "Command Disallowed (0x0c)");
    assert_eq!(BleStatus::new(0x3F).to_string(), format!("{:?} (0x3f)", BleStatus::new(0x3F)));
    assert_eq!(BleStatus::new(0x40).to_string(), "ST status 0x40");
    assert_eq!(BleStatus::INSUFFICIENT_RESOURCES.to_string(), "ST status 0x64");
    let e: &dyn core::error::Error = &BleStatus::new(0x48);
    assert!(e.source().is_none());
}

#[test]
fn an_address_is_written_most_significant_byte_first_without_its_kind() {
    let public =
        Address { kind: AddrKind::PUBLIC, addr: BdAddr([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) };
    assert_eq!(public.to_string(), "0A:1B:2C:3D:4E:5F");
    assert_eq!(Address { kind: AddrKind::RANDOM, ..public }.to_string(), "0A:1B:2C:3D:4E:5F");
}

#[test]
fn bonded_devices_reads_each_entry_and_no_more_than_the_reply_holds() {
    // Two bonds: a static random identity and a public one, as a chip's reply gave them.
    let reply = [2, 1, 0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6, 0, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A];
    let all: Vec<_> = bonded_devices(&reply).collect();
    assert_eq!(
        all,
        vec![
            Address { kind: AddrKind::RANDOM, addr: BdAddr([0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6]) },
            Address { kind: AddrKind::PUBLIC, addr: BdAddr([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) }
        ]
    );
    // A count past the entries present stops at the last whole one.
    assert_eq!(bonded_devices(&[5, 0, 1, 2, 3, 4, 5, 6, 0, 1]).count(), 1);
    assert_eq!(bonded_devices(&[]).count(), 0);
    // A type past random reads as a peer's does.
    for (raw, kind) in [(2, AddrKind::PUBLIC), (3, AddrKind::RANDOM)] {
        let reply = [1, raw, 0xE6, 0xD5, 0xC4, 0xB3, 0xA2, 0xC1];
        assert_eq!(
            bonded_devices(&reply).next(),
            Some(Address { kind, addr: BdAddr([0xE6, 0xD5, 0xC4, 0xB3, 0xA2, 0xC1]) })
        );
    }
}

#[test]
fn the_largest_bonded_devices_reply_fits_the_return_parameters() {
    use bluenrg2::RETURN_PARAMS_MAX;
    let bonds = (RETURN_PARAMS_MAX - 1) / 7;
    let mut p = vec![0x04, 0x0E, 0xFF, 0x01, 0xA3, 0xFC, 0x00, u8::try_from(bonds).unwrap()];
    p.extend((0..bonds).flat_map(|i| [1, u8::try_from(i).unwrap(), 0, 0, 0, 0, 0xC0]));
    p.resize(3 + 0xFF, 0);
    let Ev::CommandComplete { return_param_bytes, .. } = parse(&p) else { panic!() };
    assert_eq!(return_param_bytes.len(), RETURN_PARAMS_MAX);
    assert_eq!(bonded_devices(return_param_bytes).count(), 35);
}

#[test]
fn an_st_status_is_not_named_as_an_hci_error() {
    // ST's 0x48, out of memory, refusing an `ACI_GATT_ADD_SERVICE`.
    let named = [
        format!("{:?}", parse(&[0x04, 0x0E, 0x04, 0x01, 0x02, 0xFD, 0x48])),
        format!("{:?}", parse(&[0x04, 0x0F, 0x04, 0x48, 0x01, 0x02, 0xFD])),
        format!(
            "{:?}",
            bluenrg2::parse(&vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x48]))
        ),
        format!("{:?}", bluenrg2::parse(&vendor_event(vendor::GAP_PROC_COMPLETE, &[0x02, 0x48]))),
        format!("{:?}", parse(&[0x04, 0x05, 0x04, 0x48, 0x01, 0x08, 0x13])),
        format!("{:?}", parse(&[0x04, 0x05, 0x04, 0x00, 0x01, 0x08, 0x48])),
        format!("{:?}", parse(&[0x04, 0x08, 0x04, 0x48, 0x01, 0x08, 0x00])),
        format!(
            "{:?}",
            parse(&[
                0x04, 0x3E, 0x13, 0x01, 0x48, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
            ])
        ),
        format!("{:?}", parse(&[0x04, 0x3E, 0x0A, 0x03, 0x48, 0x01, 0x08, 0, 0, 0, 0, 0, 0])),
    ];
    for text in named {
        assert!(!text.contains("Insufficient Channels"), "{text}");
    }
}

#[test]
fn a_status_below_0x40_is_named_as_hci_names_it() {
    assert_eq!(format!("{:?}", BleStatus::SUCCESS), "Success");
    assert_ne!(format!("{:?}", BleStatus::new(0x3F)), "BleStatus(0x3f)");
    assert_eq!(format!("{:?}", BleStatus::new(0x40)), "BleStatus(0x40)");
    assert_eq!(format!("{:?}", BleStatus::new(0x48)), "BleStatus(0x48)");
    assert_eq!(BleStatus::new(0x48).into_inner(), 0x48);
}

#[test]
fn a_handle_above_0x0eff_is_malformed() {
    let h = [0x00, 0x0F];
    let events: [&[u8]; 6] = [
        &[0x04, 0x05, 0x04, 0x00, h[0], h[1], 0x13],
        &[0x04, 0x08, 0x04, 0x00, h[0], h[1], 0x01],
        &[
            0x04, 0x3E, 0x13, 0x01, 0x00, h[0], h[1], 0x01, 0x00, 1, 2, 3, 4, 5, 6, 24, 0, 0, 0,
            0xF4, 0x01, 0x00,
        ],
        &[0x04, 0x3E, 0x0A, 0x03, 0x00, h[0], h[1], 24, 0, 0, 0, 0xF4, 0x01],
        &[0x04, 0x3E, 0x0A, 0x03, 0x3B, h[0], h[1], 0, 0, 0, 0, 0, 0],
        &[0x04, 0x3E, 0x0B, 0x07, h[0], h[1], 27, 0, 0x48, 0x01, 27, 0, 0x48, 0x01],
    ];
    for p in events {
        assert!(matches!(parse(p), Ev::Malformed(_)), "{p:02x?}: {:?}", parse(p));
    }
    for (code, rest) in [
        (vendor::GAP_NUMERIC_COMPARISON_VALUE, &[1, 2, 3, 4][..]),
        (vendor::L2CAP_CONNECTION_UPDATE_RESP, &[0, 0]),
        (vendor::L2CAP_CONNECTION_UPDATE_REQ, &[1, 10, 0, 6, 0, 12, 0, 0, 0, 0xF4, 0x01]),
        (vendor::GATT_ATTRIBUTE_MODIFIED, &[0x1C, 0, 0, 0, 0, 0]),
        (vendor::GATT_PROC_COMPLETE, &[0]),
    ] {
        let p = vendor_event(code, &[&h[..], rest].concat());
        assert_eq!(parse(&p), Ev::Malformed(&p), "{code:#06x}");
    }
}
