//! The watch face, composed from a [`Status`].

use inscribed_disc::{Abgr2222, Surface};

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

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

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
pub fn compose(s: &Status) -> (Glass, Short) {
    let mut b = Builder::new();
    b.at::<FACE_DATE_TOP, 3, FACE_DATE_W>(|l| match s.date {
        Some(d) => push_date(l, d),
        None => l.push("set time"),
    });
    b.at::<FACE_TIME_TOP, FACE_TIME_SCALE, FACE_TIME_W>(|l| match s.clock {
        Some(c) => push_hm(l, c),
        None => l.push("--:--"),
    });
    b.at::<FACE_BATT_TOP, 3, FACE_BATT_W>(|l| {
        match s.soc_pct {
            Some(pct) => l.push_u32(u32::from(pct))?,
            None => l.push("--")?,
        }
        l.push(" %")
    });
    (b.glass, b.short)
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
    line.push(":")?;
    line.push_two_digits(c.m)
}

fn push_date<const N: usize>(line: &mut Line<N>, d: Date) -> Result<(), Overflow> {
    let named = (1..=12).contains(&d.month) && (1..=31).contains(&d.day);
    if !named {
        return line.push("?");
    }
    line.push(WEEKDAYS[d.weekday() as usize % 7])?;
    line.push(" ")?;
    line.push_u32(u32::from(d.day))?;
    line.push(" ")?;
    line.push(MONTHS[usize::from(d.month - 1)])
}
