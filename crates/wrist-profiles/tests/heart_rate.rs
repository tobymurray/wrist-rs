//! The Heart Rate Measurement against the HRS 1.0 field layout.

use wrist_profiles::heart_rate::{rr_ms, Contact, Measurement, DEFAULT_PAYLOAD, MAX_LEN, MAX_RR};

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
        let (n, rr) = m.encode(&mut out);
        assert_eq!(&out[..n], b);
        assert_eq!(rr, m.rr_len);
    }
    // A rate alone: flags 0, one byte.
    let (n, _) = Measurement::rate(61).encode(&mut out);
    assert_eq!(&out[..n], [0x00, 61]);
    // Too small a buffer writes nothing.
    assert_eq!(Measurement::rate(300).encode(&mut [0u8; 2]), (0, 0));
}

fn full(bpm: u16, energy_kj: Option<u16>) -> Measurement {
    let mut m = Measurement { energy_kj, ..Measurement::rate(bpm) };
    for (i, slot) in m.rr.iter_mut().enumerate() {
        *slot = 400 + u16::try_from(i).unwrap();
    }
    m.rr_len = MAX_RR;
    m
}

#[test]
fn nine_rr_fit_a_default_notification() {
    let mut out = [0u8; DEFAULT_PAYLOAD];
    assert_eq!(full(150, None).encode(&mut out), (20, MAX_RR));
}

#[test]
fn rr_that_do_not_fit_wait_for_the_next_notification() {
    let m = full(300, Some(1234));
    let mut out = [0u8; MAX_LEN];
    let (n, rr) = m.encode(&mut out[..DEFAULT_PAYLOAD]);
    assert_eq!((n, rr), (19, 7));
    let sent = Measurement::decode(&out[..n]).unwrap();
    assert_eq!((sent.bpm, sent.energy_kj, sent.rr()), (300, Some(1234), &m.rr()[..7]));

    let mut rest = Measurement::rate(300);
    rest.rr[..MAX_RR - rr].copy_from_slice(&m.rr()[rr..]);
    rest.rr_len = MAX_RR - rr;
    let (n, _) = rest.encode(&mut out[..DEFAULT_PAYLOAD]);
    assert_eq!(Measurement::decode(&out[..n]).unwrap().rr(), [407, 408]);

    // With room, all nine.
    assert_eq!(m.encode(&mut out), (MAX_LEN, MAX_RR));
}
