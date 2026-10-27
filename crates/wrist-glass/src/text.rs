//! Lines of text, placed where the disc can show them.

use core::ops::Range;

use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use incised_text::{Align, Face, Style};
use inscribed_disc::geometry::band_chord;
use inscribed_disc::{Abgr2222, Surface};

use crate::{HEIGHT, MARGIN, WIDTH};

/// The most UTF-8 bytes a line holds.
pub const LINE_BYTES: usize = 32;

#[allow(clippy::cast_possible_wrap)]
const W: i32 = WIDTH as i32;
#[allow(clippy::cast_possible_wrap)]
const H: i32 = HEIGHT as i32;
/// Where every line's advance is centred.
const CENTRE: i32 = W / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow;

/// Text, cut only between characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    buf: [u8; LINE_BYTES],
    len: u8,
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl Line {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; LINE_BYTES], len: 0 }
    }

    /// Words come only through [`wrist_l10n::Sink`].
    fn push(&mut self, s: &str) -> Result<(), Overflow> {
        for c in s.chars() {
            let at = usize::from(self.len);
            let dst = self.buf.get_mut(at..at + c.len_utf8()).ok_or(Overflow)?;
            self.len += u8::try_from(c.encode_utf8(dst).len()).map_err(|_| Overflow)?;
        }
        Ok(())
    }

    /// Data, not words.
    ///
    /// # Errors
    /// [`Overflow`] when it did not all fit; the characters that fitted are kept.
    pub fn push_verbatim(&mut self, s: &str) -> Result<(), Overflow> {
        self.push(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }

    fn pop(&mut self) {
        if let Some((i, _)) = self.as_str().char_indices().next_back() {
            self.len = u8::try_from(i).unwrap_or(0);
        }
    }
}

/// Writes right-aligned into `buf` and returns the used tail.
pub fn fmt_u32(buf: &mut [u8; 11], value: u32) -> &str {
    let mut n = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

impl wrist_l10n::Sink for Line {
    type Error = Overflow;

    fn text(&mut self, s: &str) -> Result<(), Overflow> {
        self.push(s)
    }

    fn number(&mut self, n: u32) -> Result<(), Overflow> {
        let mut digits = [0u8; 11];
        self.push(fmt_u32(&mut digits, n))
    }

    fn two_digits(&mut self, n: u8) -> Result<(), Overflow> {
        let n = n % 100;
        let digits = [b'0' + n / 10, b'0' + n % 10];
        self.push(core::str::from_utf8(&digits).unwrap_or("??"))
    }
}

/// Whether `text` in `face`, centred on `baseline`, keeps all its ink [`MARGIN`] inside the glass
/// on every row the ink takes.
#[must_use]
pub fn fits(face: &Face, baseline: i32, text: &str) -> bool {
    let m = face.measure(text);
    if m.ink_right <= m.ink_left {
        return true;
    }
    let chord = band_chord(baseline + m.ink_top, baseline + m.ink_bottom, W, H) - 2 * MARGIN;
    let left = (W - chord) / 2;
    let pen = CENTRE - m.advance / 2;
    chord > 0 && pen + m.ink_left >= left && pen + m.ink_right <= left + chord
}

pub const MAX_LINES: usize = 8;

/// A line as drawn: its face, its baseline, and its advance centred on the panel.
#[derive(Clone, Copy)]
pub struct Placed {
    pub face: &'static Face,
    pub baseline: i32,
    line: Line,
}

impl PartialEq for Placed {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.face, other.face) && self.baseline == other.baseline && self.line == other.line
    }
}

impl Eq for Placed {}

impl core::fmt::Debug for Placed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Placed")
            .field("face", &self.face.px)
            .field("baseline", &self.baseline)
            .field("line", &self.text())
            .finish()
    }
}

impl Placed {
    #[must_use]
    pub fn text(&self) -> &str {
        self.line.as_str()
    }

    /// The panel rows its ink takes.
    #[must_use]
    pub fn ink_rows(&self) -> core::ops::Range<i32> {
        let m = self.face.measure(self.text());
        self.baseline + m.ink_top..self.baseline + m.ink_bottom
    }

    fn layout(&self) -> impl Iterator<Item = incised_text::Placed> + '_ {
        self.face.layout(self.text(), CENTRE, Align::Center)
    }
}

/// Equal values draw the same pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glass {
    lines: [Option<Placed>; MAX_LINES],
    n: usize,
}

impl Default for Glass {
    fn default() -> Self {
        Self::new()
    }
}

impl Glass {
    #[must_use]
    pub const fn new() -> Self {
        Self { lines: [None; MAX_LINES], n: 0 }
    }

    /// Centres `line` in `face` on `baseline`, less the characters at its end that would not
    /// [`fit`](fits); a line past [`MAX_LINES`] is dropped.
    ///
    /// # Errors
    /// [`Overflow`] when a character was dropped.
    pub fn put(&mut self, face: &'static Face, baseline: i32, line: &Line) -> Result<(), Overflow> {
        let mut line = *line;
        let mut cut = Ok(());
        while !fits(face, baseline, line.as_str()) {
            line.pop();
            cut = Err(Overflow);
        }
        if let Some(slot) = self.lines.get_mut(self.n) {
            *slot = Some(Placed { face, baseline, line });
            self.n += 1;
        }
        cut
    }

    pub fn lines(&self) -> impl Iterator<Item = &Placed> + '_ {
        self.lines.iter().flatten()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.n
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The columns where this glass can differ from `before` over the same background: the ink of
    /// the characters that changed or moved in a line that kept its face and baseline, the ink of
    /// both versions of any other line, and every column without `before`.
    #[must_use]
    pub fn changed_columns(&self, before: Option<&Self>) -> [bool; WIDTH as usize] {
        let Some(before) = before else {
            return [true; WIDTH as usize];
        };
        let mut changed = [false; WIDTH as usize];
        let mut mark = |p: &incised_text::Placed| {
            for x in (p.ink_left..p.ink_right).filter_map(|x| usize::try_from(x).ok()) {
                if let Some(c) = changed.get_mut(x) {
                    *c = true;
                }
            }
        };
        for (a, b) in before.lines.iter().zip(&self.lines) {
            match (a, b) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) if core::ptr::eq(a.face, b.face) && a.baseline == b.baseline => {
                    let (mut old, mut new) = (a.layout(), b.layout());
                    loop {
                        match (old.next(), new.next()) {
                            (None, None) => break,
                            (Some(o), Some(n)) if (o.ch, o.pen) == (n.ch, n.pen) => {}
                            (o, n) => o.iter().chain(n.iter()).for_each(&mut mark),
                        }
                    }
                }
                (a, b) => {
                    for line in [a, b].into_iter().flatten() {
                        line.layout().for_each(|p| mark(&p));
                    }
                }
            }
        }
        changed
    }

    pub fn draw_with(&self, surface: &mut Surface<'_, Abgr2222>, ink: Abgr2222) {
        for i in 0..self.n {
            self.draw_line_with(surface, i, ink);
        }
    }

    /// Nothing for an `i` past the last line.
    pub fn draw_line_with(&self, surface: &mut Surface<'_, Abgr2222>, i: usize, ink: Abgr2222) {
        if let Some(Some(l)) = self.lines.get(i) {
            let _ = l.face.draw(surface, l.text(), CENTRE, l.baseline, Style::centered(ink, Abgr2222::BLACK));
        }
    }

    /// Bands of columns that tile the panel, split where each character of line `i` after the
    /// first starts, so the line can be drawn a character at a time.
    pub fn pieces(&self, i: usize) -> impl Iterator<Item = Range<i32>> + '_ {
        let pens = self.lines.get(i).and_then(Option::as_ref).into_iter().flat_map(|l| l.layout().skip(1));
        let mut start = 0;
        pens.map(|p| p.pen.clamp(0, W)).chain([W]).filter_map(move |end| {
            let band = start..end.max(start);
            start = band.end;
            (!band.is_empty()).then_some(band)
        })
    }

    /// The part of line `i` in `columns`; drawing every one of [`Glass::pieces`] draws the line.
    pub fn draw_piece_with(&self, surface: &mut Surface<'_, Abgr2222>, i: usize, columns: Range<i32>, ink: Abgr2222) {
        let width = u32::try_from(columns.end - columns.start).unwrap_or(0);
        let area = Rectangle::new(Point::new(columns.start, 0), Size::new(width, HEIGHT));
        if let Some(Some(l)) = self.lines.get(i) {
            let style = Style::centered(ink, Abgr2222::BLACK);
            let _ = l.face.draw(&mut surface.clipped(&area), l.text(), CENTRE, l.baseline, style);
        }
    }
}
