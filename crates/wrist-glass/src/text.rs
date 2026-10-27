//! Lines of text, placed where the disc can show them; a number that changes in place is set in
//! figures of one width, so what did not change stays put.

use core::ops::Range;

use embedded_graphics::draw_target::DrawTarget;
use incised_text::{chars, Face, Style};
use inscribed_disc::geometry::lit_start;
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

/// A digit's width, by Unicode's definition.
const FIGURE_DASH: char = '\u{2012}';
const MINUS: char = '\u{2212}';

/// Text, cut only between characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    buf: [u8; LINE_BYTES],
    len: u8,
    /// Bit `i` set when byte `i` is a digit of a reading or the time.
    figures: u32,
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl Line {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; LINE_BYTES], len: 0, figures: 0 }
    }

    /// Words and numbers come only through [`wrist_l10n::Sink`].
    fn push(&mut self, s: &str, figures: bool) -> Result<(), Overflow> {
        for c in s.chars() {
            let at = usize::from(self.len);
            let dst = self.buf.get_mut(at..at + c.len_utf8()).ok_or(Overflow)?;
            self.len += u8::try_from(c.encode_utf8(dst).len()).map_err(|_| Overflow)?;
            if figures {
                self.figures |= 1 << at;
            }
        }
        Ok(())
    }

    /// Data that does not change while it is shown, in the face's own widths.
    ///
    /// # Errors
    /// [`Overflow`] when it did not all fit; the characters that fitted are kept.
    pub fn push_verbatim(&mut self, s: &str) -> Result<(), Overflow> {
        self.push(s, false)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..usize::from(self.len)]).unwrap_or("")
    }

    fn pop(&mut self) {
        if let Some((i, _)) = self.as_str().char_indices().next_back() {
            self.len = u8::try_from(i).unwrap_or(0);
            self.figures &= (1 << i) - 1;
        }
    }

    /// Each character as the face looks it up, and whether it takes a figure's cell.
    fn chars(&self) -> impl Iterator<Item = (char, bool)> + '_ {
        let mut source = self.as_str().char_indices();
        chars(self.as_str()).map(move |c| {
            let (i, first) = source.next().unwrap_or((0, c));
            if first != c {
                source.next();
            }
            let digit = c.is_ascii_digit() && self.figures & (1 << i) != 0;
            (c, digit || c == FIGURE_DASH)
        })
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
        self.push(s, false)
    }

    fn number(&mut self, n: u32) -> Result<(), Overflow> {
        let mut digits = [0u8; 11];
        self.push(fmt_u32(&mut digits, n), true)
    }

    fn two_digits(&mut self, n: u8) -> Result<(), Overflow> {
        let n = n % 100;
        let digits = [b'0' + n / 10, b'0' + n % 10];
        self.push(core::str::from_utf8(&digits).unwrap_or("??"), true)
    }

    fn day(&mut self, d: u8) -> Result<(), Overflow> {
        let mut digits = [0u8; 11];
        self.push(fmt_u32(&mut digits, u32::from(d)), false)
    }
}

/// One size of type, and the face whose digits are its figures.
#[derive(Clone, Copy)]
pub struct Font {
    pub face: &'static Face,
    pub figures: &'static Face,
}

impl Font {
    #[must_use]
    pub const fn new(face: &'static Face) -> Self {
        Self { face, figures: face }
    }

    fn same(&self, other: &Self) -> bool {
        core::ptr::eq(self.face, other.face) && core::ptr::eq(self.figures, other.figures)
    }

    /// The face that draws `c`, and the character it draws: a figure dash the face lacks as its
    /// minus sign, which sits where figures do.
    pub(crate) fn source(&self, c: char, figure: bool) -> (&'static Face, char) {
        if figure && self.figures.glyph(c).is_some() {
            (self.figures, c)
        } else if c == FIGURE_DASH && self.face.glyph(c).is_none() {
            (self.face, if self.face.glyph(MINUS).is_some() { MINUS } else { '-' })
        } else {
            (self.face, c)
        }
    }

    /// `Face::measure` of `c` alone, read straight from its glyph when it has one.
    fn measure(&self, c: char, figure: bool) -> Metrics {
        let (face, c) = self.source(c, figure);
        match face.glyph(c) {
            Some(n) if n.w > 0 => {
                let (left, top) = (i32::from(n.left), -i32::from(n.top));
                Metrics {
                    advance: i32::from(n.adv),
                    ink_left: left,
                    ink_right: left + i32::from(n.w),
                    ink_top: top,
                    ink_bottom: top + i32::from(n.h),
                }
            }
            Some(n) => Metrics { advance: i32::from(n.adv), ..Metrics::default() },
            None => {
                let mut buf = [0u8; 4];
                face.measure(c.encode_utf8(&mut buf)).into()
            }
        }
    }

    /// A figure's cell: the widest of the figures' digits.
    fn figure_width(&self) -> i32 {
        ('0'..='9').filter_map(|c| self.figures.glyph(c)).map(|n| i32::from(n.adv)).max().unwrap_or(0)
    }

    /// The characters of `text` that would draw as a missing box.
    pub fn uncovered<'a>(&'a self, text: &'a str) -> impl Iterator<Item = char> + 'a {
        chars(text).filter(move |&c| {
            let (face, drawn) = self.source(c, false);
            face.glyph(drawn).is_none()
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Metrics {
    advance: i32,
    ink_left: i32,
    ink_right: i32,
    ink_top: i32,
    ink_bottom: i32,
}

impl From<incised_text::Measure> for Metrics {
    fn from(m: incised_text::Measure) -> Self {
        let incised_text::Measure { advance, ink_left, ink_right, ink_top, ink_bottom, .. } = m;
        Self { advance, ink_left, ink_right, ink_top, ink_bottom }
    }
}

/// One character of a line where it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub ch: char,
    /// Where its cell starts; every figure's cell is as wide.
    pub pen: i32,
    pub advance: i32,
    /// Its inked columns; empty for a character with no ink.
    pub ink: (i32, i32),
    /// Where its face draws it from.
    pub(crate) x: i32,
    pub(crate) figure: bool,
}

/// Each character of `line` in `font`, its advance centred on the panel.
fn layout<'a>(font: &'a Font, line: &'a Line) -> impl Iterator<Item = Glyph> + 'a {
    let cell = font.figure_width();
    let cells = move || {
        line.chars().map(move |(ch, figure)| {
            let m = font.measure(ch, figure);
            (ch, figure, m, if figure { cell } else { m.advance })
        })
    };
    let mut pen = CENTRE - cells().map(|(.., a)| a).sum::<i32>() / 2;
    cells().map(move |(ch, figure, m, advance)| {
        let x = if figure { pen + (advance - m.advance) / 2 } else { pen };
        let ink = if m.ink_right > m.ink_left { (x + m.ink_left, x + m.ink_right) } else { (x, x) };
        let g = Glyph { ch, pen, advance, ink, x, figure };
        pen += advance;
        g
    })
}

/// The rows `line`'s ink takes in `font`, from the baseline.
fn ink_rows(font: &Font, line: &Line) -> Range<i32> {
    line.chars()
        .map(|(c, figure)| font.measure(c, figure))
        .filter(|m| m.ink_right > m.ink_left)
        .map(|m| m.ink_top..m.ink_bottom)
        .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
        .unwrap_or(0..0)
}

/// [`inscribed_disc::geometry::band_chord`] without a scan: chords narrow away from the centre,
/// so a band's narrowest is at one of its ends.
fn narrowest_chord(rows: Range<i32>) -> i32 {
    let chord = |y: i32| (W - 2 * lit_start(y, W, H)).max(0);
    if rows.is_empty() {
        0
    } else {
        chord(rows.start).min(chord(rows.end - 1))
    }
}

/// Whether `line` in `font`, centred on `baseline`, keeps all its ink [`MARGIN`] inside the glass
/// on every row the ink takes.
#[must_use]
pub fn fits(font: &Font, baseline: i32, line: &Line) -> bool {
    let Some((ink_left, ink_right)) =
        layout(font, line).map(|g| g.ink).filter(|i| i.1 > i.0).reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    else {
        return true;
    };
    let rows = ink_rows(font, line);
    let chord = narrowest_chord(baseline + rows.start..baseline + rows.end) - 2 * MARGIN;
    let left = (W - chord) / 2;
    chord > 0 && ink_left >= left && ink_right <= left + chord
}

pub const MAX_LINES: usize = 8;

/// A line as drawn: its font, its baseline, and its advance centred on the panel.
#[derive(Clone, Copy)]
pub struct Placed {
    pub font: Font,
    pub baseline: i32,
    line: Line,
}

impl PartialEq for Placed {
    fn eq(&self, other: &Self) -> bool {
        self.font.same(&other.font) && self.baseline == other.baseline && self.line == other.line
    }
}

impl Eq for Placed {}

impl core::fmt::Debug for Placed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Placed")
            .field("face", &self.font.face.px)
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
    pub fn ink_rows(&self) -> Range<i32> {
        let rows = ink_rows(&self.font, &self.line);
        self.baseline + rows.start..self.baseline + rows.end
    }

    /// Each character where it is drawn.
    pub fn glyphs(&self) -> impl Iterator<Item = Glyph> + '_ {
        layout(&self.font, &self.line)
    }

    /// Draws the characters whose ink meets `columns`.
    fn draw<D: DrawTarget<Color = Abgr2222>>(&self, target: &mut D, columns: &Range<i32>, ink: Abgr2222) {
        let style = Style::left(ink, Abgr2222::BLACK);
        for g in self.glyphs().filter(|g| g.ink.0 < columns.end && g.ink.1 > columns.start) {
            let (face, c) = self.font.source(g.ch, g.figure);
            let mut buf = [0u8; 4];
            let _ = face.draw(target, c.encode_utf8(&mut buf), g.x, self.baseline, style);
        }
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

    /// Centres `line` in `font` on `baseline`, less the characters at its end that would not
    /// [`fit`](fits); a line past [`MAX_LINES`] is dropped.
    ///
    /// # Errors
    /// [`Overflow`] when a character was dropped.
    pub fn put(&mut self, font: Font, baseline: i32, line: &Line) -> Result<(), Overflow> {
        let mut line = *line;
        let mut cut = Ok(());
        while !fits(&font, baseline, &line) {
            line.pop();
            cut = Err(Overflow);
        }
        if let Some(slot) = self.lines.get_mut(self.n) {
            *slot = Some(Placed { font, baseline, line });
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
    /// the characters that changed or moved in a line that kept its font and baseline, the ink of
    /// both versions of any other line, and every column without `before`.
    #[must_use]
    pub fn changed_columns(&self, before: Option<&Self>) -> [bool; WIDTH as usize] {
        crate::scene::Scene::of(self).changed_columns(before.map(crate::scene::Scene::of).as_ref())
    }

    pub fn draw_with(&self, surface: &mut Surface<'_, Abgr2222>, ink: Abgr2222) {
        for i in 0..self.n {
            self.draw_line_with(surface, i, ink);
        }
    }

    /// Nothing for an `i` past the last line.
    pub fn draw_line_with(&self, surface: &mut Surface<'_, Abgr2222>, i: usize, ink: Abgr2222) {
        if let Some(Some(l)) = self.lines.get(i) {
            l.draw(surface, &(0..W), ink);
        }
    }
}

/// The runs of marked columns, cut to at most `widest` columns each, left to right.
pub fn strips(marked: &[bool; WIDTH as usize], widest: usize) -> impl Iterator<Item = Range<i32>> + '_ {
    let widest = widest.max(1);
    let mut x = 0;
    core::iter::from_fn(move || {
        while x < marked.len() && !marked[x] {
            x += 1;
        }
        let start = x;
        while x < marked.len() && marked[x] && x - start < widest {
            x += 1;
        }
        let as_i32 = |v: usize| i32::try_from(v).unwrap_or(W);
        (x > start).then(|| as_i32(start)..as_i32(x))
    })
}

#[cfg(test)]
mod tests {
    use inscribed_disc::geometry::band_chord;

    use super::{narrowest_chord, Metrics, H, W};
    use crate::pages::{ATKINSON, POPPINS};

    #[test]
    fn a_character_measures_as_its_face_measures_it() {
        let fonts = [&POPPINS, &ATKINSON].into_iter().flat_map(|f| [f.words, f.id, f.charge, f.time]);
        for font in fonts {
            for c in (0..0x2400).filter_map(char::from_u32) {
                for figure in [false, true] {
                    let (face, drawn) = font.source(c, figure);
                    let mut buf = [0u8; 4];
                    let want = Metrics::from(face.measure(drawn.encode_utf8(&mut buf)));
                    assert_eq!(font.measure(c, figure), want, "{} px {c:?}", font.face.px);
                }
            }
        }
    }

    #[test]
    fn the_narrowest_chord_is_the_band_chord() {
        for y0 in -20..H + 20 {
            for y1 in y0 - 2..H + 20 {
                assert_eq!(narrowest_chord(y0..y1), band_chord(y0, y1, W, H), "{y0}..{y1}");
            }
        }
    }
}
