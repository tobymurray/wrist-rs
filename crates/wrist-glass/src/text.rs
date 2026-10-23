//! Lines of glyphs, placed where the disc can show them.

use inscribed_disc::geometry::band_chord;
use inscribed_disc::{Abgr2222, Surface};

use crate::font::{glyph, lit_pixels};
use crate::{HEIGHT, MARGIN, WIDTH};

pub const SCALE: u32 = 3;
pub const GLYPH_W: u32 = 5 * SCALE;
pub const GLYPH_H: u32 = 7 * SCALE;
pub const ADVANCE: u32 = GLYPH_W + SCALE;
pub const MAX_COLS: usize = ((WIDTH + SCALE) / ADVANCE) as usize;

const fn span(n: u32, scale: u32) -> u32 {
    if n == 0 {
        0
    } else {
        n * 6 * scale - scale
    }
}

/// How many glyphs at `scale` fit inside [`MARGIN`] on a line whose top is
/// `top`; 0 off the panel.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
#[must_use]
pub const fn capacity_at(top: i32, scale: u32) -> usize {
    let h = 7 * scale as i32;
    if scale == 0 || top < 0 || top + h > HEIGHT as i32 {
        return 0;
    }
    let chord = band_chord(top, top + h, WIDTH as i32, HEIGHT as i32) - 2 * MARGIN;
    if chord < 5 * scale as i32 {
        return 0;
    }
    ((chord as u32 + scale) / (6 * scale)) as usize
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow {
    /// The whole content's length.
    pub needed: usize,
}

/// At most `N` glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Default for Line<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Line<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [b' '; N],
            len: 0,
        }
    }

    /// # Errors
    /// [`Overflow`] when it did not all fit; what fitted is kept.
    pub fn push(&mut self, s: &str) -> Result<(), Overflow> {
        push_into(&mut self.buf, &mut self.len, s.as_bytes())
    }

    /// # Errors
    /// As [`Line::push`].
    pub fn push_i32(&mut self, value: i32) -> Result<(), Overflow> {
        let mut digits = [0u8; 11];
        self.push(fmt_i32(&mut digits, value))
    }

    /// # Errors
    /// As [`Line::push`].
    pub fn push_u32(&mut self, value: u32) -> Result<(), Overflow> {
        let mut digits = [0u8; 11];
        self.push(fmt_u32(&mut digits, value))
    }

    /// # Errors
    /// As [`Line::push`].
    pub fn push_bytes(&mut self, s: &[u8]) -> Result<(), Overflow> {
        push_into(&mut self.buf, &mut self.len, s)
    }

    /// `v % 100` as exactly two digits.
    ///
    /// # Errors
    /// As [`Line::push`].
    pub fn push_two_digits(&mut self, v: u8) -> Result<(), Overflow> {
        let v = v % 100;
        self.push_bytes(&[b'0' + v / 10, b'0' + v % 10])
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// One copy for every line width, not one per width.
#[inline(never)]
fn push_into(buf: &mut [u8], len: &mut usize, s: &[u8]) -> Result<(), Overflow> {
    let needed = *len + s.len();
    for &b in s {
        if *len == buf.len() {
            return Err(Overflow { needed });
        }
        buf[*len] = b;
        *len += 1;
    }
    Ok(())
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

/// Writes right-aligned into `buf` and returns the used tail.
pub fn fmt_i32(buf: &mut [u8; 11], value: i32) -> &str {
    let mut n = value.unsigned_abs();
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + u8::try_from(n % 10).unwrap_or(0);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if value < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

pub const MAX_LINES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub top: i32,
    pub scale: u32,
    glyphs: [u8; MAX_COLS],
    len: usize,
}

impl Placed {
    #[must_use]
    pub fn text(&self) -> &[u8] {
        &self.glyphs[..self.len]
    }

    /// Centred, as drawn.
    fn left(&self) -> i32 {
        let len = u32::try_from(self.len).unwrap_or(0);
        i32::try_from(WIDTH.saturating_sub(span(len, self.scale)) / 2).unwrap_or(0)
    }

    /// The columns glyph `k` can light.
    fn glyph_columns(&self, k: usize) -> core::ops::Range<i32> {
        let s = i32::try_from(self.scale).unwrap_or(1);
        let x = self.left() + i32::try_from(k).unwrap_or(0) * 6 * s;
        x..x + 5 * s
    }

    fn line_columns(&self) -> core::ops::Range<i32> {
        let len = u32::try_from(self.len).unwrap_or(0);
        self.left()..self.left() + i32::try_from(span(len, self.scale)).unwrap_or(0)
    }
}

/// Equal values draw the same pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glass {
    lines: [Placed; MAX_LINES],
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
        Self {
            lines: [Placed { top: 0, scale: 0, glyphs: [b' '; MAX_COLS], len: 0 }; MAX_LINES],
            n: 0,
        }
    }

    /// Centres `line` with its top at `TOP`; a line that cannot fit the glass
    /// there does not build, and one past [`MAX_LINES`] is dropped.
    pub fn put<const TOP: i32, const SCALE_: u32, const N: usize>(&mut self, line: &Line<N>) {
        const {
            assert!(N <= MAX_COLS, "line is wider than any row");
            assert!(
                N <= capacity_at(TOP, SCALE_),
                "line is wider than the glass under its rows"
            );
        }
        self.place(TOP, SCALE_, line);
    }

    fn place<const N: usize>(&mut self, top: i32, scale: u32, line: &Line<N>) {
        if self.n == MAX_LINES {
            return;
        }
        let mut glyphs = [b' '; MAX_COLS];
        let len = line.len.min(MAX_COLS);
        glyphs[..len].copy_from_slice(&line.as_bytes()[..len]);
        self.lines[self.n] = Placed { top, scale, glyphs, len };
        self.n += 1;
    }

    #[must_use]
    pub fn lines(&self) -> &[Placed] {
        &self.lines[..self.n]
    }

    /// The columns where this glass can differ from `before` over the same background: the
    /// changed glyphs of a line that kept its place and length, the whole of one that did not,
    /// and every column without `before`.
    #[must_use]
    pub fn changed_columns(&self, before: Option<&Self>) -> [bool; WIDTH as usize] {
        let Some(before) = before else {
            return [true; WIDTH as usize];
        };
        let mut changed = [false; WIDTH as usize];
        let mut mark = |columns: core::ops::Range<i32>| {
            for x in columns.filter_map(|x| usize::try_from(x).ok()) {
                if let Some(c) = changed.get_mut(x) {
                    *c = true;
                }
            }
        };
        for i in 0..self.n.max(before.n) {
            match (before.lines().get(i), self.lines().get(i)) {
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) if (a.top, a.scale, a.len) == (b.top, b.scale, b.len) => {
                    for (k, (x, y)) in a.text().iter().zip(b.text()).enumerate() {
                        if x != y {
                            mark(a.glyph_columns(k));
                        }
                    }
                }
                (a, b) => {
                    for line in [a, b].into_iter().flatten() {
                        mark(line.line_columns());
                    }
                }
            }
        }
        changed
    }

    /// In panel pixels, at each line's scale.
    #[must_use]
    pub fn lit_pixels(&self) -> u32 {
        self.lines()
            .iter()
            .map(|l| l.text().iter().copied().map(lit_pixels).sum::<u32>() * l.scale * l.scale)
            .sum()
    }

    pub fn draw_with(&self, surface: &mut Surface<'_, Abgr2222>, ink: Abgr2222) {
        for i in 0..self.n {
            self.draw_line_with(surface, i, ink);
        }
    }

    /// Nothing for an `i` past the last line.
    pub fn draw_line_with(&self, surface: &mut Surface<'_, Abgr2222>, i: usize, ink: Abgr2222) {
        if let Some(l) = self.lines().get(i) {
            let x0 = l.left();
            let s = i32::try_from(l.scale).unwrap_or(1);
            for (col, &ch) in (0i32..).zip(l.text()) {
                if ch == b' ' {
                    continue;
                }
                for (gx, column) in (0i32..).zip(glyph(ch)) {
                    for gy in 0..7i32 {
                        if column & (1 << gy) != 0 {
                            let x = x0 + (col * 6 + gx) * s;
                            surface.fill_rect(x, l.top + gy * s, s, s, ink);
                        }
                    }
                }
            }
        }
    }
}
