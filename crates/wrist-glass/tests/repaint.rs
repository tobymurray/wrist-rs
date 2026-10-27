//! Repainting the columns that changed leaves the frame a full render draws, whatever was there.

use wrist_glass::pages::{
    background, compose, compose_about, compose_battery, render, repaint, About, Battery, Clock, Date, Faces, Status,
    ATKINSON, BUILT_IN, EN_GB, POPPINS,
};
use wrist_glass::scene::Scene;
use wrist_glass::stress::{compose_stress, Stress};
use wrist_glass::text::{strips, Glass, Line};
use wrist_glass::{HEIGHT, WIDTH};

const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;
const FAMILIES: [&Faces; 2] = [&POPPINS, &ATKINSON];
const WIDTHS: [usize; 4] = [1, 7, 16, W];

fn pixels(g: &Glass) -> Vec<u8> {
    let mut fb = vec![0u8; W * H];
    assert!(render(&mut fb, g));
    fb
}

fn bg() -> Vec<u8> {
    let mut bg = vec![0u8; W * H];
    assert!(background(&mut bg));
    bg
}

fn face(faces: &Faces, h: u8, m: u8, day: u8, soc_pct: Option<u8>) -> Glass {
    let status = Status { clock: Some(Clock { h, m, s: 0 }), date: Some(Date { year: 2026, month: 10, day }), soc_pct };
    compose(&status, &EN_GB, faces).0
}

/// `fb` repainted from `before` to `after`, a strip of at most `widest` columns at a time, with
/// `after`'s scene laid over whatever `scene` held.
fn step(fb: &mut [u8], bg: &[u8], scene: &mut Scene, before: Option<&Scene>, after: &Glass, widest: usize) {
    for i in 0..after.len() {
        scene.lay(after, i);
    }
    if after.is_empty() {
        *scene = Scene::new();
    }
    let changed = scene.changed_columns(before);
    for columns in strips(&changed, widest) {
        assert!(repaint(fb, bg, scene, before, columns));
    }
}

fn walk(what: &str, frames: &[Glass]) {
    let bg = bg();
    for widest in WIDTHS {
        let mut fb = vec![0xA5u8; W * H];
        let mut scene = Scene::of(&frames[frames.len() / 2]);
        step(&mut fb, &bg, &mut scene, None, &frames[0], widest);
        assert!(fb == pixels(&frames[0]), "{what}: the first frame, strips of {widest}");
        for pair in frames.windows(2) {
            let before = scene.clone();
            step(&mut fb, &bg, &mut scene, Some(&before), &pair[1], widest);
            assert!(fb == pixels(&pair[1]), "{what}: {:?} -> {:?}, strips of {widest}", pair[0], pair[1]);
            assert_eq!(scene.changed_columns(Some(&before)), pair[1].changed_columns(Some(&pair[0])));
        }
    }
}

#[test]
fn every_minute_of_a_day() {
    for faces in FAMILIES {
        let mut frames: Vec<_> = (0..24).flat_map(|h| (0..60).map(move |m| (h, m))).map(|(h, m)| face(faces, h, m, 27, Some(87))).collect();
        frames.push(face(faces, 0, 0, 28, Some(87)));
        walk(faces.name, &frames);
    }
}

#[test]
fn every_charge_up_and_down() {
    for faces in FAMILIES {
        let up = (0..=u8::MAX).map(|p| face(faces, 9, 41, 27, Some(p)));
        let down = (0..=u8::MAX).rev().map(|p| face(faces, 9, 41, 27, Some(p)));
        let frames: Vec<_> = up.chain([face(faces, 9, 41, 27, None)]).chain(down).collect();
        walk(faces.name, &frames);
        let battery = |mv| compose_battery(&Battery { soc_pct: Some(87), cell_mv: mv, external: Some(false) }, &EN_GB, faces).0;
        let frames: Vec<_> = (2900..4500).step_by(7).map(Some).chain([None, Some(65_535), Some(0)]).map(battery).collect();
        walk(faces.name, &frames);
    }
}

#[test]
fn between_every_page_and_locale() {
    for faces in FAMILIES {
        let mut frames = Vec::new();
        for l in BUILT_IN {
            frames.push(compose(&Status::default(), l, faces).0);
            frames.push(compose(&Status { clock: Some(Clock { h: 23, m: 59, s: 0 }), date: Some(Date { year: 2026, month: 9, day: 30 }), soc_pct: Some(100) }, l, faces).0);
            frames.push(compose_battery(&Battery { soc_pct: Some(5), cell_mv: Some(3412), external: Some(true) }, l, faces).0);
            frames.push(compose_battery(&Battery::default(), l, faces).0);
            frames.push(compose_about(&About { version: "0.2.0", build: "5784ec72019b-dirty" }, l, faces).0);
            frames.push(compose_about(&About { version: "100.100.100", build: "a-name-longer-than-1" }, l, faces).0);
        }
        frames.push(Glass::new());
        frames.push(face(faces, 12, 0, 1, Some(50)));
        walk(faces.name, &frames);
    }
}

#[test]
fn through_every_stress_screen() {
    for faces in FAMILIES {
        let frames: Vec<_> =
            Stress::ALL.into_iter().flat_map(|k| (0..40).map(move |step| compose_stress(k, step, faces).0)).collect();
        walk(faces.name, &frames);
    }
}

#[test]
fn a_box_for_a_missing_character() {
    for faces in FAMILIES {
        let glass = |s: &str| {
            let mut line = Line::new();
            line.push_verbatim(s).unwrap();
            let mut g = Glass::new();
            let _ = g.put(faces.time, 143, &line);
            g
        };
        walk(faces.name, &[glass("1 2"), glass("1\u{2007}2"), glass("\u{2603}"), glass("1 2")]);
    }
}

#[test]
fn strips_cover_what_is_marked_and_nothing_else() {
    let mut seed = 0x2545_F491_u32;
    for _ in 0..2000 {
        let mut marked = [false; W];
        for m in &mut marked {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *m = seed.is_multiple_of(3) || (seed >> 8).is_multiple_of(11);
        }
        for widest in [0, 1, 2, 16, 300] {
            let mut covered = [false; W];
            let mut end = 0;
            for s in strips(&marked, widest) {
                assert!(s.start >= end && s.start < s.end, "{s:?} after {end}");
                assert!(s.end - s.start <= i32::try_from(widest.max(1)).unwrap());
                for x in s.clone() {
                    covered[usize::try_from(x).unwrap()] = true;
                }
                end = s.end;
            }
            assert_eq!(covered, marked);
        }
    }
}
