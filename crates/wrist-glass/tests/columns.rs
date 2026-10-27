use wrist_glass::pages::{compose, compose_about, compose_battery, render, About, Battery, Clock, Date, Faces, Status, ATKINSON, EN_GB, POPPINS};
use wrist_glass::text::Glass;
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

fn pixel_columns(a: &[u8], b: &[u8]) -> Vec<usize> {
    (0..W).filter(|&x| (0..H).any(|y| a[y * W + x] != b[y * W + x])).collect()
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
        frames.push(compose_about(&About { version: "0.2.0", build: "5784ec72019b-dirty" }, &EN_GB, faces).0);
        frames.push(compose_about(&About { version: "0.2.0", build: "5784ec72019b" }, &EN_GB, faces).0);
        frames.push(glass(faces, 9, 41, 30, Some(55)));
        for pair in frames.windows(2) {
            assert!(partial_redraw(&pair[0], &pair[1]) == pixels(&pair[1]), "{}: {:?} -> {:?}", faces.name, pair[0], pair[1]);
        }
    }
}

#[test]
fn a_minute_marks_only_the_digit_that_turned() {
    for faces in FAMILIES {
        let (a, b) = (glass(faces, 16, 27, 20, Some(100)), glass(faces, 16, 28, 20, Some(100)));
        let marked = marked_columns(Some(&a), &b);
        let changed = pixel_columns(&pixels(&a), &pixels(&b));
        assert_ne!(changed, Vec::<usize>::new());
        assert!(changed.first() >= marked.first() && changed.last() <= marked.last());
        assert!(marked.len() < W / 4, "{}: {} columns", faces.name, marked.len());
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
