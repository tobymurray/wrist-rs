//! The watch face, composed from a [`Status`].

use inscribed_disc::{Abgr2222, Surface};

use crate::strings as s;
use crate::text::{Glass, Line, Overflow};
use crate::{ring, HEIGHT, WIDTH};

/// A 24-hour time of day; any values print, modulo 100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    pub h: u8,
    pub m: u8,
    pub s: u8,
}

/// A month outside 1 to 12 or a day outside 1 to 31 prints as `?`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// 0 Monday to 6 Sunday, by Sakamoto's method.
    #[must_use]
    pub const fn weekday(self) -> u8 {
        const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let m = if self.month >= 1 && self.month <= 12 { self.month } else { 1 };
        let y = if m < 3 { self.year.saturating_sub(1) } else { self.year } as u32;
        let sunday0 = (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] as u32 + self.day as u32) % 7;
        ((sunday0 + 6) % 7) as u8
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` until the clock is set, as is `date`.
    pub clock: Option<Clock>,
    pub date: Option<Date>,
    /// Whole percent; any value.
    pub soc_pct: Option<u8>,
}

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

    fn note(&mut self, r: Result<(), Overflow>) {
        if r.is_err() {
            self.short |= 1 << self.n;
        }
        self.n += 1;
    }

    fn at<const TOP: i32, const SCALE: u32, const N: usize>(
        &mut self,
        f: impl FnOnce(&mut Line<N>) -> Result<(), Overflow>,
    ) {
        let mut line = Line::<N>::new();
        let r = f(&mut line);
        self.glass.put::<TOP, SCALE, N>(&line);
        self.note(r);
    }
}

const FACE_DATE_TOP: i32 = 66;
/// `Sat 17 Oct`.
const FACE_DATE_W: usize = 10;
const FACE_TIME_TOP: i32 = 94;
const FACE_TIME_SCALE: u32 = 7;
/// `23:59`.
const FACE_TIME_W: usize = 5;
const FACE_BATT_TOP: i32 = 152;
/// `255 %`.
const FACE_BATT_W: usize = 5;

#[must_use]
pub fn compose(status: &Status) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<FACE_DATE_TOP, 3, FACE_DATE_W>(|l| match status.date {
        Some(d) => push_date(l, d),
        None => l.push(s::SET_TIME),
    });
    b.at::<FACE_TIME_TOP, FACE_TIME_SCALE, FACE_TIME_W>(|l| match status.clock {
        Some(c) => push_hm(l, c),
        None => l.push(s::TIME_UNSET),
    });
    b.at::<FACE_BATT_TOP, 3, FACE_BATT_W>(|l| push_percent(l, status.soc_pct));
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

const TITLE_TOP: i32 = 58;
/// `BATTERY`.
const TITLE_W: usize = 7;
const BATT_PCT_TOP: i32 = 88;
const BATT_PCT_SCALE: u32 = 5;
const BATT_MV_TOP: i32 = 136;
/// `65535 mV`.
const BATT_MV_W: usize = 8;
const BATT_INPUT_TOP: i32 = 164;
/// `input cell`.
const BATT_INPUT_W: usize = 10;
const ABOUT_NAME_TOP: i32 = 88;
/// `wrist-rs`.
const ABOUT_NAME_W: usize = 8;
const ABOUT_VERSION_TOP: i32 = 116;
/// `100.100.100`.
const ABOUT_VERSION_W: usize = 11;
const ABOUT_BUILD_TOP: i32 = 146;
/// Twelve hex digits.
const ABOUT_BUILD_W: usize = 12;
const ABOUT_DIRTY_TOP: i32 = 166;
const ABOUT_DIRTY_W: usize = 12;

/// The charge, the cell's voltage and the input.
#[must_use]
pub fn compose_battery(battery: &Battery) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<TITLE_TOP, 3, TITLE_W>(|l| l.push(s::BATTERY_TITLE));
    b.at::<BATT_PCT_TOP, BATT_PCT_SCALE, FACE_BATT_W>(|l| push_percent(l, battery.soc_pct));
    b.at::<BATT_MV_TOP, 3, BATT_MV_W>(|l| {
        match battery.cell_mv {
            Some(mv) => l.push_u32(mv)?,
            None => l.push(s::UNSET)?,
        }
        l.push(s::MILLIVOLT_SUFFIX)
    });
    b.at::<BATT_INPUT_TOP, 3, BATT_INPUT_W>(|l| {
        l.push(match battery.external {
            Some(true) => s::INPUT_USB,
            Some(false) => s::INPUT_CELL,
            None => s::INPUT_UNKNOWN,
        })
    });
    (b.glass, b.short)
}

/// The version and the build ID, upper case since the font has no small `b`; a suffix after
/// the ID's first twelve characters goes on a line of its own.
#[must_use]
pub fn compose_about(a: &About<'_>) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<TITLE_TOP, 3, TITLE_W>(|l| l.push(s::ABOUT_TITLE));
    b.at::<ABOUT_NAME_TOP, 3, ABOUT_NAME_W>(|l| l.push(s::PRODUCT));
    b.at::<ABOUT_VERSION_TOP, 3, ABOUT_VERSION_W>(|l| l.push(a.version));
    let build = a.build.as_bytes();
    let (id, rest) = build.split_at(build.len().min(ABOUT_BUILD_W));
    b.at::<ABOUT_BUILD_TOP, 2, ABOUT_BUILD_W>(|l| push_upper(l, id));
    if !rest.is_empty() {
        b.at::<ABOUT_DIRTY_TOP, 2, ABOUT_DIRTY_W>(|l| push_upper(l, rest.strip_prefix(b"-").unwrap_or(rest))); // verbatim: the build ID's own separator
    }
    (b.glass, b.short)
}

fn push_upper<const N: usize>(line: &mut Line<N>, s: &[u8]) -> Result<(), Overflow> {
    let mut buf = [0u8; N];
    let n = s.len().min(N);
    for (dst, src) in buf.iter_mut().zip(s) {
        *dst = src.to_ascii_uppercase();
    }
    line.push_bytes(&buf[..n])?;
    if s.len() > N {
        return Err(Overflow { needed: s.len() });
    }
    Ok(())
}

/// False, drawing nothing, when `fb` is not `WIDTH * HEIGHT` bytes.
pub fn render(fb: &mut [u8], glass: &Glass) -> bool {
    if !background(fb) {
        return false;
    }
    (0..glass.lines().len()).all(|i| draw_line(fb, glass, i))
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

/// `hh:mm`, each field modulo 100.
///
/// # Errors
/// `Overflow` when the line has room for fewer than five glyphs.
pub fn push_hm<const N: usize>(line: &mut Line<N>, c: Clock) -> Result<(), Overflow> {
    line.push_two_digits(c.h)?;
    line.push(s::TIME_SEPARATOR)?;
    line.push_two_digits(c.m)
}

fn push_date<const N: usize>(line: &mut Line<N>, d: Date) -> Result<(), Overflow> {
    let named = (1..=12).contains(&d.month) && (1..=31).contains(&d.day);
    if !named {
        return line.push(s::DATE_INVALID);
    }
    line.push(s::WEEKDAYS[d.weekday() as usize % 7])?;
    line.push(s::DATE_GAP)?;
    line.push_u32(u32::from(d.day))?;
    line.push(s::DATE_GAP)?;
    line.push(s::MONTHS[usize::from(d.month - 1)])
}

/// `100 %`, or `-- %` before the gauge has a reading.
fn push_percent<const N: usize>(line: &mut Line<N>, pct: Option<u8>) -> Result<(), Overflow> {
    match pct {
        Some(pct) => line.push_u32(u32::from(pct))?,
        None => line.push(s::UNSET)?,
    }
    line.push(s::PERCENT_SUFFIX)
}
