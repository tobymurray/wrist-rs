use wrist_time::backup::{Flags, OWNED, STOCK};

#[test]
fn every_register_written_is_one_stock_never_touches() {
    for n in OWNED {
        assert!(n < 32 && !STOCK.contains(&n), "BKP{n}R");
    }
    assert!(STOCK.contains(&5));
    let mut owned = OWNED.to_vec();
    owned.sort_unstable();
    owned.dedup();
    assert_eq!(owned.len(), OWNED.len());
}

#[test]
fn flags_read_only_a_tagged_word() {
    for f in [Flags::default(), Flags { radio: true, source: 2 }, Flags { radio: false, source: 1 }] {
        assert_eq!(Flags::decode(f.encode()), Some(f));
    }
    let on = Flags { radio: true, source: 1 }.encode();
    assert_eq!(Flags::decode(on ^ 1 << 20), None);
    assert_eq!(Flags::decode(0), None);
    assert_eq!(Flags::decode(0x424C_0001), None);
}
