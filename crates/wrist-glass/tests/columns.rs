use wrist_glass::pages::{compose, compose_about, compose_battery, render, About, Battery, Clock, Date, Faces, Status, ATKINSON, EN_GB, POPPINS};
use wrist_glass::stress::{compose_stress, Stress};
use wrist_glass::text::{Glass, Line};
use wrist_glass::{HEIGHT, WIDTH};

const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;
const FAMILIES: [&Faces; 2] = [&POPPINS, &ATKINSON];

fn glass(faces: &Faces, h: u8, m: u8, day: u8, soc_pct: Option<u8>) -> Glass {
    let status = Status {
        clock: Some(Clock { h, m, s: 0 }),
        date: Some(Date { year: 2026, month: 9, day }),
        soc_pct,
    };
    compose(&status, &EN_GB, faces).0
}

fn pixels(g: &Glass) -> Vec<u8> {
    let mut fb = vec![0u8; W * H];
    assert!(render(&mut fb, g));
    fb
}

fn marked_columns(before: Option<&Glass>, after: &Glass) -> Vec<usize> {
    let changed = after.changed_columns(before);
    (0..W).filter(|&x| changed[x]).collect()
}

/// What the panel holds after `before` is replaced by `after` in the marked columns only.
fn partial_redraw(before: &Glass, after: &Glass) -> Vec<u8> {
    let mut panel = pixels(before);
    let fresh = pixels(after);
    for x in marked_columns(Some(before), after) {
        for y in 0..H {
            panel[y * W + x] = fresh[y * W + x];
        }
    }
    panel
}

#[test]
fn a_partial_redraw_is_the_full_redraw() {
    for faces in FAMILIES {
        let mut frames = Vec::new();
        for (h, m) in (0..24).flat_map(|h| (0..60).map(move |m| (h, m))) {
            frames.push(glass(faces, h, m, 29, Some(100)));
        }
        frames.push(glass(faces, 0, 0, 30, Some(100)));
        frames.extend((0..=u8::MAX).map(|pct| glass(faces, 23, 59, 30, Some(pct))));
        frames.push(glass(faces, 23, 59, 30, None));
        frames.push(compose(&Status::default(), &EN_GB, faces).0);
        frames.push(compose_battery(&Battery { soc_pct: Some(87), cell_mv: Some(4012), external: Some(false) }, &EN_GB, faces).0);
        frames.push(compose_battery(&Battery { soc_pct: Some(100), cell_mv: Some(4405), external: Some(true) }, &EN_GB, faces).0);
        frames.push(compose_about(&About { version: "0.2.0", build: "5784ec72019b-dirty", crc: None }, &EN_GB, faces).0);
        frames.push(compose_about(&About { version: "0.2.0", build: "5784ec72019b", crc: None }, &EN_GB, faces).0);
        frames.push(glass(faces, 9, 41, 30, Some(55)));
        frames.extend(Stress::ALL.into_iter().flat_map(|k| (0..12).map(move |step| compose_stress(k, step, faces).0)));
        for pair in frames.windows(2) {
            assert!(partial_redraw(&pair[0], &pair[1]) == pixels(&pair[1]), "{}: {:?} -> {:?}", faces.name, pair[0], pair[1]);
        }
    }
}

/// The ink columns, before and after, of the characters of line `i` that changed.
fn turned_columns(before: &Glass, after: &Glass, i: usize) -> Vec<usize> {
    let (a, b) = (before.lines().nth(i).unwrap(), after.lines().nth(i).unwrap());
    assert_eq!(a.glyphs().count(), b.glyphs().count(), "{a:?} -> {b:?}");
    let turned = a.glyphs().zip(b.glyphs()).filter(|(o, n)| o.ch != n.ch).flat_map(|(o, n)| [o.ink, n.ink]);
    let mut columns: Vec<usize> = turned.flat_map(|(l, r)| l..r).map(|x| usize::try_from(x).unwrap()).collect();
    columns.sort_unstable();
    columns.dedup();
    columns
}

#[test]
fn a_minute_marks_only_the_digits_that_turned() {
    for faces in FAMILIES {
        let minutes: Vec<_> = (0..24).flat_map(|h| (0..60).map(move |m| (h, m))).collect();
        for (&(h0, m0), &(h1, m1)) in minutes.iter().zip(minutes.iter().cycle().skip(1)) {
            let (a, b) = (glass(faces, h0, m0, 20, Some(100)), glass(faces, h1, m1, 20, Some(100)));
            assert_eq!(marked_columns(Some(&a), &b), turned_columns(&a, &b, 1), "{}: {h0}:{m0} -> {h1}:{m1}", faces.name);
        }
    }
}

#[test]
fn a_reading_marks_only_the_digits_that_turned() {
    for faces in FAMILIES {
        for pct in 0..u8::MAX {
            let (a, b) = (glass(faces, 16, 27, 20, Some(pct)), glass(faces, 16, 27, 20, Some(pct + 1)));
            if a.lines().nth(2).unwrap().text().len() == b.lines().nth(2).unwrap().text().len() {
                assert_eq!(marked_columns(Some(&a), &b), turned_columns(&a, &b, 2), "{}: {pct} %", faces.name);
            }
        }
        let battery = |mv| compose_battery(&Battery { soc_pct: Some(87), cell_mv: Some(mv), external: Some(false) }, &EN_GB, faces).0;
        for mv in 3000..4500 {
            let (a, b) = (battery(mv), battery(mv + 1));
            assert_eq!(marked_columns(Some(&a), &b), turned_columns(&a, &b, 2), "{}: {mv} mV", faces.name);
        }
    }
}

#[test]
fn an_unchanged_face_marks_nothing() {
    for faces in FAMILIES {
        let g = glass(faces, 16, 27, 20, Some(100));
        assert_eq!(marked_columns(Some(&g), &g), Vec::<usize>::new());
    }
}

#[test]
fn without_a_previous_glass_every_column_is_marked() {
    assert_eq!(marked_columns(None, &glass(&POPPINS, 16, 27, 20, Some(100))).len(), W);
}

#[test]
fn a_character_drawn_as_a_box_is_redrawn_where_it_is() {
    for faces in FAMILIES {
        for font in [faces.words, faces.time] {
            let glass = |s: &str| {
                let mut line = Line::new();
                line.push_verbatim(s).unwrap();
                let mut g = Glass::new();
                let _ = g.put(font, 143, &line);
                g
            };
            let (a, b) = (glass("1 2"), glass("1\u{2007}2"));
            assert!(partial_redraw(&a, &b) == pixels(&b), "{}: {a:?} -> {b:?}", faces.name);
            assert!(partial_redraw(&b, &a) == pixels(&a), "{}: {b:?} -> {a:?}", faces.name);
        }
    }
}
