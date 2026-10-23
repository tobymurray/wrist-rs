use wrist_glass::pages::{compose, render, Clock, Date, Status};
use wrist_glass::text::Glass;
use wrist_glass::{HEIGHT, WIDTH};

const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;

fn glass(h: u8, m: u8, day: u8, soc_pct: Option<u8>) -> Glass {
    let status = Status {
        clock: Some(Clock { h, m, s: 0 }),
        date: Some(Date { year: 2026, month: 10, day }),
        soc_pct,
    };
    compose(&status).0
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

#[test]
fn every_changed_pixel_is_in_a_marked_column() {
    for (a, b) in [
        (glass(16, 27, 20, Some(100)), glass(16, 28, 20, Some(100))),
        (glass(16, 29, 20, Some(100)), glass(16, 30, 20, Some(100))),
        (glass(9, 59, 20, Some(100)), glass(10, 0, 20, Some(100))),
        (glass(16, 27, 20, Some(100)), glass(16, 27, 20, Some(99))),
        (glass(23, 59, 20, Some(9)), glass(0, 0, 21, Some(10))),
        (glass(12, 0, 20, None), glass(12, 0, 20, Some(55))),
    ] {
        let marked = marked_columns(Some(&a), &b);
        for x in pixel_columns(&pixels(&a), &pixels(&b)) {
            assert!(marked.contains(&x), "column {x} changed but was not marked");
        }
    }
}

#[test]
fn a_minute_marks_only_the_digit_that_turned() {
    let (a, b) = (glass(16, 27, 20, Some(100)), glass(16, 28, 20, Some(100)));
    let marked = marked_columns(Some(&a), &b);
    let changed = pixel_columns(&pixels(&a), &pixels(&b));
    assert_ne!(changed, Vec::<usize>::new());
    assert!(changed.first() >= marked.first() && changed.last() <= marked.last());
    assert!(marked.len() < W / 4, "{} columns", marked.len());
}

#[test]
fn an_unchanged_face_marks_nothing() {
    let g = glass(16, 27, 20, Some(100));
    assert_eq!(marked_columns(Some(&g), &g), Vec::<usize>::new());
}

#[test]
fn without_a_previous_glass_every_column_is_marked() {
    assert_eq!(marked_columns(None, &glass(16, 27, 20, Some(100))).len(), W);
}
