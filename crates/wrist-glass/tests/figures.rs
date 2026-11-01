//! A line that changes in place keeps every character that did not where it was: for every value
//! each such line can show, in every built-in locale and both typefaces, a character at the same
//! place in a string of the same length is drawn in the same columns.

use std::collections::HashMap;

use wrist_glass::pages::{compose, compose_battery, Battery, Clock, Faces, Locale, Status, ATKINSON, BUILT_IN, POPPINS};
use wrist_glass::text::Glass;

const FAMILIES: [&Faces; 2] = [&POPPINS, &ATKINSON];

/// Every value line `i` of `glasses` takes, held to one pen and ink for each (length, index,
/// character).
fn stays_put(what: &str, i: usize, glasses: impl IntoIterator<Item = Glass>) {
    let mut seen = HashMap::new();
    for glass in glasses {
        let line = glass.lines().nth(i).unwrap();
        let n = line.glyphs().count();
        for (at, g) in line.glyphs().enumerate() {
            let first = seen.entry((n, at, g.ch)).or_insert((g.pen, g.ink, line.text().to_owned()));
            assert_eq!((first.0, first.1), (g.pen, g.ink), "{what}: {:?} at {at} moved from {:?} to {:?}", g.ch, first.2, line.text());
        }
    }
}

fn each(f: impl Fn(&str, &Faces, &Locale)) {
    for faces in FAMILIES {
        for l in BUILT_IN {
            f(&format!("{} {}", faces.name, l.tag), faces, l);
        }
    }
}

fn face(l: &Locale, faces: &Faces, clock: Option<Clock>, soc_pct: Option<u8>) -> Glass {
    compose(&Status { clock, date: None, soc_pct, steps: None }, l, faces).0
}

#[test]
fn the_time_keeps_its_place_every_minute() {
    each(|what, faces, l| {
        let minutes = (0..24).flat_map(|h| (0..60).map(move |m| Some(Clock { h, m, s: 0 })));
        stays_put(what, 1, minutes.chain([None]).map(|c| face(l, faces, c, None)));
    });
}

#[test]
fn the_charge_keeps_its_place() {
    each(|what, faces, l| {
        let pcts = || (0..=u8::MAX).map(Some).chain([None]);
        stays_put(what, 2, pcts().map(|p| face(l, faces, None, p)));
        stays_put(what, 1, pcts().map(|p| compose_battery(&Battery { soc_pct: p, ..Battery::default() }, l, faces).0));
    });
}

#[test]
fn the_cell_voltage_keeps_its_place() {
    each(|what, faces, l| {
        let mvs = (0..=9999).map(Some).chain([None]);
        stays_put(what, 2, mvs.map(|mv| compose_battery(&Battery { cell_mv: mv, ..Battery::default() }, l, faces).0));
    });
}
