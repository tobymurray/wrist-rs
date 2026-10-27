//! A glass laid out once, every character where it is drawn.

use core::ops::Range;

use embedded_graphics::draw_target::DrawTargetExt;
use embedded_graphics::prelude::{Point, Size};
use embedded_graphics::primitives::Rectangle;
use incised_text::{Face, Ink, Node, Style};
use inscribed_disc::{Abgr2222, Surface};

use crate::text::{Glass, LINE_BYTES, MAX_LINES};
use crate::{HEIGHT, WIDTH};

#[allow(clippy::cast_possible_wrap)]
const W: i32 = WIDTH as i32;

#[allow(clippy::cast_possible_wrap)]
const H: i32 = HEIGHT as i32;

/// A set of panel rows.
#[derive(Default)]
struct Rows([u32; HEIGHT.div_ceil(32) as usize]);

impl Rows {
    fn mark(&mut self, rows: Range<i32>) {
        for y in rows.start.max(0)..rows.end.min(H) {
            #[allow(clippy::cast_sign_loss)]
            let y = y as usize;
            self.0[y / 32] |= 1 << (y % 32);
        }
    }

    fn has(&self, y: i32) -> bool {
        usize::try_from(y).is_ok_and(|y| self.0[y / 32] & (1 << (y % 32)) != 0)
    }

    /// The marked rows, as runs from the top.
    fn runs(&self) -> impl Iterator<Item = Range<i32>> + '_ {
        let mut y = 0;
        core::iter::from_fn(move || {
            while y < H && !self.has(y) {
                y += 1;
            }
            let start = y;
            while y < H && self.has(y) {
                y += 1;
            }
            (y > start).then_some(start..y)
        })
    }
}

pub const MAX_GLYPHS: usize = MAX_LINES * LINE_BYTES;

/// A [`crate::text::Glyph`] in less room; equal exactly when the glyphs are.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    ch: char,
    pen: i16,
    advance: i16,
    ink: (i16, i16),
    x: i16,
    figure: bool,
}

/// All zeros is nothing, so an empty scene costs no flash.
#[derive(Clone, Copy)]
struct Drawn {
    key: Key,
    /// Where `c` is drawn from: a figure dash the face lacks is drawn as another character.
    face: Option<&'static Face>,
    c: char,
    node: Option<&'static Node>,
}

const NOTHING: Drawn = Drawn {
    key: Key { ch: '\0', pen: 0, advance: 0, ink: (0, 0), x: 0, figure: false },
    face: None,
    c: '\0',
    node: None,
};

#[derive(Clone, Copy)]
struct LineAt {
    face: &'static Face,
    figures: &'static Face,
    baseline: i32,
    /// Its glyphs' indices, `start..end`.
    glyphs: (u16, u16),
}

#[derive(Clone)]
pub struct Scene {
    glyphs: [Drawn; MAX_GLYPHS],
    n: usize,
    lines: [Option<LineAt>; MAX_LINES],
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

fn short(v: i32) -> i16 {
    i16::try_from(v).unwrap_or(if v < 0 { i16::MIN } else { i16::MAX })
}

impl Scene {
    #[must_use]
    pub const fn new() -> Self {
        Self { glyphs: [NOTHING; MAX_GLYPHS], n: 0, lines: [None; MAX_LINES] }
    }

    /// Every line of `glass`.
    #[must_use]
    pub fn of(glass: &Glass) -> Self {
        let mut scene = Self::new();
        for i in 0..glass.len() {
            scene.lay(glass, i);
        }
        scene
    }

    /// Lays out line `i` of `glass`, after lines `0..i`; line 0 starts the scene afresh.
    pub fn lay(&mut self, glass: &Glass, i: usize) {
        if i == 0 {
            self.n = 0;
            self.lines = [None; MAX_LINES];
        }
        let (Some(placed), Some(slot)) = (glass.lines().nth(i), self.lines.get_mut(i)) else {
            return;
        };
        let start = self.n;
        for g in placed.glyphs() {
            let Some(at) = self.glyphs.get_mut(self.n) else { break };
            let (face, c) = placed.font.source(g.ch, g.figure);
            let key = Key { ch: g.ch, pen: short(g.pen), advance: short(g.advance), ink: (short(g.ink.0), short(g.ink.1)), x: short(g.x), figure: g.figure };
            *at = Drawn { key, face: Some(face), c, node: face.glyph(c) };
            self.n += 1;
        }
        let as_u16 = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        *slot = Some(LineAt {
            face: placed.font.face,
            figures: placed.font.figures,
            baseline: placed.baseline,
            glyphs: (as_u16(start), as_u16(self.n)),
        });
    }

    fn line(&self, at: &LineAt) -> impl Iterator<Item = &Drawn> + '_ {
        self.glyphs[usize::from(at.glyphs.0)..usize::from(at.glyphs.1)].iter()
    }

    /// Marks the rows its characters in `columns` can draw on.
    fn cover(&self, columns: &Range<i32>, rows: &mut Rows) {
        for at in self.lines.iter().flatten() {
            for d in self.line(at) {
                if i32::from(d.key.ink.0) >= columns.end || i32::from(d.key.ink.1) <= columns.start {
                    continue;
                }
                match d.node {
                    Some(n) => {
                        let top = at.baseline - i32::from(n.top);
                        rows.mark(top..top + i32::from(n.h));
                    }
                    None => rows.mark(0..H),
                }
            }
        }
    }

    /// [`Glass::changed_columns`] between the glasses these scenes lay out.
    #[must_use]
    pub fn changed_columns(&self, before: Option<&Self>) -> [bool; WIDTH as usize] {
        let Some(before) = before else {
            return [true; WIDTH as usize];
        };
        let mut changed = [false; WIDTH as usize];
        let mut mark = |d: &Drawn| {
            for x in (d.key.ink.0..d.key.ink.1).filter_map(|x| usize::try_from(x).ok()) {
                if let Some(c) = changed.get_mut(x) {
                    *c = true;
                }
            }
        };
        for (a, b) in before.lines.iter().zip(&self.lines) {
            match (a, b) {
                (Some(a), Some(b))
                    if core::ptr::eq(a.face, b.face) && core::ptr::eq(a.figures, b.figures) && a.baseline == b.baseline =>
                {
                    let (mut old, mut new) = (before.line(a), self.line(b));
                    loop {
                        match (old.next(), new.next()) {
                            (None, None) => break,
                            (Some(o), Some(n)) if o.key == n.key => {}
                            (o, n) => o.into_iter().chain(n).for_each(&mut mark),
                        }
                    }
                }
                (a, b) => {
                    if let Some(a) = a {
                        before.line(a).for_each(&mut mark);
                    }
                    if let Some(b) = b {
                        self.line(b).for_each(&mut mark);
                    }
                }
            }
        }
        changed
    }

    /// `columns` of `fb` as the glass draws there in `ink` over `background`, put back only on
    /// the rows a character of this scene or of `before`, the one `fb` holds, covers.
    pub fn repaint(
        &self,
        fb: &mut [u8],
        mut background: impl FnMut(&mut [u8], Range<i32>, Range<i32>),
        before: Option<&Self>,
        columns: Range<i32>,
        ink: Abgr2222,
    ) {
        let columns = columns.start.max(0)..columns.end.min(W);
        if columns.is_empty() {
            return;
        }
        let mut rows = Rows::default();
        match before {
            Some(before) => {
                before.cover(&columns, &mut rows);
                self.cover(&columns, &mut rows);
            }
            None => rows.mark(0..H),
        }
        for run in rows.runs() {
            background(fb, run, columns.clone());
        }
        let ramp = [1, 2, 3].map(|level| ink.shade(Abgr2222::BLACK, level).0);
        for at in self.lines.iter().flatten() {
            for d in self.line(at) {
                let (left, right) = (i32::from(d.key.ink.0), i32::from(d.key.ink.1));
                if left >= columns.end || right <= columns.start {
                    continue;
                }
                let x = i32::from(d.key.x);
                let Some(face) = d.face else { continue };
                if let Some(n) = d.node {
                    let top_left = (x + i32::from(n.left), at.baseline - i32::from(n.top));
                    crate::blit::node(fb, face, n, top_left, ramp, &columns);
                } else if let Some(mut surface) = Surface::<Abgr2222>::round(fb, WIDTH, HEIGHT) {
                    let width = u32::try_from(columns.end - columns.start).unwrap_or(0);
                    let band = Rectangle::new(Point::new(columns.start, 0), Size::new(width, HEIGHT));
                    let mut buf = [0u8; 4];
                    let style = Style::left(ink, Abgr2222::BLACK);
                    let _ = face.draw(&mut surface.clipped(&band), d.c.encode_utf8(&mut buf), x, at.baseline, style);
                }
            }
        }
    }
}

