use wrist_settings::{crc32, from_hex, to_hex, Device, Key, Problem, Settings, Value, PAYLOAD, VALUE_TEXT, WORDS};

const H10: Device = Device { random: false, address: [0x5C, 0xFC, 0xD7, 0x03, 0x12, 0xAC] };

fn everything() -> Settings {
    Settings { strap: Some(H10), relay: true, multi: true }
}

fn text(v: Value) -> String {
    let mut out = [0u8; VALUE_TEXT];
    let n = v.write(&mut out);
    String::from_utf8(out[..n].to_vec()).unwrap()
}

#[test]
fn crc_is_zlibs() {
    // zlib.crc32(b"12345678"), the bytes of two little-endian words.
    assert_eq!(crc32(&[0x3433_3231, 0x3837_3635]), 0x9AE0_DAAF);
    assert_eq!(crc32(&[]), 0);
}

#[test]
fn defaults_are_off() {
    let s = Settings::default();
    assert_eq!((s.strap, s.relay, s.multi), (None, false, false));
}

#[test]
fn round_trip_through_words() {
    for s in [Settings::default(), everything(), Settings { relay: true, ..Settings::default() }] {
        assert_eq!(Settings::decode(&s.encode()), Ok(s));
    }
}

#[test]
fn cleared_words_are_absent() {
    assert_eq!(Settings::decode(&[0; WORDS]), Err(Problem::Absent));
}

#[test]
fn any_flipped_bit_is_corrupt() {
    let good = everything().encode();
    for word in 0..WORDS {
        for bit in 0..32 {
            let mut w = good;
            w[word] ^= 1 << bit;
            let r = Settings::decode(&w);
            assert!(r.is_err(), "word {word} bit {bit} read as {r:?}");
        }
    }
}

#[test]
fn a_later_version_is_not_read() {
    let mut w = everything().encode();
    w[0] = (w[0] & !0xFF00) | 2 << 8;
    w[WORDS - 1] = crc32(&w[..WORDS - 1]);
    assert_eq!(Settings::decode(&w), Err(Problem::Newer(2)));
}

#[test]
fn unknown_keys_are_skipped() {
    // A later setting (key 9, two bytes) between two this version knows.
    let s = Settings::from_bytes(&[2, 1, 1, 9, 2, 0xAA, 0xBB, 3, 1, 1]).unwrap();
    assert_eq!(s, Settings { strap: None, relay: true, multi: true });
}

#[test]
fn malformed_payloads_are_refused() {
    for bad in [
        &[2, 2, 1, 1][..],                 // a switch two bytes long
        &[2, 1, 7],                        // a switch neither on nor off
        &[1, 6, 0, 1, 2, 3, 4, 5],         // an address without its kind
        &[1, 7, 2, 1, 2, 3, 4, 5, 6],      // a kind neither public nor random
        &[9, 4, 0, 0],                     // runs past the end
        &[2, 1, 1, 3],                     // a dangling byte
    ] {
        assert_eq!(Settings::from_bytes(bad), None, "{bad:?}");
    }
    assert_eq!(Settings::from_bytes(&[0; PAYLOAD + 1]), None);
}

#[test]
fn only_changed_settings_are_written() {
    let mut out = [0u8; PAYLOAD];
    assert_eq!(Settings::default().to_bytes(&mut out), 0);
    assert_eq!(everything().to_bytes(&mut out), 9 + 3 + 3);
    assert_eq!(&out[..9], &[1, 7, 0, 0x5C, 0xFC, 0xD7, 0x03, 0x12, 0xAC]);
}

#[test]
fn hex_round_trip() {
    let mut payload = [0u8; PAYLOAD];
    let n = everything().to_bytes(&mut payload);
    let mut digits = [0u8; 2 * PAYLOAD];
    let d = to_hex(&payload[..n], &mut digits);
    let hex = core::str::from_utf8(&digits[..d]).unwrap();
    assert_eq!(hex, "0107005CFCD70312AC020101030101");
    assert_eq!(from_hex(hex), Some(everything()));
    assert_eq!(from_hex(&hex.to_lowercase()), Some(everything()));
    assert_eq!(from_hex(""), Some(Settings::default()));
    for bad in ["0", "zz", "+1", "0201"] {
        assert_eq!(from_hex(bad), None, "{bad}");
    }
}

#[test]
fn names_and_values() {
    for k in Key::ALL {
        assert_eq!(Key::from_name(k.name()), Some(k));
    }
    assert_eq!(Key::from_name("hr"), None);
    assert_eq!(Key::Relay.parse("on"), Some(Value::Switch(true)));
    assert_eq!(Key::Multi.parse(" off "), Some(Value::Switch(false)));
    assert_eq!(Key::Relay.parse("yes"), None);
    assert_eq!(Key::Relay.parse("on now"), None);
    assert_eq!(Key::Strap.parse("none"), Some(Value::Device(None)));
    assert_eq!(Key::Strap.parse("AC:12:03:D7:FC:5C"), Some(Value::Device(Some(H10))));
    let random = Device { random: true, ..H10 };
    assert_eq!(Key::Strap.parse("ac:12:03:d7:fc:5c random"), Some(Value::Device(Some(random))));
    for bad in ["AC:12:03:D7:FC", "AC:12:03:D7:FC:5C:00", "AC:12:03:D7:FC:+5", "AC:12:03:D7:FC:5C other", ""] {
        assert_eq!(Key::Strap.parse(bad), None, "{bad}");
    }
    assert_eq!(text(Value::Device(Some(H10))), "AC:12:03:D7:FC:5C");
    assert_eq!(text(Value::Device(Some(random))), "AC:12:03:D7:FC:5C random");
    assert_eq!(text(Value::Device(None)), "none");
    assert_eq!(text(Value::Switch(true)), "on");
}

#[test]
fn set_checks_the_kind() {
    let mut s = Settings::default();
    assert!(!s.set(Key::Relay, Value::Device(None)));
    assert!(s.set(Key::Strap, Value::Device(Some(H10))));
    assert_eq!(s.get(Key::Strap), Value::Device(Some(H10)));
}
