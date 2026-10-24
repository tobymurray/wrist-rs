//! The Heart Rate Measurement against the HRS 1.0 field layout, and the shapes a Polar H10
//! sends (an 8-bit rate, contact, RR intervals).

use wrist_profiles::heart_rate::{rr_ms, Contact, Measurement, MAX_LEN, MAX_RR};

#[test]
fn an_8_bit_rate_with_contact_and_rr() {
    // Flags 0x16: contact supported and detected, RR present; 72 bpm; RR 812/1024 s.
    let m = Measurement::decode(&[0x16, 72, 0x2C, 0x03]).unwrap();
    assert_eq!(m.bpm, 72);
    assert_eq!(m.contact, Contact::Detected);
    assert_eq!(m.energy_kj, None);
    assert_eq!(m.rr(), [812]);
    assert_eq!(rr_ms(812), 793);
}

#[test]
fn a_16_bit_rate_with_energy_and_two_rr() {
    // Flags 0x19: 16-bit rate, energy, RR; 300 bpm; 1234 kJ; RR 200 and 205.
    let b = [0x19, 0x2C, 0x01, 0xD2, 0x04, 0xC8, 0x00, 0xCD, 0x00];
    let m = Measurement::decode(&b).unwrap();
    assert_eq!((m.bpm, m.energy_kj, m.rr()), (300, Some(1234), &[200, 205][..]));
    assert_eq!(m.contact, Contact::Unsupported);
}

#[test]
fn contact_lost_is_told_apart() {
    assert_eq!(Measurement::decode(&[0x04, 0]).unwrap().contact, Contact::Lost);
}

#[test]
fn too_short_for_its_flags_is_refused() {
    assert_eq!(Measurement::decode(&[]), None);
    assert_eq!(Measurement::decode(&[0x00]), None);
    assert_eq!(Measurement::decode(&[0x01, 0x48]), None);
    assert_eq!(Measurement::decode(&[0x08, 0x48, 0x01]), None);
    // RR flagged and absent is no RR, and an odd trailing byte is dropped.
    assert_eq!(Measurement::decode(&[0x10, 60]).unwrap().rr_len, 0);
    assert_eq!(Measurement::decode(&[0x10, 60, 1, 2, 3]).unwrap().rr(), [0x0201]);
}

#[test]
fn rr_past_the_limit_is_dropped() {
    let mut b = vec![0x10, 60];
    for i in 0..12u16 {
        b.extend((1000 + i).to_le_bytes());
    }
    let m = Measurement::decode(&b).unwrap();
    assert_eq!(m.rr_len, MAX_RR);
    assert_eq!(m.rr()[MAX_RR - 1], 1008);
}

#[test]
fn encode_round_trips() {
    let mut out = [0u8; MAX_LEN];
    for b in [&[0x16, 72, 0x2C, 0x03][..], &[0x19, 0x2C, 0x01, 0xD2, 0x04, 0xC8, 0x00, 0xCD, 0x00], &[0x04, 55]] {
        let m = Measurement::decode(b).unwrap();
        let n = m.encode(&mut out);
        assert_eq!(&out[..n], b);
    }
    // A rate alone: flags 0, one byte.
    let n = Measurement::rate(61).encode(&mut out);
    assert_eq!(&out[..n], [0x00, 61]);
    // Too small a buffer writes nothing.
    assert_eq!(Measurement::rate(61).encode(&mut [0u8; 4]), 0);
}

#[test]
fn nine_rr_fit_a_default_notification() {
    let mut m = Measurement::rate(150);
    m.rr = [400; MAX_RR];
    m.rr_len = MAX_RR;
    let mut out = [0u8; MAX_LEN];
    assert_eq!(m.encode(&mut out), 20);
}
