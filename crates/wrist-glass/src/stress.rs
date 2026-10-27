//! Screens that work the drawing hard, chosen from the console, each a function of its step.

use wrist_l10n::Sink;

use crate::pages::{compose, compose_about, compose_battery, About, Battery, Clock, Date, Faces, Short, Status, EN_GB};
use crate::text::{Font, Glass, Line, LINE_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stress {
    /// Every glyph of every face, a window of each at a time.
    Glyphs,
    /// Every digit of the face's lines turning at once.
    Churn,
    /// Lines cut where the disc is narrowest, top and bottom, and full width across the middle.
    Edge,
    /// Two screens with nothing in common, by turns.
    Full,
    /// The face, battery and about pages, by turns.
    Pages,
}

impl Stress {
    pub const ALL: [Self; 5] = [Self::Glyphs, Self::Churn, Self::Edge, Self::Full, Self::Pages];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Glyphs => "glyphs",
            Self::Churn => "churn",
            Self::Edge => "edge",
            Self::Full => "full",
            Self::Pages => "pages",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }
}

struct Lines {
    glass: Glass,
    short: Short,
    n: u8,
}

impl Lines {
    fn new() -> Self {
        Self { glass: Glass::new(), short: 0, n: 0 }
    }

    fn put(&mut self, font: Font, baseline: i32, f: impl FnOnce(&mut Line) -> Result<(), crate::text::Overflow>) {
        let mut line = Line::new();
        let pushed = f(&mut line);
        if pushed.and(self.glass.put(font, baseline, &line)).is_err() {
            self.short |= 1 << self.n;
        }
        self.n += 1;
    }

    fn done(self) -> (Glass, Short) {
        (self.glass, self.short)
    }
}

/// As many of `font`'s characters as a line holds, from the `step`th window of them.
fn window(line: &mut Line, font: &Font, step: u32) -> Result<(), crate::text::Overflow> {
    let nodes = font.face.nodes;
    let per = LINE_BYTES / 2;
    let from = usize::try_from(step).unwrap_or(0).wrapping_mul(per) % nodes.len().max(1);
    for k in 0..per.min(nodes.len()) {
        if let Some(c) = char::from_u32(u32::from(nodes[(from + k) % nodes.len()].cp)) {
            let mut buf = [0u8; 4];
            line.push_verbatim(c.encode_utf8(&mut buf))?;
        }
    }
    Ok(())
}

const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";

fn rotated(line: &mut Line, step: u32, take: usize) -> Result<(), crate::text::Overflow> {
    let from = usize::try_from(step).unwrap_or(0) % ALPHABET.len();
    for c in ALPHABET.chars().cycle().skip(from).take(take) {
        let mut buf = [0u8; 4];
        line.push_verbatim(c.encode_utf8(&mut buf))?;
    }
    Ok(())
}

/// Step `step` of `kind`, in `faces`.
#[must_use]
pub fn compose_stress(kind: Stress, step: u32, faces: &Faces) -> (Glass, Short) {
    let mut l = Lines::new();
    match kind {
        Stress::Glyphs => {
            l.put(faces.time, 82, |line| window(line, &faces.time, step));
            l.put(faces.charge, 132, |line| window(line, &faces.charge, step));
            l.put(faces.words, 162, |line| window(line, &faces.words, step));
            l.put(faces.id, 184, |line| window(line, &faces.id, step));
            l.put(Font::new(faces.words.figures), 206, |line| window(line, &Font::new(faces.words.figures), step));
        }
        Stress::Churn => {
            let d = u8::try_from(step % 10).unwrap_or(0);
            l.put(faces.words, 84, |line| {
                line.number(u32::from(d) * 1111)?;
                line.text(" mV")
            });
            l.put(faces.time, 143, |line| {
                line.two_digits(d * 11)?;
                line.text(":")?;
                line.two_digits(d * 11)
            });
            l.put(faces.words, 175, |line| {
                line.number(u32::from(d) * 111)?;
                line.text(" %")
            });
        }
        Stress::Edge => {
            l.put(faces.id, 24, |line| rotated(line, step, 30));
            l.put(faces.words, 52, |line| rotated(line, step + 7, 30));
            l.put(faces.words, 128, |line| rotated(line, step + 13, 30));
            l.put(faces.words, 204, |line| rotated(line, step + 19, 30));
            l.put(faces.id, 228, |line| rotated(line, step + 23, 30));
        }
        Stress::Full if step.is_multiple_of(2) => {
            l.put(faces.charge, 100, |line| line.number(888_888));
            l.put(faces.time, 185, |line| {
                line.two_digits(88)?;
                line.text(":")?;
                line.two_digits(88)
            });
        }
        Stress::Full => {
            for (i, baseline) in [60, 90, 120, 150, 180, 205].into_iter().enumerate() {
                l.put(faces.words, baseline, |line| rotated(line, step + 5 * u32::try_from(i).unwrap_or(0), 18));
            }
        }
        Stress::Pages => {
            return match step % 3 {
                0 => {
                    let clock = Some(Clock { h: 12, m: 34, s: 0 });
                    let date = Some(Date { year: 2026, month: 10, day: 27 });
                    compose(&Status { clock, date, soc_pct: Some(56) }, &EN_GB, faces)
                }
                1 => compose_battery(&Battery { soc_pct: Some(56), cell_mv: Some(3871), external: Some(false) }, &EN_GB, faces),
                _ => compose_about(&About { version: "0.2.0", build: "5784ec72019b" }, &EN_GB, faces),
            };
        }
    }
    l.done()
}
