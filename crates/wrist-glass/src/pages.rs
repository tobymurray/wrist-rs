//! The watch face, composed from a [`Status`].

use inscribed_disc::{Abgr2222, Surface};

use wrist_l10n::Msg;

use crate::text::{room_at, Glass, Line, Overflow};
use crate::{ring, HEIGHT, WIDTH};

pub use wrist_l10n::{Clock, Date, Locale, BUILT_IN, EN_GB};

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
const FACE_DATE_W: usize = room_at(FACE_DATE_TOP, 3);
const FACE_TIME_TOP: i32 = 94;
const FACE_TIME_SCALE: u32 = 7;
const FACE_TIME_W: usize = room_at(FACE_TIME_TOP, FACE_TIME_SCALE);
const FACE_BATT_TOP: i32 = 152;
const FACE_BATT_W: usize = room_at(FACE_BATT_TOP, 3);

#[must_use]
pub fn compose(status: &Status, locale: &Locale) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<FACE_DATE_TOP, 3, FACE_DATE_W>(|l| match status.date {
        Some(d) if d.named() => locale.date(l, d),
        Some(_) => locale.message(l, Msg::FaceDateInvalid),
        None => locale.message(l, Msg::FaceSetTime),
    });
    b.at::<FACE_TIME_TOP, FACE_TIME_SCALE, FACE_TIME_W>(|l| match status.clock {
        Some(c) => locale.time(l, c),
        None => locale.message(l, Msg::FaceTimeUnset),
    });
    b.at::<FACE_BATT_TOP, 3, FACE_BATT_W>(|l| push_percent(l, locale, status.soc_pct));
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
const TITLE_W: usize = room_at(TITLE_TOP, 3);
const BATT_PCT_TOP: i32 = 88;
const BATT_PCT_SCALE: u32 = 5;
const BATT_PCT_W: usize = room_at(BATT_PCT_TOP, BATT_PCT_SCALE);
const BATT_MV_TOP: i32 = 136;
const BATT_MV_W: usize = room_at(BATT_MV_TOP, 3);
const BATT_INPUT_TOP: i32 = 164;
const BATT_INPUT_W: usize = room_at(BATT_INPUT_TOP, 3);
const ABOUT_NAME_TOP: i32 = 88;
const ABOUT_NAME_W: usize = room_at(ABOUT_NAME_TOP, 3);
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
pub fn compose_battery(battery: &Battery, locale: &Locale) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<TITLE_TOP, 3, TITLE_W>(|l| locale.message(l, Msg::BatteryTitle));
    b.at::<BATT_PCT_TOP, BATT_PCT_SCALE, BATT_PCT_W>(|l| push_percent(l, locale, battery.soc_pct));
    b.at::<BATT_MV_TOP, 3, BATT_MV_W>(|l| match battery.cell_mv {
        Some(mv) => locale.message(l, Msg::BatteryCellMv { mv }),
        None => locale.message(l, Msg::BatteryCellUnset),
    });
    b.at::<BATT_INPUT_TOP, 3, BATT_INPUT_W>(|l| {
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
pub fn compose_about(a: &About<'_>, locale: &Locale) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<TITLE_TOP, 3, TITLE_W>(|l| locale.message(l, Msg::AboutTitle));
    b.at::<ABOUT_NAME_TOP, 3, ABOUT_NAME_W>(|l| locale.message(l, Msg::AboutProduct));
    b.at::<ABOUT_VERSION_TOP, 3, ABOUT_VERSION_W>(|l| l.push_verbatim(a.version.as_bytes()));
    let build = a.build.as_bytes();
    let (id, rest) = build.split_at(build.len().min(ABOUT_BUILD_W));
    b.at::<ABOUT_BUILD_TOP, 2, ABOUT_BUILD_W>(|l| push_upper(l, id));
    if !rest.is_empty() {
        b.at::<ABOUT_DIRTY_TOP, 2, ABOUT_DIRTY_W>(|l| push_upper(l, rest.strip_prefix(b"-").unwrap_or(rest)));
    }
    (b.glass, b.short)
}

fn push_upper<const N: usize>(line: &mut Line<N>, s: &[u8]) -> Result<(), Overflow> {
    let mut buf = [0u8; N];
    let n = s.len().min(N);
    for (dst, src) in buf.iter_mut().zip(s) {
        *dst = src.to_ascii_uppercase();
    }
    line.push_verbatim(&buf[..n])?;
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

fn push_percent<const N: usize>(line: &mut Line<N>, locale: &Locale, pct: Option<u8>) -> Result<(), Overflow> {
    match pct {
        Some(pct) => locale.message(line, Msg::ChargePercent { pct: u32::from(pct) }),
        None => locale.message(line, Msg::ChargeUnset),
    }
}
