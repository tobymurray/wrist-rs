#![allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]

use inscribed_disc::audit::off_glass;
use inscribed_disc::{Abgr2222, Clip, Surface};
use wrist_glass::pages::{compose, render, Clock, Date, Status};
use wrist_glass::text::Glass;
use wrist_glass::{ring, HEIGHT, MARGIN, RING_INSET, RING_STROKE, WIDTH};

const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;
const WI: i32 = WIDTH as i32;
const HI: i32 = HEIGHT as i32;
const BLACK: u8 = 0;

fn cases() -> Vec<(&'static str, Status)> {
    vec![
        (
            "max",
            Status {
                clock: Some(Clock { h: 23, m: 59, s: 59 }),
                date: Some(Date { year: u16::MAX, month: 9, day: 30 }),
                soc_pct: Some(u8::MAX),
            },
        ),
        (
            "min",
            Status {
                clock: Some(Clock { h: 0, m: 0, s: 0 }),
                date: Some(Date { year: 0, month: 13, day: 0 }),
                soc_pct: Some(0),
            },
        ),
        ("none", Status::default()),
        ("typical", typical()),
    ]
}

fn typical() -> Status {
    Status {
        clock: Some(Clock { h: 16, m: 27, s: 33 }),
        date: Some(Date { year: 2026, month: 10, day: 17 }),
        soc_pct: Some(100),
    }
}

fn text_frame(glass: &Glass, clip: Clip) -> Vec<u8> {
    let mut fb = vec![BLACK; W * H];
    let mut surface = match clip {
        Clip::Disc => Surface::<Abgr2222>::round(&mut fb, WIDTH, HEIGHT),
        Clip::Rect => Surface::<Abgr2222>::rect(&mut fb, WIDTH, HEIGHT),
    }
    .unwrap();
    glass.draw_with(&mut surface, Abgr2222::WHITE);
    fb
}

fn texts(glass: &Glass) -> Vec<String> {
    glass.lines().iter().map(|l| String::from_utf8_lossy(l.text()).into_owned()).collect()
}

#[test]
fn no_line_is_cut_short_at_any_extreme() {
    for (name, s) in cases() {
        let (glass, short) = compose(&s);
        assert_eq!(short, 0, "{name}: lines {short:#010b} did not fit: {:?}", texts(&glass));
    }
}

#[test]
fn lines_do_not_overlap() {
    for (name, s) in cases() {
        let (glass, _) = compose(&s);
        let mut bands: Vec<_> = glass.lines().iter().map(|l| (l.top, l.top + 7 * l.scale as i32)).collect();
        bands.sort_unstable();
        for pair in bands.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "{name}: lines overlap: {bands:?}");
        }
    }
}

#[test]
fn the_face_is_centred() {
    let (glass, _) = compose(&typical());
    let lines = glass.lines();
    let top = lines.iter().map(|l| l.top).min().unwrap();
    let bottom = lines.iter().map(|l| l.top + 7 * l.scale as i32).max().unwrap();
    assert!((top - (HI - bottom)).abs() <= 1, "rows {top} to {bottom} are off centre");
}

#[test]
fn unclipped_text_is_inside_the_ring() {
    let inside_ring = RING_INSET + i32::try_from(RING_STROKE).unwrap();
    assert!(MARGIN >= inside_ring);
    for (name, s) in cases() {
        let (glass, _) = compose(&s);
        let fb = text_frame(&glass, Clip::Rect);
        let off: Vec<_> = off_glass(&fb, WI, HI, inside_ring, BLACK).collect();
        assert!(off.is_empty(), "{name}: {} glyph pixels under the ring or off the glass, first at {:?}", off.len(), off[0]);
    }
}

#[test]
fn the_disc_clip_drops_no_glyph_pixel() {
    for (name, s) in cases() {
        let (glass, _) = compose(&s);
        let fb = text_frame(&glass, Clip::Disc);
        let lit = u32::try_from(fb.iter().filter(|&&b| b != BLACK).count()).unwrap();
        assert_eq!(lit, glass.lit_pixels(), "{name}: glyph pixels lost to the clip");
    }
}

#[test]
fn the_ring_is_on_the_glass() {
    let mut fb = vec![BLACK; W * H];
    let mut surface = Surface::<Abgr2222>::rect(&mut fb, WIDTH, HEIGHT).unwrap();
    ring(&mut surface);
    assert_eq!(off_glass(&fb, WI, HI, 0, BLACK).count(), 0);
    assert!(fb.iter().any(|&b| b != BLACK), "the ring drew nothing");
}

#[test]
fn the_stepwise_render_is_the_same_frame() {
    let mut bg = vec![BLACK; W * H];
    assert!(wrist_glass::pages::background(&mut bg));
    for (name, s) in cases() {
        let (glass, _) = compose(&s);
        let mut whole = vec![BLACK; W * H];
        assert!(render(&mut whole, &glass));
        let mut steps = bg.clone();
        for i in 0..glass.lines().len() {
            assert!(wrist_glass::pages::draw_line(&mut steps, &glass, i));
        }
        assert!(whole == steps, "{name}: the stepwise frame differs");
    }
}

#[test]
fn a_wrong_sized_buffer_draws_nothing() {
    let mut fb = vec![BLACK; 10];
    assert!(!render(&mut fb, &Glass::new()));
}

#[test]
fn the_face_reads_right() {
    let (glass, _) = compose(&typical());
    assert_eq!(texts(&glass), ["Sat 17 Oct", "16:27", "100 %"]);
    assert_eq!(glass.lines()[1].scale, 7);
    let (glass, _) = compose(&Status::default());
    assert_eq!(texts(&glass), ["set time", "--:--", "-- %"]);
    let a_second_later = Status { clock: Some(Clock { h: 16, m: 27, s: 34 }), ..typical() };
    assert_eq!(compose(&typical()).0, compose(&a_second_later).0);
}

#[test]
fn weekdays() {
    let wd = |year, month, day| Date { year, month, day }.weekday();
    assert_eq!(wd(2026, 10, 17), 5);
    assert_eq!(wd(2000, 1, 1), 5);
    assert_eq!(wd(2024, 2, 29), 3);
    assert_eq!(wd(1970, 1, 1), 3);
}

#[cfg(feature = "preview")]
#[test]
fn previews() {
    use inscribed_disc::preview::{to_rgba, write_png, Bezel, Options};
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("wrist-glass");
    std::fs::create_dir_all(&dir).unwrap();
    let opts = Options { scale: 2, bezel: Bezel::Fill([40, 40, 40]) };
    for (name, s) in cases() {
        let (glass, _) = compose(&s);
        let mut fb = vec![BLACK; W * H];
        assert!(render(&mut fb, &glass));
        let rgba = to_rgba(&fb, WIDTH, HEIGHT, opts);
        write_png(dir.join(format!("face-{name}.png")), &rgba, WIDTH * 2, HEIGHT * 2).unwrap();
    }
    eprintln!("previews in {}", dir.display());
}
