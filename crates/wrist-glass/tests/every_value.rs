//! Every value each slot can hold fits it in every built-in locale, so no locale's data or
//! translation can cut a line.

use wrist_glass::pages::{compose, compose_about, compose_battery, About, Battery, Clock, Date, Status, BUILT_IN};

fn days_in(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[test]
fn every_date_fits_the_face() {
    for l in BUILT_IN {
        let mut combos = std::collections::HashSet::new();
        for year in 2000..2028 {
            for month in 1..=12 {
                for day in 1..=days_in(year, month) {
                    let d = Date { year, month, day };
                    let status = Status { clock: Some(Clock { h: 23, m: 59, s: 0 }), date: Some(d), soc_pct: Some(100) };
                    assert_eq!(compose(&status, l).1, 0, "{} {year}-{month}-{day}", l.tag);
                    combos.insert((d.weekday(), day, month));
                }
            }
        }
        // Every real (weekday, day, month): 7 for each of the 366 days a year can have.
        assert_eq!(combos.len(), 7 * 366);
        let not_a_day = Status { date: Some(Date { year: 2026, month: 13, day: 1 }), ..Status::default() };
        assert_eq!(compose(&not_a_day, l).1, 0, "{} not a day", l.tag);
    }
}

#[test]
fn every_time_and_charge_fits_the_face() {
    for l in BUILT_IN {
        for h in 0..24 {
            for m in 0..60 {
                let status = Status { clock: Some(Clock { h, m, s: 0 }), date: None, soc_pct: None };
                assert_eq!(compose(&status, l).1, 0, "{} {h}:{m}", l.tag);
            }
        }
        for pct in 0..=u8::MAX {
            assert_eq!(compose(&Status { soc_pct: Some(pct), ..Status::default() }, l).1, 0, "{} {pct} %", l.tag);
        }
    }
}

#[test]
fn every_reading_fits_the_battery_page() {
    for l in BUILT_IN {
        for pct in 0..=u8::MAX {
            let b = Battery { soc_pct: Some(pct), cell_mv: None, external: None };
            assert_eq!(compose_battery(&b, l).1, 0, "{} {pct} %", l.tag);
        }
        for mv in 0..=u32::from(u16::MAX) {
            for external in [Some(true), Some(false), None] {
                let b = Battery { soc_pct: None, cell_mv: Some(mv), external };
                assert_eq!(compose_battery(&b, l).1, 0, "{} {mv} mV", l.tag);
            }
        }
    }
}

#[test]
fn the_about_page_fits() {
    for l in BUILT_IN {
        assert_eq!(compose_about(&About { version: "0.2.0", build: "5784ec72019b" }, l).1, 0, "{}", l.tag);
    }
}
