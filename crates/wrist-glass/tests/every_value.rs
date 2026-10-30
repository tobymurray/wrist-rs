//! Every value each slot can hold fits it, to the pixel, in every built-in locale and both
//! typefaces, without touching another line, so no locale's data or translation can cut a line.

use wrist_glass::pages::{compose, compose_about, compose_battery, About, Battery, Clock, Date, Faces, Short, Status, ATKINSON, BUILT_IN, POPPINS, STEPS_SHOWN_MAX};
use wrist_glass::text::{Glass, Placed};

const FAMILIES: [&Faces; 2] = [&POPPINS, &ATKINSON];

fn days_in(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn whole(what: &str, (glass, short): (Glass, Short)) {
    assert_eq!(short, 0, "{what}: {glass:?}");
    let mut rows: Vec<_> = glass.lines().map(Placed::ink_rows).filter(|r| !r.is_empty()).collect();
    rows.sort_unstable_by_key(|r| r.start);
    assert!(rows.windows(2).all(|p| p[0].end <= p[1].start), "{what}: lines overlap: {glass:?}");
}

#[test]
fn every_date_fits_the_face() {
    for (faces, l) in FAMILIES.iter().flat_map(|f| BUILT_IN.iter().map(move |l| (f, l))) {
        let mut combos = std::collections::HashSet::new();
        for year in 2000..2028 {
            for month in 1..=12 {
                for day in 1..=days_in(year, month) {
                    let d = Date { year, month, day };
                    let status = Status { clock: Some(Clock { h: 23, m: 59, s: 0 }), date: Some(d), soc_pct: Some(100), steps: Some(wrist_glass::pages::STEPS_SHOWN_MAX) };
                    whole(&format!("{} {} {year}-{month}-{day}", faces.name, l.tag), compose(&status, l, faces));
                    combos.insert((d.weekday(), day, month));
                }
            }
        }
        // Every real (weekday, day, month): 7 for each of the 366 days a year can have.
        assert_eq!(combos.len(), 7 * 366);
        let not_a_day = Status { date: Some(Date { year: 2026, month: 13, day: 1 }), ..Status::default() };
        whole(&format!("{} {} not a day", faces.name, l.tag), compose(&not_a_day, l, faces));
    }
}

#[test]
fn every_time_and_charge_fits_the_face() {
    for (faces, l) in FAMILIES.iter().flat_map(|f| BUILT_IN.iter().map(move |l| (f, l))) {
        for h in 0..24 {
            for m in 0..60 {
                let status = Status { clock: Some(Clock { h, m, s: 0 }), date: None, soc_pct: None, steps: None };
                whole(&format!("{} {} {h}:{m}", faces.name, l.tag), compose(&status, l, faces));
            }
        }
        for pct in 0..=u8::MAX {
            let status = Status { soc_pct: Some(pct), ..Status::default() };
            whole(&format!("{} {} {pct} %", faces.name, l.tag), compose(&status, l, faces));
        }
    }
}

#[test]
fn every_step_count_fits_the_face() {
    for (faces, l) in FAMILIES.iter().flat_map(|f| BUILT_IN.iter().map(move |l| (f, l))) {
        let big = (1..=6).map(|d| 10u32.pow(d) - 1).chain([STEPS_SHOWN_MAX, STEPS_SHOWN_MAX + 1, u32::MAX]);
        for steps in (0..=19_999).chain(big) {
            let status = Status {
                clock: Some(Clock { h: 23, m: 59, s: 0 }),
                date: Some(Date { year: 2026, month: 9, day: 30 }),
                soc_pct: Some(100),
                steps: Some(steps),
            };
            whole(&format!("{} {} {steps} steps", faces.name, l.tag), compose(&status, l, faces));
        }
    }
}

#[test]
fn counts_past_the_most_shown_show_as_it() {
    for faces in FAMILIES {
        let at = |steps| {
            let status = Status { steps: Some(steps), ..Status::default() };
            compose(&status, &BUILT_IN[0], faces).0
        };
        assert_eq!(at(u32::MAX), at(STEPS_SHOWN_MAX));
        assert_ne!(at(STEPS_SHOWN_MAX - 1), at(STEPS_SHOWN_MAX));
    }
}

#[test]
fn no_count_draws_no_line() {
    for faces in FAMILIES {
        let without = compose(&Status::default(), &BUILT_IN[0], faces).0;
        let with = compose(&Status { steps: Some(0), ..Status::default() }, &BUILT_IN[0], faces).0;
        assert_eq!(with.lines().count(), without.lines().count() + 1);
    }
}

#[test]
fn every_reading_fits_the_battery_page() {
    for (faces, l) in FAMILIES.iter().flat_map(|f| BUILT_IN.iter().map(move |l| (f, l))) {
        for pct in 0..=u8::MAX {
            let b = Battery { soc_pct: Some(pct), cell_mv: None, external: None };
            whole(&format!("{} {} {pct} %", faces.name, l.tag), compose_battery(&b, l, faces));
        }
        for mv in 0..=u32::from(u16::MAX) {
            for external in [Some(true), Some(false), None] {
                let b = Battery { soc_pct: None, cell_mv: Some(mv), external };
                let (glass, short) = compose_battery(&b, l, faces);
                assert_eq!(short, 0, "{} {} {mv} mV", faces.name, l.tag);
                if mv.is_multiple_of(1111) {
                    whole(&format!("{} {} {mv} mV", faces.name, l.tag), (glass, short));
                }
            }
        }
    }
}

#[test]
fn the_about_page_fits() {
    for (faces, l) in FAMILIES.iter().flat_map(|f| BUILT_IN.iter().map(move |l| (f, l))) {
        for build in ["5784ec72019b", "5784ec72019b-dirty", "888888888888-dirty"] {
            whole(&format!("{} {} {build}", faces.name, l.tag), compose_about(&About { version: "100.100.100", build }, l, faces));
        }
    }
}
