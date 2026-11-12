use std::cell::RefCell;

use wrist_time::alarm::Alarm;
use wrist_time::backup::{migrate, old, Flags, ALARM, FLAGS, LAST_SET_AT, OWNED, STOCK, ZONE};
use wrist_time::zone::Zone;

/// A 16-bit value over stock's magic (`HARDWARE.md`).
fn stock_word(value: i16) -> u32 {
    (u32::from(value.cast_unsigned()) << 16) | 0x32F2
}

struct Domain(RefCell<[u32; 32]>);

impl Domain {
    fn new() -> Self {
        Self(RefCell::new([0; 32]))
    }

    fn get(&self, n: u32) -> u32 {
        self.0.borrow()[usize::try_from(n).unwrap()]
    }

    fn set(&self, n: u32, v: u32) {
        self.0.borrow_mut()[usize::try_from(n).unwrap()] = v;
    }

    /// As `rtc::migrate`.
    fn boot(&self) -> bool {
        let Some((zone, flags)) = migrate(|n| self.get(n)) else { return false };
        for (n, v) in ZONE.into_iter().zip(zone) {
            self.set(n, v);
        }
        self.set(FLAGS, flags.encode());
        true
    }

    fn zone(&self) -> Option<Zone> {
        Zone::decode(ZONE.map(|n| self.get(n)))
    }

    fn flags(&self) -> Flags {
        Flags::decode(self.get(FLAGS)).unwrap()
    }
}

fn london() -> Zone {
    Zone::parse("60 1792890000 0 1806195600 60").unwrap()
}

/// The registers as the release before the move leaves them.
fn previous_release() -> Domain {
    let d = Domain::new();
    for (n, v) in old::ZONE.into_iter().zip(london().encode()) {
        d.set(n, v);
    }
    d.set(old::SOURCE, old::SOURCE_TAG | 1);
    d.set(old::RADIO, old::RADIO_ON);
    d.set(ALARM, Alarm::new(7, 0).unwrap().encode());
    d.set(LAST_SET_AT, 1_790_000_000);
    d
}

#[test]
fn every_register_written_is_one_stock_never_touches() {
    for n in OWNED {
        assert!(n < 32 && !STOCK.contains(&n), "BKP{n}R");
    }
    assert!(STOCK.contains(&5));
    let mut owned = OWNED.to_vec();
    owned.dedup();
    assert_eq!(owned.len(), OWNED.len());
}

#[test]
fn an_update_keeps_the_zone_the_radio_the_alarm_and_the_last_set() {
    let d = previous_release();
    let alarm = d.get(ALARM);
    assert!(d.boot());
    assert_eq!(d.zone(), Some(london()));
    assert_eq!(d.flags(), Flags { radio: true, source: 1 });
    assert_eq!(d.get(ALARM), alarm);
    assert_eq!(d.get(LAST_SET_AT), 1_790_000_000);
}

#[test]
fn the_old_registers_are_read_once_and_never_written() {
    let d = previous_release();
    let before: Vec<u32> = STOCK.iter().map(|&n| d.get(n)).collect();
    assert!(d.boot());
    assert_eq!(STOCK.map(|n| d.get(n)).to_vec(), before);
    for n in ZONE {
        d.set(n, 0);
    }
    assert!(!d.boot());
    assert_eq!(d.zone(), None, "a cleared zone stays cleared");
}

#[test]
fn stock_values_in_the_old_registers_come_over_as_nothing() {
    let d = Domain::new();
    d.set(0, stock_word(1));
    d.set(1, stock_word(-300));
    d.set(2, stock_word(60));
    d.set(5, 0x00A6_0602);
    d.set(11, 412.5f32.to_bits());
    d.set(12, 0x1C29_F7B3);
    d.set(13, (-23.5f32).to_bits());
    d.set(14, 41.25f32.to_bits());
    d.set(16, 1.02f32.to_bits());
    assert!(d.boot());
    assert_eq!(d.zone(), None);
    assert_eq!(d.flags(), Flags::default());
}

#[test]
fn a_return_from_stock_leaves_the_moved_records_alone() {
    let d = previous_release();
    assert!(d.boot());
    for n in STOCK {
        d.set(n, 0xDEAD_BEEF ^ n);
    }
    d.set(1, stock_word(-300));
    assert!(!d.boot());
    assert_eq!(d.zone(), Some(london()));
    assert_eq!(d.flags(), Flags { radio: true, source: 1 });
}

#[test]
fn a_reset_backup_domain_starts_empty() {
    let d = Domain::new();
    assert!(d.boot());
    assert_eq!(d.zone(), None);
    assert_eq!(d.flags(), Flags::default());
    assert!(!d.boot());
}

#[test]
fn flags_read_only_a_tagged_word() {
    for f in [Flags::default(), Flags { radio: true, source: 2 }, Flags { radio: false, source: 1 }] {
        assert_eq!(Flags::decode(f.encode()), Some(f));
    }
    let on = Flags { radio: true, source: 1 }.encode();
    assert_eq!(Flags::decode(on ^ 1 << 20), None);
    assert_eq!(Flags::decode(old::RADIO_ON), None);
}

#[test]
fn a_return_from_a_later_image_keeps_its_bits_and_moves_nothing() {
    let d = previous_release();
    assert!(d.boot());
    let tokyo = Zone::parse("540").unwrap();
    for (n, v) in ZONE.into_iter().zip(tokyo.encode()) {
        d.set(n, v);
    }
    let later = Flags::default().encode() | 0b11 << 4 | 1 << 2;
    d.set(FLAGS, later);
    assert!(!d.boot());
    assert_eq!(d.zone(), Some(tokyo));
    assert_eq!(d.flags(), Flags { radio: false, source: 3 });
    assert_eq!(Flags { radio: true, ..d.flags() }.encode_over(later), later | 1);
}
