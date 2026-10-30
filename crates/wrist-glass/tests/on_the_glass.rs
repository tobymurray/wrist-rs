#![allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]

use inscribed_disc::audit::off_glass;
use inscribed_disc::{Abgr2222, Clip, Surface};
use wrist_glass::pages::{
    compose, compose_about, compose_battery, render, About, Battery, Clock, Date, Faces, Short, Status, ATKINSON, EN_GB,
    POPPINS,
};
use wrist_glass::text::{Glass, Placed};
use wrist_glass::{ring, HEIGHT, MARGIN, RING_INSET, RING_STROKE, WIDTH};

const W: usize = WIDTH as usize;
const H: usize = HEIGHT as usize;
const WI: i32 = WIDTH as i32;
const HI: i32 = HEIGHT as i32;
const BLACK: u8 = 0;
const FAMILIES: [&Faces; 2] = [&POPPINS, &ATKINSON];

fn cases() -> Vec<(&'static str, Status)> {
    vec![
        (
            "max",
            Status {
                clock: Some(Clock { h: 23, m: 59, s: 59 }),
                date: Some(Date { year: u16::MAX, month: 9, day: 30 }),
                soc_pct: Some(u8::MAX),
                steps: Some(wrist_glass::pages::STEPS_SHOWN_MAX),
            },
        ),
        (
            "min",
            Status {
                clock: Some(Clock { h: 0, m: 0, s: 0 }),
                date: Some(Date { year: 0, month: 13, day: 0 }),
                soc_pct: Some(0),
                steps: Some(0),
            },
        ),
        ("none", Status::default()),
        ("typical", typical()),
        (
            "wed-30-sept",
            Status {
                clock: Some(Clock { h: 23, m: 59, s: 0 }),
                date: Some(Date { year: 2026, month: 9, day: 30 }),
                soc_pct: Some(100),
                steps: None,
            },
        ),
    ]
}

fn typical() -> Status {
    Status {
        clock: Some(Clock { h: 16, m: 27, s: 33 }),
        date: Some(Date { year: 2026, month: 10, day: 17 }),
        soc_pct: Some(100),
        steps: Some(4321),
    }
}

/// Every page at its extremes in both typefaces: the face's cases, then the battery and about pages'.
fn every_page() -> Vec<(String, (Glass, Short))> {
    FAMILIES.iter().flat_map(|f| pages(f).into_iter().map(|(name, g)| (format!("{}/{name}", f.name), g))).collect()
}

fn pages(faces: &Faces) -> Vec<(String, (Glass, Short))> {
    let mut all: Vec<_> =
        cases().into_iter().map(|(name, s)| (format!("face-{name}"), compose(&s, &EN_GB, faces))).collect();
    let batteries = [
        ("max", Battery { soc_pct: Some(u8::MAX), cell_mv: Some(65_535), external: Some(false) }),
        ("min", Battery { soc_pct: Some(0), cell_mv: Some(0), external: Some(true) }),
        ("none", Battery::default()),
    ];
    for (name, b) in batteries {
        all.push((format!("battery-{name}"), compose_battery(&b, &EN_GB, faces)));
    }
    let abouts = [
        ("release", About { version: "0.1.0", build: "5784ec72019b" }),
        ("dirty", About { version: "0.1.0", build: "5784ec72019b-dirty" }),
        ("widest", About { version: "100.100.100", build: "a-name-longer-than-1" }),
        ("long-version", About { version: LONG_VERSION, build: "5784ec72019b" }),
        ("long-build", About { version: "0.1.0", build: "a-name-longer-than-two-lines-of-the-page" }),
    ];
    for (name, a) in abouts {
        all.push((format!("about-{name}"), compose_about(&a, &EN_GB, faces)));
    }
    all
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

fn texts(glass: &Glass) -> Vec<&str> {
    glass.lines().map(Placed::text).collect()
}

const LONG_VERSION: &str = "0.10.0-rc.10+20261026.5784ec72019b";

#[test]
fn no_line_is_cut_short_at_any_extreme() {
    for (name, (glass, short)) in every_page().into_iter().filter(|(n, _)| !n.contains("/about-long")) {
        assert_eq!(short, 0, "{name}: lines {short:#010b} did not fit: {:?}", texts(&glass));
    }
}

#[test]
fn lines_do_not_overlap() {
    for (name, (glass, _)) in every_page() {
        let mut bands: Vec<_> = glass.lines().map(Placed::ink_rows).filter(|r| !r.is_empty()).collect();
        bands.sort_unstable_by_key(|r| r.start);
        for pair in bands.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{name}: lines overlap: {bands:?}");
        }
    }
}

#[test]
fn the_face_is_centred() {
    for (faces, steps) in FAMILIES.into_iter().flat_map(|f| [(f, None), (f, Some(4321))]) {
        let (glass, _) = compose(&Status { steps, ..typical() }, &EN_GB, faces);
        let top = glass.lines().map(|l| l.ink_rows().start).min().unwrap();
        let bottom = glass.lines().map(|l| l.ink_rows().end).max().unwrap();
        assert!((top - (HI - bottom)).abs() <= 2, "{}: rows {top} to {bottom} are off centre", faces.name);
    }
}

#[test]
fn unclipped_text_is_inside_the_ring() {
    let inside_ring = RING_INSET + i32::try_from(RING_STROKE).unwrap();
    assert!(MARGIN >= inside_ring);
    for (name, (glass, _)) in every_page() {
        let fb = text_frame(&glass, Clip::Rect);
        let off: Vec<_> = off_glass(&fb, WI, HI, inside_ring, BLACK).collect();
        assert!(off.is_empty(), "{name}: {} glyph pixels under the ring or off the glass, first at {:?}", off.len(), off[0]);
    }
}

#[test]
fn the_disc_clip_drops_no_glyph_pixel() {
    for (name, (glass, _)) in every_page() {
        assert!(text_frame(&glass, Clip::Disc) == text_frame(&glass, Clip::Rect), "{name}: glyph pixels lost to the clip");
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
    for (name, (glass, _)) in every_page() {
        let mut whole = vec![BLACK; W * H];
        assert!(render(&mut whole, &glass));
        let mut steps = bg.clone();
        for i in 0..glass.len() {
            assert!(wrist_glass::pages::draw_line(&mut steps, &glass, i));
        }
        assert!(whole == steps, "{name}: the stepwise frame differs");
        let mut strips = vec![0xA5; W * H];
        let scene = wrist_glass::scene::Scene::of(&glass);
        for x in (0..WI).step_by(9) {
            assert!(wrist_glass::pages::repaint(&mut strips, &scene, None, x..x + 9));
        }
        assert!(whole == strips, "{name}: the frame repainted in strips differs");
    }
}

#[test]
fn the_background_painted_box_by_box_is_the_background() {
    let mut whole = vec![0x5A; W * H];
    assert!(wrist_glass::pages::background(&mut whole));
    for (bw, bh) in [(1, 1), (7, 30), (16, 240), (61, 13), (240, 1), (240, 240)] {
        let mut boxes = vec![0xA5; W * H];
        for y in (-5..HI).step_by(bh) {
            for x in (-3..WI).step_by(bw) {
                let (w, h) = (i32::try_from(bw).unwrap(), i32::try_from(bh).unwrap());
                wrist_glass::pages::background_in(&mut boxes, y..y + h, x..x + w);
            }
        }
        assert!(whole == boxes, "boxes of {bw} x {bh}");
    }
}

#[test]
fn a_wrong_sized_buffer_draws_nothing() {
    let mut fb = vec![BLACK; 10];
    assert!(!render(&mut fb, &Glass::new()));
}

#[test]
fn the_face_reads_right() {
    for faces in FAMILIES {
        let (glass, _) = compose(&typical(), &EN_GB, faces);
        assert_eq!(texts(&glass), ["Sat 17 Oct", "16:27", "100 %", "4321 steps"]);
        let faces_used: Vec<u8> = glass.lines().map(|l| l.font.face.px).collect();
        assert_eq!(faces_used, [faces.words.face.px, faces.time.face.px, faces.words.face.px, faces.id.face.px]);
        let one = Status { steps: Some(1), ..typical() };
        assert_eq!(texts(&compose(&one, &EN_GB, faces).0)[3], "1 step");
        let (glass, _) = compose(&Status::default(), &EN_GB, faces);
        assert_eq!(texts(&glass), ["set time", "‒‒:‒‒", "‒‒ %"]);
        let a_second_later = Status { clock: Some(Clock { h: 16, m: 27, s: 34 }), ..typical() };
        assert_eq!(compose(&typical(), &EN_GB, faces).0, compose(&a_second_later, &EN_GB, faces).0);
    }
}

#[test]
fn the_pages_read_right() {
    for faces in FAMILIES {
        let battery = Battery { soc_pct: Some(87), cell_mv: Some(4012), external: Some(false) };
        let (glass, _) = compose_battery(&battery, &EN_GB, faces);
        assert_eq!(texts(&glass), ["BATTERY", "87 %", "4012 mV", "input cell"]);
        assert_eq!(glass.lines().nth(1).unwrap().font.face.px, faces.charge.face.px);
        let (glass, _) = compose_battery(&Battery::default(), &EN_GB, faces);
        assert_eq!(texts(&glass), ["BATTERY", "‒‒ %", "‒‒‒‒ mV", "input ?"]);
        let (glass, _) = compose_about(&About { version: "0.1.0", build: "5784ec72019b" }, &EN_GB, faces);
        assert_eq!(texts(&glass), ["ABOUT", "wrist-rs", "0.1.0", "5784EC72019B"]);
        assert_eq!(glass.lines().nth(3).unwrap().font.face.px, faces.id.face.px);
        let (glass, _) = compose_about(&About { version: "0.1.0", build: "5784ec72019b-dirty" }, &EN_GB, faces);
        assert_eq!(texts(&glass), ["ABOUT", "wrist-rs", "0.1.0", "5784EC72019B", "DIRTY"]);
        let (glass, _) = compose_about(&About { version: "0.1.0", build: "gate-test" }, &EN_GB, faces);
        assert_eq!(texts(&glass), ["ABOUT", "wrist-rs", "0.1.0", "GATE-TEST"]);
    }
}

#[test]
fn a_version_too_long_is_cut_and_says_so() {
    for faces in FAMILIES {
        let (glass, short) = compose_about(&About { version: LONG_VERSION, build: "5784ec72019b" }, &EN_GB, faces);
        let shown = texts(&glass)[2];
        assert!(shown.len() > 5 && shown.len() < LONG_VERSION.len() && LONG_VERSION.starts_with(shown), "{shown}");
        assert_eq!(short, 1 << 2);
    }
}

#[test]
fn a_build_name_too_long_is_cut_and_says_so() {
    for faces in FAMILIES {
        let (glass, short) =
            compose_about(&About { version: "0.1.0", build: "a-name-longer-than-two-lines-of-the-page" }, &EN_GB, faces);
        let (id, suffix) = (texts(&glass)[3], texts(&glass)[4]);
        assert_eq!(id, "A-NAME-LONGE");
        assert!(suffix.len() > 5 && suffix.len() < 28 && "R-THAN-TWO-LINES-OF-THE-PAGE".starts_with(suffix), "{suffix}");
        assert_eq!(short, 1 << 4);
    }
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
    let opts = Options { scale: 2, bezel: Bezel::Fill([40, 40, 40]) };
    for (name, (glass, _)) in every_page() {
        let mut fb = vec![BLACK; W * H];
        assert!(render(&mut fb, &glass));
        let rgba = to_rgba(&fb, WIDTH, HEIGHT, opts);
        let path = dir.join(format!("{name}.png"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_png(path, &rgba, WIDTH * 2, HEIGHT * 2).unwrap();
    }
    eprintln!("previews in {}", dir.display());
}
