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

    fn boot(&self) -> bool {
        migrate(|n| self.get(n), |n, v| self.set(n, v))
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
fn only_the_old_records_own_tags_are_zeroed_and_only_once() {
    let d = previous_release();
    let before: Vec<u32> = STOCK.iter().map(|&n| d.get(n)).collect();
    assert!(d.boot());
    for (&n, &was) in STOCK.iter().zip(&before) {
        let now = if n == old::ZONE[0] || n == old::SOURCE { 0 } else { was };
        assert_eq!(d.get(n), now, "BKP{n}R");
    }
    for n in ZONE {
        d.set(n, 0);
    }
    assert!(!d.boot());
    assert_eq!(d.zone(), None, "a cleared zone stays cleared");
}

#[test]
fn a_change_an_older_image_applied_after_a_revert_is_not_applied_twice() {
    let d = Domain::new();
    let gmt = Zone::parse("0 1806195600 60").unwrap();
    for (n, v) in old::ZONE.into_iter().zip(gmt.encode()) {
        d.set(n, v);
    }
    assert!(d.boot());
    assert_eq!(d.zone(), Some(gmt));
    let (shift, after) = gmt.due(1_806_195_600 + 600).unwrap();
    assert_eq!(shift, 60);
    for (n, v) in old::ZONE.into_iter().zip(after.encode()) {
        d.set(n, v);
    }
    d.set(old::SOURCE, old::SOURCE_TAG | 2);
    assert!(d.boot());
    assert_eq!(d.zone(), Some(after));
    assert_eq!(d.zone().unwrap().due(1_806_195_600 + 3600 + 600), None);
    assert_eq!(d.flags().source, 2);
    assert!(!d.boot());
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

#[test]
fn a_time_set_on_an_older_image_clears_the_moved_zone() {
    let d = previous_release();
    assert!(d.boot());
    d.set(old::SOURCE, old::SOURCE_TAG | 1);
    assert!(d.boot());
    assert_eq!(d.zone(), None);
    assert_eq!(d.flags(), Flags { radio: true, source: 1 });
}

#[test]
fn an_older_image_after_a_later_one_keeps_the_later_bits() {
    let d = previous_release();
    assert!(d.boot());
    let later = Flags::default().encode() | 0b11 << 4 | 1 << 2 | 1;
    d.set(FLAGS, later);
    assert_eq!(Flags { radio: false, ..d.flags() }.encode_over(later), later & !1);
    for (n, v) in old::ZONE.into_iter().zip(london().encode()) {
        d.set(n, v);
    }
    assert!(d.boot());
    assert_eq!(d.get(FLAGS), later);
}

#[test]
fn a_reset_at_any_write_of_the_move_loses_nothing() {
    let start = || {
        let d = previous_release();
        assert!(d.boot());
        d.set(old::SOURCE, old::SOURCE_TAG | 1);
        for (n, v) in old::ZONE.into_iter().zip(london().encode()) {
            d.set(n, v);
        }
        d
    };
    let whole = start();
    assert!(whole.boot());
    for cut in 0.. {
        let d = start();
        let writes = RefCell::new(0);
        let done = migrate(
            |n| d.get(n),
            |n, v| {
                if *writes.borrow() < cut {
                    d.set(n, v);
                }
                *writes.borrow_mut() += 1;
            },
        );
        assert!(done);
        d.boot();
        assert_eq!(d.0.borrow().to_vec(), whole.0.borrow().to_vec(), "reset after {cut} writes");
        if *writes.borrow() <= cut {
            break;
        }
    }
}
