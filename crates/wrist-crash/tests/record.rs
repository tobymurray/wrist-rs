use wrist_crash::{decode, fnv1a, next_count, Build, Decoded, Kind, Record};

const LPTIM: u32 = 0xA49A_F3EA;

fn panic(build: Build) -> Record {
    Record { kind: Kind::Panic { line: 126, file_fnv1a: LPTIM }, build, uptime_s: 750, count: 1 }
}

#[test]
fn records_round_trip() {
    let fault = Record {
        kind: Kind::Fault { cfsr_low20: 0x8200, pc: 0x0806_8251 },
        build: Build::from_id("b7fdeedecd9f-dirty"),
        uptime_s: u32::MAX,
        count: 255,
    };
    for record in [panic(Build::from_id("de244fa480e1")), panic(Build::from_id("gate-test")), fault] {
        assert_eq!(decode(record.encode()), Decoded::Record(record));
    }
}

#[test]
fn the_tag_and_its_word_keep_the_older_layout() {
    let w = panic(Build::from_id("de244fa480e1")).encode();
    assert_eq!(w[4..], [0xD2C0_007E, LPTIM]);
    assert_eq!(w[0], 0xde24_4fa4);
    assert_eq!(w[1] >> 16, 0x80e1);
}

#[test]
fn build_ids_parse_and_print() {
    let mut out = [0; 18];
    assert_eq!(Build::from_id("b7fdeedecd9f"), Build::Hash { id: 0xb7fd_eede_cd9f, dirty: false });
    assert_eq!(Build::from_id("b7fdeedecd9f").text(&mut out), "b7fdeedecd9f");
    assert_eq!(Build::from_id("00fdeedecd9f-dirty").text(&mut out), "00fdeedecd9f-dirty");
    for named in ["gate-test", "unknown", "b7fdeedecd9", "b7fdeedecd9fx", "b7fdeedecd9f-dirtyx", ""] {
        assert_eq!(Build::from_id(named), Build::Named(fnv1a(named.as_bytes())), "{named}");
    }
    assert_eq!(Build::Named(0x0123_abcd).text(&mut out), "#0123abcd");
}

#[test]
fn an_older_images_record_is_old_whatever_lies_below_it() {
    let old = Decoded::Old(Kind::Panic { line: 126, file_fnv1a: LPTIM });
    for below in [[0; 4], [!0; 4], [0x2026_FFE0, 0x0806_8251, 0, 0xDEAD_BEEF]] {
        let w = [below[0], below[1], below[2], below[3], 0xD2C0_007E, LPTIM];
        assert_eq!(decode(w), old);
    }
    let fault = [0, 0, 0, 0, 0xD2F0_8200, 0x0806_8251];
    assert_eq!(decode(fault), Decoded::Old(Kind::Fault { cfsr_low20: 0x8200, pc: 0x0806_8251 }));
}

#[test]
fn any_changed_word_makes_a_record_old() {
    let w = panic(Build::from_id("de244fa480e1")).encode();
    for i in [0, 1, 2, 3, 5] {
        let mut bad = w;
        bad[i] ^= 1 << 7;
        assert!(matches!(decode(bad), Decoded::Old(_)), "word {i}");
    }
}

#[test]
fn blank_or_untagged_words_are_no_record() {
    for w in [[0; 6], [!0; 6], [1, 2, 3, 4, 0x1234_5678, 6], [0, 0, 0, 0, 0xD2D0_0001, 0]] {
        assert_eq!(decode(w), Decoded::None);
    }
}

#[test]
fn the_count_follows_what_was_there() {
    assert_eq!(next_count(decode([0; 6])), 1);
    assert_eq!(next_count(decode([0, 0, 0, 0, 0xD2C0_007E, LPTIM])), 2);
    let mut r = panic(Build::from_id("de244fa480e1"));
    r.count = 3;
    assert_eq!(next_count(decode(r.encode())), 4);
    r.count = 255;
    assert_eq!(next_count(decode(r.encode())), 255);
}
