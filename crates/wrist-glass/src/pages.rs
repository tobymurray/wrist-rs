//! The watch face, composed from a [`Status`].

use core::ops::Range;

use incised_text::faces;
use inscribed_disc::{Abgr2222, Surface};

use wrist_l10n::Msg;

use crate::scene::Scene;
use crate::text::{Font, Glass, Line, Overflow};
use crate::{ring, HEIGHT, RING_COLOR, RING_RUNS, WIDTH};

#[allow(clippy::cast_possible_wrap)]
const W: i32 = WIDTH as i32;
#[allow(clippy::cast_possible_wrap)]
const H: i32 = HEIGHT as i32;

pub use wrist_l10n::{Clock, Date, Locale, BUILT_IN, EN_GB};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` until the clock is set, as is `date`.
    pub clock: Option<Clock>,
    pub date: Option<Date>,
    /// Whole percent; any value.
    pub soc_pct: Option<u8>,
}

/// One typeface's font for each kind of line.
pub struct Faces {
    pub name: &'static str,
    pub words: Font,
    pub id: Font,
    pub charge: Font,
    pub time: Font,
}

pub const POPPINS: Faces = Faces {
    name: "poppins",
    words: Font::new(&faces::POPPINS_SEMIBOLD_26_LATIN),
    id: Font::new(&faces::POPPINS_SEMIBOLD_20_ASCII),
    charge: Font::new(&faces::POPPINS_SEMIBOLD_47_NUMERIC),
    time: Font::new(&faces::POPPINS_SEMIBOLD_67_NUMERIC),
};

pub const ATKINSON: Faces = Faces {
    name: "atkinson",
    words: Font { face: &faces::ATKINSON_BOLD_28_LATIN, figures: &faces::ATKINSON_BOLD_28_FIGURES },
    id: Font::new(&faces::ATKINSON_BOLD_20_ASCII),
    charge: Font::new(&faces::ATKINSON_BOLD_52_NUMERIC),
    time: Font::new(&faces::ATKINSON_BOLD_72_NUMERIC),
};

pub const FACES: &Faces = &ATKINSON;

/// Bit `i` set when line `i` of a [`compose`] was cut; 0 when none was.
pub type Short = u8;

struct Builder {
    glass: Glass,
    short: Short,
    n: u8,
}

impl Builder {
    fn new() -> Self {
        Self { glass: Glass::new(), short: 0, n: 0 }
    }

    fn at(&mut self, font: Font, baseline: i32, f: impl FnOnce(&mut Line) -> Result<(), Overflow>) {
        let mut line = Line::new();
        let pushed = f(&mut line);
        let placed = self.glass.put(font, baseline, &line);
        if pushed.and(placed).is_err() {
            self.short |= 1 << self.n;
        }
        self.n += 1;
    }
}

const FACE_DATE: i32 = 84;
const FACE_TIME: i32 = 143;
const FACE_BATT: i32 = 175;

#[must_use]
pub fn compose(status: &Status, locale: &Locale, faces: &Faces) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at(faces.words, FACE_DATE, |l| match status.date {
        Some(d) if d.named() => locale.date(l, d),
        Some(_) => locale.message(l, Msg::FaceDateInvalid),
        None => locale.message(l, Msg::FaceSetTime),
    });
    b.at(faces.time, FACE_TIME, |l| match status.clock {
        Some(c) => locale.time(l, c),
        None => locale.message(l, Msg::FaceTimeUnset),
    });
    b.at(faces.words, FACE_BATT, |l| push_percent(l, locale, status.soc_pct));
    (b.glass, b.short)
}

/// What the battery page shows; `None` for a reading not yet made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Battery {
    pub soc_pct: Option<u8>,
    pub cell_mv: Option<u32>,
    /// True on USB or another external input, false on the cell.
    pub external: Option<bool>,
}

/// What the about page shows: the image's version and build ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct About<'a> {
    pub version: &'a str,
    /// 12 hex digits, then `-dirty` for an uncommitted tree, or a name a build was given.
    pub build: &'a str,
}

const TITLE: i32 = 79;
const BATT_PCT: i32 = 123;
const BATT_MV: i32 = 157;
const BATT_INPUT: i32 = 185;
const ABOUT_NAME: i32 = 109;
const ABOUT_VERSION: i32 = 137;
const ABOUT_BUILD: i32 = 160;
const ABOUT_SUFFIX: i32 = 180;
/// Twelve hex digits.
const ID_CHARS: usize = 12;

/// The charge, the cell's voltage and the input.
#[must_use]
pub fn compose_battery(battery: &Battery, locale: &Locale, faces: &Faces) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at(faces.words, TITLE, |l| locale.message(l, Msg::BatteryTitle));
    b.at(faces.charge, BATT_PCT, |l| push_percent(l, locale, battery.soc_pct));
    b.at(faces.words, BATT_MV, |l| match battery.cell_mv {
        Some(mv) => locale.message(l, Msg::BatteryCellMv { mv }),
        None => locale.message(l, Msg::BatteryCellUnset),
    });
    b.at(faces.words, BATT_INPUT, |l| {
        locale.message(
            l,
            match battery.external {
                Some(true) => Msg::BatteryInputUsb,
                Some(false) => Msg::BatteryInputCell,
                None => Msg::BatteryInputUnknown,
            },
        )
    });
    (b.glass, b.short)
}

/// The version, and the build ID in upper case; a suffix after the ID's first twelve characters
/// goes on a line of its own.
#[must_use]
pub fn compose_about(a: &About<'_>, locale: &Locale, faces: &Faces) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at(faces.words, TITLE, |l| locale.message(l, Msg::AboutTitle));
    b.at(faces.words, ABOUT_NAME, |l| locale.message(l, Msg::AboutProduct));
    b.at(faces.words, ABOUT_VERSION, |l| l.push_verbatim(a.version));
    let split = a.build.char_indices().nth(ID_CHARS).map_or(a.build.len(), |(i, _)| i);
    let (id, rest) = a.build.split_at(split);
    b.at(faces.id, ABOUT_BUILD, |l| push_upper(l, id));
    if !rest.is_empty() {
        b.at(faces.id, ABOUT_SUFFIX, |l| push_upper(l, rest.strip_prefix('-').unwrap_or(rest)));
    }
    (b.glass, b.short)
}

fn push_upper(line: &mut Line, s: &str) -> Result<(), Overflow> {
    for c in s.chars() {
        let mut buf = [0u8; 4];
        line.push_verbatim(c.to_ascii_uppercase().encode_utf8(&mut buf))?;
    }
    Ok(())
}

/// False, drawing nothing, when `fb` is not `WIDTH * HEIGHT` bytes.
pub fn render(fb: &mut [u8], glass: &Glass) -> bool {
    if !background(fb) {
        return false;
    }
    (0..glass.len()).all(|i| draw_line(fb, glass, i))
}

/// [`background`] within `rows` and `columns` only, so no copy of it need be kept.
pub fn background_in(fb: &mut [u8], rows: Range<i32>, columns: Range<i32>) {
    let (rows, columns) = (rows.start.clamp(0, H)..rows.end.clamp(0, H), columns.start.clamp(0, W)..columns.end.clamp(0, W));
    if fb.len() != (WIDTH * HEIGHT) as usize || columns.is_empty() {
        return;
    }
    #[allow(clippy::cast_sign_loss)]
    let (a, z) = (columns.start as usize, columns.end as usize);
    #[allow(clippy::cast_sign_loss)]
    for y in rows.start as usize..rows.end as usize {
        let row = &mut fb[y * WIDTH as usize..(y + 1) * WIDTH as usize];
        row[a..z].fill(Abgr2222::BLACK.0);
        for &(from, to) in &RING_RUNS[y] {
            let (from, to) = (usize::from(from).max(a), usize::from(to).min(z));
            if from < to {
                row[from..to].fill(RING_COLOR.0);
            }
        }
    }
}

/// False for a buffer of the wrong size.
pub fn background(fb: &mut [u8]) -> bool {
    let Some(mut surface) = Surface::<Abgr2222>::round(fb, WIDTH, HEIGHT) else {
        return false;
    };
    surface.clear(Abgr2222::BLACK);
    ring(&mut surface);
    true
}

/// False for a buffer of the wrong size.
pub fn draw_line(fb: &mut [u8], glass: &Glass, i: usize) -> bool {
    let Some(mut surface) = Surface::<Abgr2222>::round(fb, WIDTH, HEIGHT) else {
        return false;
    };
    glass.draw_line_with(&mut surface, i, Abgr2222::WHITE);
    true
}

/// [`Scene::repaint`] in white over [`background`]; false for a buffer of the wrong size.
pub fn repaint(fb: &mut [u8], scene: &Scene, before: Option<&Scene>, columns: Range<i32>) -> bool {
    if fb.len() != (WIDTH * HEIGHT) as usize {
        return false;
    }
    scene.repaint(fb, background_in, before, columns, Abgr2222::WHITE);
    true
}

fn push_percent(line: &mut Line, locale: &Locale, pct: Option<u8>) -> Result<(), Overflow> {
    match pct {
        Some(pct) => locale.message(line, Msg::ChargePercent { pct: u32::from(pct) }),
        None => locale.message(line, Msg::ChargeUnset),
    }
}
