//! Packets as the BlueNRG-2 sends them, in the shapes installs 40 to 51 met:
//! the 217-byte console write, two events in one read, a read cut short.

use wrist_aci::{
    att_mtu, command, header_sizes, numeric_comparison, packet_len, packet_whole, pairing_complete, parse, vendor,
    AttributeModified, Event,
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
    // public identity.
    assert_eq!(p, [0x01, 0x01, 0x02, 0x00, 0x10, 0x10, 0x01, 0, 0, 0, 0, 0x00]);
}

#[test]
fn a_fixed_pin_goes_little_endian_after_its_flag() {
    let r = wrist_aci::AuthenticationRequirement { fixed_pin: Some(123_456), ..wrist_aci::PAIRING };
    assert_eq!(r.params()[6..11], [0x00, 0x40, 0xE2, 0x01, 0x00]);
}
