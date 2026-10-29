//! Packets as the BlueNRG-2 sends them, in the shapes installs 40 to 51 met:
//! the 217-byte console write, two events in one read, a read cut short.

use wrist_aci::{
    ad_name, ad_uuid128s, ad_uuid16s, att_mtu, characteristics, command, descriptors, gap_proc_complete, gatt_error,
    header_sizes, notification, numeric_comparison, packet_len, packet_whole, pairing_complete, parse, read_value,
    reports, service_range, services, vendor, Address, AttributeModified, Characteristic, Event, L2capUpdateRequest,
    Service, Uuid,
};

/// A vendor event packet: `04 FF len code params`.
fn vendor_event(code: u16, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, 0xFF, u8::try_from(2 + params.len()).unwrap()];
    p.extend_from_slice(&code.to_le_bytes());
    p.extend_from_slice(params);
    p
}

#[test]
fn header_sizes_read_both_fields() {
    // Install 40: a write header answered with 265 bytes of room.
    assert_eq!(header_sizes(&[0x00, 0x09, 0x01, 0x00, 0x00]), (265, 0));
    assert_eq!(header_sizes(&[0x00, 0x09, 0x01, 0xE6, 0x00]), (265, 230));
}

#[test]
fn command_packets_carry_opcode_and_length() {
    let mut out = [0u8; 16];
    let n = command(0xFD81, &[0x01, 0x08, 12, 0, 24, 0, 0, 0, 0xF4, 0x01], &mut out);
    assert_eq!(&out[..n], &[0x01, 0x81, 0xFD, 10, 0x01, 0x08, 12, 0, 24, 0, 0, 0, 0xF4, 0x01]);
    let n = command(0x1001, &[], &mut out);
    assert_eq!(&out[..n], &[0x01, 0x01, 0x10, 0]);
    // Cut to the buffer, never past it.
    let mut small = [0u8; 6];
    assert_eq!(command(0xFC0C, &[1, 2, 3, 4, 5], &mut small), 6);
    assert_eq!(small, [0x01, 0x0C, 0xFC, 2, 1, 2]);
}

#[test]
fn parameters_past_255_bytes_are_cut_and_the_length_says_so() {
    let params: Vec<u8> = (0..300u16).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let mut out = [0u8; 400];
    assert_eq!(command(0xFD06, &params, &mut out), 4 + 255);
    assert_eq!(out[3], 255);
    assert_eq!(&out[4..259], &params[..255]);
    assert!(out[259..].iter().all(|&b| b == 0));
}

#[test]
fn a_buffer_too_small_for_the_header_gets_nothing() {
    let mut tiny = [0xAAu8; 3];
    assert_eq!(command(0xFC0C, &[1, 2], &mut tiny), 0);
    assert_eq!(tiny, [0xAA; 3]);
}

#[test]
fn two_events_in_one_read_are_measured_apart() {
    let mut read = vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]);
    let first = read.len();
    read.extend(vendor_event(vendor::ATT_EXCHANGE_MTU_RESP, &[0x01, 0x08, 0xDC, 0x00]));
    assert_eq!(packet_len(&read), first);
    assert_eq!(packet_len(&read[first..]), read.len() - first);
}

#[test]
fn a_packet_cut_short_is_not_whole() {
    let p = vendor_event(vendor::GATT_ATTRIBUTE_MODIFIED, &[0u8; 225]);
    assert!(packet_whole(&p));
    assert!(!packet_whole(&p[..200]));
    assert_eq!(packet_len(&p[..200]), 200);
}

#[test]
fn an_acl_packet_is_measured_by_its_16_bit_length() {
    // `02`, handle, then a length of 0x0102: 258 data bytes, then an event behind it.
    let mut read = vec![0x02, 0x01, 0x20, 0x02, 0x01];
    read.extend([0x5A; 258]);
    let acl = read.len();
    read.extend(vendor_event(vendor::GATT_PROC_COMPLETE, &[0x01, 0x08, 0x00]));
    assert_eq!(packet_len(&read), acl);
    assert!(packet_whole(&read[..acl]));
    assert!(!packet_whole(&read[..acl - 1]));
    assert_eq!(packet_len(&read[..acl - 1]), acl - 1);
    assert_eq!(parse(&read[acl..]), Some(Event::Vendor { code: vendor::GATT_PROC_COMPLETE, params: &[0x01, 0x08, 0x00] }));
}

#[test]
fn a_217_byte_console_write_decodes() {
    // Connection 0x0801, attribute 0x0019 (NUS RX value), offset 0, 217 bytes.
    let mut params = vec![0x01, 0x08, 0x19, 0x00, 0x00, 0x00, 217, 0x00];
    params.extend((0..217u8).map(|i| i.wrapping_mul(7)));
    let p = vendor_event(vendor::GATT_ATTRIBUTE_MODIFIED, &params);
    assert_eq!(p.len(), 230); // two of these overflow the old 260-byte buffer
    let Some(Event::Vendor { code, params }) = parse(&p) else { panic!("not vendor") };
    assert_eq!(code, vendor::GATT_ATTRIBUTE_MODIFIED);
    let w = AttributeModified::decode(params).unwrap();
    assert_eq!((w.attr, w.offset, w.data.len()), (0x19, 0, 217));
    assert_eq!(w.data[1], 7);
}

#[test]
fn an_attribute_write_longer_than_its_event_is_cut() {
    let params = [0x01, 0x08, 0x1C, 0x00, 0x00, 0x00, 0x09, 0x00, 0x01, 0x00];
    let w = AttributeModified::decode(&params).unwrap();
    assert_eq!(w.data, &[0x01, 0x00]);
    assert!(AttributeModified::decode(&params[..7]).is_none());
}

#[test]
fn command_complete_and_status() {
    let p = [0x04, 0x0E, 0x05, 0x01, 0x06, 0xFD, 0x00, 0xAA];
    assert_eq!(parse(&p), Some(Event::CommandComplete { opcode: 0xFD06, status: 0, params: &[0xAA] }));
    let p = [0x04, 0x0F, 0x04, 0x00, 0x01, 0x13, 0xFD];
    assert_eq!(parse(&p), Some(Event::CommandStatus { opcode: 0xFD13, status: 0 }));
}

#[test]
fn connection_complete_gives_handle_interval_and_latency() {
    // Status, handle 0x0801, role, peer type, address, interval 24 (30 ms),
    // latency 0, timeout, clock accuracy.
    let mut p = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00];
    p.extend([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    p.extend([24, 0, 0, 0, 0xF4, 0x01, 0x00]);
    assert_eq!(parse(&p), Some(Event::Connected { handle: 0x0801, interval: Some((24, 0)) }));
    // Enhanced: two resolvable addresses before the interval.
    let mut e = vec![0x04, 0x3E, 0x1F, 0x0A, 0x00, 0x01, 0x08, 0x01, 0x00];
    e.extend([0x11; 6]);
    e.extend([0; 12]);
    e.extend([39, 0, 0, 0, 0xF4, 0x01, 0x00]);
    assert_eq!(parse(&e), Some(Event::Connected { handle: 0x0801, interval: Some((39, 0)) }));
    // A failed connection is not one.
    let mut f = p.clone();
    f[4] = 0x3E;
    assert_eq!(parse(&f), None);
    // Cut before the latency ends: the handle still, no interval.
    assert_eq!(parse(&p[..18]), Some(Event::Connected { handle: 0x0801, interval: None }));
    assert_eq!(parse(&e[..30]), Some(Event::Connected { handle: 0x0801, interval: None }));
}

#[test]
fn connection_update_disconnection_and_encryption() {
    // Install 48: BlueZ moved to 15 ms (12 units) on request.
    let p = [0x04, 0x3E, 0x0A, 0x03, 0x00, 0x01, 0x08, 12, 0, 0, 0, 0xF4, 0x01];
    assert_eq!(parse(&p), Some(Event::ConnectionUpdate { interval: 12, latency: 0 }));
    assert_eq!(parse(&[0x04, 0x05, 0x04, 0x00, 0x01, 0x08, 0x13]), Some(Event::Disconnected { reason: 0x13 }));
    assert_eq!(parse(&[0x04, 0x08, 0x04, 0x00, 0x01, 0x08, 0x01]), Some(Event::Encryption { on: 1 }));
}

#[test]
fn gap_events_decode() {
    // Pairing: connection 0x0801, number 123456.
    let mut params = vec![0x01, 0x08];
    params.extend(123_456u32.to_le_bytes());
    assert_eq!(numeric_comparison(&params), Some((0x0801, 123_456)));
    assert_eq!(pairing_complete(&[0x01, 0x08, 0x00, 0x00]), Some((0, 0)));
    assert_eq!(pairing_complete(&[0x01, 0x08, 0x02]), None);
}

#[test]
fn the_mtu_exchange_decodes() {
    assert_eq!(att_mtu(&[0x01, 0x08, 0xDC, 0x00]), Some(220));
    assert_eq!(att_mtu(&[0x01, 0x08, 0xDC]), None);
}

#[test]
fn pairing_is_secure_connections_only_with_mitm_and_16_byte_keys() {
    let p = wrist_aci::PAIRING.params();
    // Bonding, MITM, SC only, no keypress, key 16 to 16, no fixed pin (the host is asked), pin 0,
    // static random identity.
    assert_eq!(p, [0x01, 0x01, 0x02, 0x00, 0x10, 0x10, 0x01, 0, 0, 0, 0, 0x01]);
}

#[test]
fn a_fixed_pin_goes_little_endian_after_its_flag() {
    let r = wrist_aci::AuthenticationRequirement { fixed_pin: Some(123_456), ..wrist_aci::PAIRING };
    assert_eq!(r.params()[6..11], [0x00, 0x40, 0xE2, 0x01, 0x00]);
}

#[test]
fn the_address_is_static_random_and_never_changes() {
    use wrist_aci::{static_random, static_random_address};
    // Pinned: a different address would orphan every bond.
    let uid = [0x0034_0021, 0x3133_5111, 0x3039_3533];
    assert_eq!(static_random_address(uid), static_random_address(uid));
    assert_eq!(static_random_address(uid), [0x2B, 0x1E, 0xCE, 0x05, 0x69, 0xFF]);
    for x in 0..1_000u32 {
        let a = static_random_address([x, x.rotate_left(7) ^ 0x5A5A, !x]);
        assert_eq!(a[5] & 0xC0, 0xC0, "{a:02X?}");
    }
    // The other 46 bits are never all zero or all one.
    assert_eq!(static_random(0), [0x01, 0, 0, 0, 0, 0xC0]);
    assert_eq!(static_random(u64::MAX), [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(static_random(0x0000_3FFF_FFFF_FFFF), [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn neighbouring_dies_get_different_addresses() {
    let mut seen = std::collections::HashSet::new();
    for x in 0..64u32 {
        for y in 0..64u32 {
            assert!(seen.insert(wrist_aci::static_random_address([(x << 16) | y, 0x3133_5111, 0x3039_3533])));
        }
    }
}

fn strap_report() -> Vec<u8> {
    let mut ad = vec![0x02, 0x01, 0x06, 0x05, 0x03, 0x0D, 0x18, 0x0F, 0x18];
    ad.extend([0x05, 0x09, b'H', b'1', b'0', b' ']);
    let mut r = vec![0x00, 0x01, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xC6, u8::try_from(ad.len()).unwrap()];
    r.extend(ad);
    r.push((-67i8).cast_unsigned());
    r
}

#[test]
fn advertising_reports_decode() {
    let mut p = vec![0x04, 0x3E, 0x00, 0x02, 0x02];
    p.extend(strap_report());
    // A scan response from a public address, carrying only a name.
    p.extend([0x04, 0x00, 1, 2, 3, 4, 5, 6, 4, 3, 0x08, b'P', b'M', 0xB0]);
    p[2] = u8::try_from(p.len() - 3).unwrap();
    let Some(Event::Advertising { data }) = parse(&p) else { panic!("not a report") };
    let all: Vec<_> = reports(data).collect();
    assert_eq!(all.len(), 2);
    let strap = all[0];
    assert_eq!(strap.kind, 0);
    assert_eq!(strap.address, Address { kind: 1, bytes: [0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xC6] });
    assert_eq!(strap.rssi, -67);
    assert_eq!(ad_name(strap.data), Some(&b"H10 "[..]));
    assert_eq!(ad_uuid16s(strap.data).collect::<Vec<_>>(), [0x180D, 0x180F]);
    assert_eq!(all[1].kind, 4);
    assert_eq!(ad_name(all[1].data), Some(&b"PM"[..]));
    assert_eq!(all[1].rssi, -80);
    // A count past what the event holds stops at the last whole report.
    let mut cut = vec![0x03];
    cut.extend(strap_report());
    cut.extend(&strap_report()[..5]);
    assert_eq!(reports(&cut).count(), 1);
}

#[test]
fn advertising_data_with_a_128_bit_uuid_and_junk() {
    // CE060000-43E5-11E4-916C-0800200C9A66, little-endian.
    let pm5 = [0x66, 0x9A, 0x0C, 0x20, 0x00, 0x08, 0x6C, 0x91, 0xE4, 0x11, 0xE5, 0x43, 0x00, 0x00, 0x06, 0xCE];
    let mut ad = vec![17, 0x07];
    ad.extend(pm5);
    assert_eq!(ad_uuid128s(&ad).collect::<Vec<_>>(), [&pm5]);
    // A structure longer than the data, and a zero length, end the list without a panic.
    assert_eq!(ad_name(&[0x09, 0x09, b'x']), None);
    assert_eq!(ad_uuid16s(&[0x00, 0x03, 0x0D, 0x18]).count(), 0);
}

#[test]
fn primary_services_decode() {
    // Connection 0x0801, elements of 6: (1, 5, 0x1800), (6, 9, 0x180D).
    let p = [0x01, 0x08, 6, 12, 1, 0, 5, 0, 0x00, 0x18, 6, 0, 9, 0, 0x0D, 0x18];
    let all: Vec<_> = services(&p).collect();
    assert_eq!(
        all,
        [Service { start: 1, end: 5, uuid: Uuid::U16(0x1800) }, Service { start: 6, end: 9, uuid: Uuid::U16(0x180D) }]
    );
    // A 128-bit service, element length 20.
    let mut q = vec![0x01, 0x08, 20, 20, 0x10, 0, 0x20, 0];
    q.extend([0xAB; 16]);
    assert_eq!(services(&q).next(), Some(Service { start: 0x10, end: 0x20, uuid: Uuid::U128([0xAB; 16]) }));
    // An element length that fits no UUID yields nothing.
    assert_eq!(services(&[0x01, 0x08, 5, 5, 1, 0, 2, 0, 3]).count(), 0);
}

#[test]
fn characteristics_decode() {
    // Heart Rate Measurement: declaration 7, notify, value 8; Body Sensor Location 10, read, 11.
    let p = [0x01, 0x08, 7, 14, 7, 0, 0x10, 8, 0, 0x37, 0x2A, 10, 0, 0x02, 11, 0, 0x38, 0x2A];
    let all: Vec<_> = characteristics(&p).collect();
    assert_eq!(all[0], Characteristic { declaration: 7, properties: 0x10, value: 8, uuid: Uuid::U16(0x2A37) });
    assert_eq!(all[1].uuid, Uuid::U16(0x2A38));
    // A data length past the event is cut to it: one whole element.
    let cut = [0x01, 0x08, 7, 14, 7, 0, 0x10, 8, 0, 0x37, 0x2A, 10, 0];
    assert_eq!(characteristics(&cut).count(), 1);
}

#[test]
fn descriptors_service_range_and_values_decode() {
    // Format 1: (9, 0x2902).
    assert_eq!(descriptors(&[0x01, 0x08, 1, 4, 9, 0, 0x02, 0x29]).collect::<Vec<_>>(), [(9, Uuid::U16(0x2902))]);
    assert_eq!(descriptors(&[0x01, 0x08, 3, 4, 9, 0, 0x02, 0x29]).count(), 0);
    assert_eq!(service_range(&[0x01, 0x08, 1, 6, 0, 9, 0]), Some((6, 9)));
    assert_eq!(read_value(&[0x01, 0x08, 2, 0x01, 0x02]), Some(&[0x01, 0x02][..]));
    // Notification: connection, attribute 8, three bytes.
    assert_eq!(notification(&[0x01, 0x08, 8, 0, 3, 0x16, 72, 0x2C]), Some((8, &[0x16, 72, 0x2C][..])));
    assert_eq!(notification(&[0x01, 0x08, 8, 0, 9, 0x16]), Some((8, &[0x16][..])));
    assert_eq!(notification(&[0x01, 0x08, 8]), None);
    assert_eq!(gatt_error(&[0x01, 0x08, 0x0A, 0x0B, 0x00, 0x05]), Some((0x0A, 0x0B, 0x05)));
    assert_eq!(gap_proc_complete(&[0x02, 0x00, 0x00]), Some((0x02, 0)));
}

#[test]
fn a_peripherals_parameter_request_decodes() {
    // Connection 0x0801, identifier 7, L2CAP length 8, 400 to 800 ms, latency 0, 6 s.
    let p = [0x01, 0x08, 7, 8, 0, 0x40, 0x01, 0x80, 0x02, 0, 0, 0x58, 0x02];
    assert_eq!(
        L2capUpdateRequest::decode(&p),
        Some(L2capUpdateRequest { handle: 0x0801, identifier: 7, interval_min: 320, interval_max: 640, latency: 0, timeout: 600 })
    );
    assert_eq!(L2capUpdateRequest::decode(&p[..12]), None);
}
