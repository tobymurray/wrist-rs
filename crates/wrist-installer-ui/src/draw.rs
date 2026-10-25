//! The screens, in the geometry of the SDK's activity widgets and the colours of `SDK::GUI::Color`.

use incised_text::{faces, pick, Face, Style};
use inscribed_disc::color::shade;
use inscribed_disc::{Abgr2222, Surface};

use crate::flow::HOLD_MS;
use crate::installer::{Hint, Hints, Hold, Info, Screen, HOLD_NAME, HOLD_PROMPT, TITLE};

pub const WIDTH: u32 = 240;
pub const HEIGHT: u32 = 240;

const CENTRE: f32 = 120.0;

pub const BLACK: Abgr2222 = Abgr2222::BLACK;
const WHITE: Abgr2222 = Abgr2222::from_levels(3, 3, 3);
const GRAY: Abgr2222 = Abgr2222::from_levels(2, 2, 2);
const GRAY_DARK: Abgr2222 = Abgr2222::from_levels(1, 1, 1);
const TEAL: Abgr2222 = Abgr2222::from_levels(0, 2, 2);
/// `SDK::GUI::Color::YELLOW_DARK`: a button that does something here.
const AMBER: Abgr2222 = Abgr2222::from_levels(3, 2, 0);
/// `SDK::GUI::Color::CHARTREUSE`: the button being held.
const CHARTREUSE: Abgr2222 = Abgr2222::from_levels(1, 3, 0);
const GREEN: Abgr2222 = Abgr2222::from_levels(0, 3, 0);

/// A band of the disc between two radii and two angles, with round ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arc {
    pub inner: f32,
    pub outer: f32,
    pub start: f32,
    pub end: f32,
}

impl Arc {
    /// Sized as the SDK's widgets size an arc, from its centre-line `radius` and `width`.
    #[allow(clippy::cast_precision_loss)]
    const fn sdk(radius: i32, width: i32, start: i32, end: i32) -> Arc {
        let outer = radius + (width + 1) / 2;
        Arc { inner: (outer - width) as f32, outer: outer as f32, start: start as f32, end: end as f32 }
    }
}

/// R1, Select, as the SDK's button hints place it.
pub const R1_ARC: Arc = Arc::sdk(113, 6, 52, 69);
/// R2, Back.
pub const R2_ARC: Arc = Arc::sdk(113, 6, 112, 128);
/// As the SDK's timer ring.
pub const RING: Arc = Arc::sdk(112, 9, 138, 138 + 264);

/// Lines of text keep inside this radius where a hint or the ring could be beside them.
pub const TEXT_RADIUS: f32 = 104.0;
/// And never wider than the SDK screens' text column.
pub const TEXT_WIDTH: i32 = 180;

pub const SMALL: &Face = &faces::POPPINS_REGULAR_14_ASCII;
/// Atkinson Hyperlegible Next tells 0 from O and 8 from B, which a CRC read against a file needs.
pub const LARGE: &Face = &faces::ATKINSON_BOLD_25_ASCII;
pub const COUNTDOWN: &Face = &faces::ATKINSON_BOLD_49_CLOCK;
/// The CRC under the countdown, in the face the ready screen showed it in.
pub const HOLD_CRC: &Face = &faces::ATKINSON_BOLD_16_ASCII;
/// The headline steps down through these until it fits, as a long refusal code needs.
pub const HEADLINES: [&Face; 3] = [LARGE, &faces::ATKINSON_BOLD_20_ASCII, HOLD_CRC];

/// The face `headline` is drawn in.
#[must_use]
pub fn headline_face(headline: &str) -> &'static Face {
    let room = width_at(LARGE, at::HEADLINE).min(TEXT_WIDTH);
    pick(&HEADLINES, headline, room).unwrap_or(HEADLINES[HEADLINES.len() - 1])
}

/// Baselines.
pub mod at {
    pub const TITLE: i32 = 44;
    pub const HEADLINE: i32 = 82;
    pub const CAPTION: i32 = 106;
    pub const DETAIL: i32 = 136;
    pub const DETAIL_PITCH: i32 = 19;
    pub const HINT: i32 = 206;

    pub const HOLD_PROMPT: i32 = 56;
    pub const HOLD_NAME: i32 = 90;
    pub const HOLD_NUMBER: i32 = 158;
    pub const HOLD_CRC: i32 = 190;
}

/// The most lines a detail may take before it would reach the hint.
pub const DETAIL_LINES: usize = 3;

/// Draws `screen` over the whole frame.
pub fn render(surface: &mut Surface<'_, Abgr2222>, screen: &Screen) {
    surface.clear(BLACK);
    render_text(surface, screen);
    render_arcs(surface, screen);
}

/// The text alone, so the tests can hold it apart from the arcs.
pub fn render_text(surface: &mut Surface<'_, Abgr2222>, screen: &Screen) {
    match screen {
        Screen::Info(info) => info_text(surface, info),
        Screen::Hold(hold) => hold_text(surface, hold),
    }
}

/// The hints and the ring alone.
pub fn render_arcs(surface: &mut Surface<'_, Abgr2222>, screen: &Screen) {
    match screen {
        Screen::Info(info) => hints(surface, info.hints),
        Screen::Hold(hold) => {
            arc(surface, RING, GRAY_DARK);
            if let Some(progress) = progress(RING, hold.held_ms) {
                arc(surface, progress, GREEN);
            }
            // No back hint: letting go of R1 cancels.
            hints(surface, Hints { r1: Hint::Green, r2: Hint::Off });
        }
    }
}

/// The ring's filled part after `held_ms`, continuous rather than in whole degrees.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn progress(ring: Arc, held_ms: u32) -> Option<Arc> {
    let fraction = held_ms.min(HOLD_MS) as f32 / HOLD_MS as f32;
    (fraction > 0.0).then_some(Arc { end: ring.start + (ring.end - ring.start) * fraction, ..ring })
}

fn info_text(surface: &mut Surface<'_, Abgr2222>, info: &Info) {
    line(surface, SMALL, TITLE, at::TITLE, TEAL);
    line(surface, headline_face(info.headline.as_str()), info.headline.as_str(), at::HEADLINE, WHITE);
    line(surface, SMALL, info.caption, at::CAPTION, GRAY);
    let mut lines = [""; DETAIL_LINES];
    let count = wrap(SMALL, info.detail, at::DETAIL, at::DETAIL_PITCH, &mut lines);
    for (i, text) in (0..).zip(lines.iter().take(count)) {
        line(surface, SMALL, text, at::DETAIL + i * at::DETAIL_PITCH, WHITE);
    }
    line(surface, SMALL, info.hint, at::HINT, GRAY);
}

fn hold_text(surface: &mut Surface<'_, Abgr2222>, hold: &Hold) {
    line(surface, SMALL, HOLD_PROMPT, at::HOLD_PROMPT, GRAY);
    line(surface, LARGE, HOLD_NAME, at::HOLD_NAME, WHITE);
    let digit = [b'0' + u8::try_from(hold.number.min(9)).unwrap_or(9)];
    line(surface, COUNTDOWN, core::str::from_utf8(&digit).unwrap_or(""), at::HOLD_NUMBER, WHITE);
    line(surface, HOLD_CRC, hold.crc.as_str(), at::HOLD_CRC, GRAY);
}

fn line(surface: &mut Surface<'_, Abgr2222>, face: &Face, text: &str, baseline: i32, ink: Abgr2222) {
    if !text.is_empty() {
        // The surface clips and cannot fail.
        let _ = face.draw(surface, text, 120, baseline, Style::centered(ink, BLACK));
    }
}

/// How wide a line on `baseline` may be: the column, or less where the disc narrows.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn width_at(face: &Face, baseline: i32) -> i32 {
    let top = (baseline - i32::from(face.ascent)) as f32 - CENTRE;
    let bottom = (baseline + i32::from(face.descent)) as f32 - CENTRE;
    let reach = top.abs().max(bottom.abs());
    let half = (TEXT_RADIUS * TEXT_RADIUS - reach * reach).max(0.0);
    (2.0 * libm::sqrtf(half)) as i32
}

/// Word wraps `text` into `lines` from `first` down, each to its own width: newlines end a
/// line, and a word is never split. Returns how many lines it needs, which exceeds
/// `lines.len()` when it did not fit.
pub fn wrap<'t>(face: &Face, text: &'t str, first: i32, pitch: i32, lines: &mut [&'t str]) -> usize {
    let mut count = 0usize;
    for paragraph in text.split('\n') {
        let mut start = 0usize;
        let mut end = 0usize;
        let width = |count: usize| {
            let row = first + pitch * i32::try_from(count).unwrap_or(i32::MAX / 2);
            width_at(face, row).min(TEXT_WIDTH)
        };
        let breaks = paragraph
            .char_indices()
            .filter(|&(_, c)| c == ' ')
            .map(|(i, _)| i)
            .chain(core::iter::once(paragraph.len()));
        for brk in breaks {
            let candidate = &paragraph[start..brk];
            if end == start || face.measure(candidate).advance <= width(count) {
                end = brk;
            } else {
                if let Some(slot) = lines.get_mut(count) {
                    *slot = &paragraph[start..end];
                }
                count += 1;
                start = end + 1;
                end = brk;
            }
        }
        if start < paragraph.len() {
            if let Some(slot) = lines.get_mut(count) {
                *slot = &paragraph[start..];
            }
            count += 1;
        }
    }
    count
}

fn hints(surface: &mut Surface<'_, Abgr2222>, hints: Hints) {
    for (hint, at) in [(hints.r1, R1_ARC), (hints.r2, R2_ARC)] {
        let ink = match hint {
            Hint::Off => continue,
            Hint::White => WHITE,
            Hint::Amber => AMBER,
            Hint::Green => CHARTREUSE,
        };
        arc(surface, at, ink);
    }
}

/// Screen coordinates are y down, so a clockwise turn from `a` to `b` is a positive cross.
fn cross(a: (f32, f32), b: (f32, f32)) -> f32 {
    a.0 * b.1 - a.1 * b.0
}

fn direction(degrees: f32) -> (f32, f32) {
    let r = degrees.to_radians();
    (libm::sinf(r), -libm::cosf(r))
}

/// The box an arc can reach: its ends, and each quarter turn it passes, padded by its caps.
#[allow(clippy::cast_possible_truncation)]
fn reach(arc: Arc, width: i32, height: i32) -> (core::ops::Range<i32>, core::ops::Range<i32>) {
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let turns = [0.0, 90.0, 180.0, 270.0, 360.0, 450.0];
    let passed = turns.iter().copied().filter(|t| (arc.start..=arc.end).contains(t));
    for angle in [arc.start, arc.end].into_iter().chain(passed) {
        let (dx, dy) = direction(angle);
        for radius in [arc.inner, arc.outer] {
            let (x, y) = (CENTRE + radius * dx, CENTRE + radius * dy);
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
    }
    let pad = (arc.outer - arc.inner) / 2.0 + 2.0;
    (
        (libm::floorf(top - pad) as i32).max(0)..(libm::ceilf(bottom + pad) as i32).min(height),
        (libm::floorf(left - pad) as i32).max(0)..(libm::ceilf(right + pad) as i32).min(width),
    )
}

/// Fills `a` in `ink`, each edge pixel blended by its centre's distance from the arc's edge.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn arc(surface: &mut Surface<'_, Abgr2222>, a: Arc, ink: Abgr2222) {
    if a.end <= a.start {
        return;
    }
    let (rows, columns) = reach(a, surface.width(), surface.height());
    let shape = Shape::of(a);
    let (outside, hole) = (a.outer + 1.0, a.inner - 1.0);
    for y in rows {
        let dy = y as f32 + 0.5 - CENTRE;
        if dy.abs() > outside {
            continue;
        }
        // Each row crosses the band twice, left and right of the hole.
        let far = libm::sqrtf(square(outside) - square(dy));
        let near = if dy.abs() < hole { libm::sqrtf(square(hole) - square(dy)) } else { 0.0 };
        let left = (libm::floorf(CENTRE - far) as i32, libm::ceilf(CENTRE - near) as i32);
        let right = (libm::floorf(CENTRE + near) as i32, libm::ceilf(CENTRE + far) as i32);
        for (x0, x1) in [left, right] {
            for x in x0.max(columns.start)..x1.min(columns.end) {
                let depth = shape.depth(x as f32 + 0.5, y as f32 + 0.5);
                let level = (3.0 * (depth + 0.5).clamp(0.0, 1.0) + 0.5) as u8;
                if level == 0 {
                    continue;
                }
                // One pixel at a time, so the clip is the cheap test and not a span's.
                if surface.is_lit(x, y) {
                    let i = (y * surface.width() + x) as usize;
                    let pixel = &mut surface.bytes_mut()[i];
                    *pixel = shade(ink, Abgr2222(*pixel), level).0;
                }
            }
        }
    }
}

fn square(v: f32) -> f32 {
    v * v
}

impl Arc {
    /// How far inside the arc a point is, in pixels; negative outside.
    #[must_use]
    pub fn depth(&self, x: f32, y: f32) -> f32 {
        Shape::of(*self).depth(x, y)
    }
}

/// An arc with what every pixel needs worked out once.
struct Shape {
    mid: f32,
    half: f32,
    span: f32,
    from: (f32, f32),
    to: (f32, f32),
}

impl Shape {
    fn of(a: Arc) -> Shape {
        Shape {
            mid: f32::midpoint(a.inner, a.outer),
            half: (a.outer - a.inner) / 2.0,
            span: a.end - a.start,
            from: direction(a.start),
            to: direction(a.end),
        }
    }

    fn depth(&self, x: f32, y: f32) -> f32 {
        let u = (x - CENTRE, y - CENTRE);
        let (from, to) = (self.from, self.to);
        let in_sweep = if self.span >= 360.0 {
            true
        } else if self.span <= 180.0 {
            cross(from, u) >= 0.0 && cross(u, to) >= 0.0
        } else {
            !(cross(to, u) > 0.0 && cross(u, from) > 0.0)
        };
        if in_sweep {
            return self.half - (libm::sqrtf(square(u.0) + square(u.1)) - self.mid).abs();
        }
        // Past either end, the round cap there.
        let cap = |d: (f32, f32)| libm::sqrtf(square(u.0 - self.mid * d.0) + square(u.1 - self.mid * d.1));
        self.half - cap(from).min(cap(to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eight by eight samples a pixel, around the whole disc.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn sampled(a: Arc) -> Vec<u8> {
        let mut levels = vec![0u8; (WIDTH * HEIGHT) as usize];
        for (i, level) in levels.iter_mut().enumerate() {
            let (x, y) = ((i % WIDTH as usize) as f32, (i / WIDTH as usize) as f32);
            let mut covered = 0;
            for s in 0..64 {
                let (sx, sy) = ((s % 8) as f32 + 0.5, (s / 8) as f32 + 0.5);
                covered += u32::from(a.depth(x + sx / 8.0, y + sy / 8.0) >= 0.0);
            }
            *level = ((covered * 3 + 32) / 64) as u8;
        }
        levels
    }

    #[test]
    fn arcs_are_within_a_level_of_sampling_every_pixel() {
        let mut arcs = vec![RING, R1_ARC, R2_ARC, Arc { start: 0.0, end: 360.0, ..RING }];
        for held in [1, 7, 500, 1499, 1500, 1501, 2999, 3000] {
            arcs.extend(progress(RING, held));
        }
        for a in arcs {
            let mut fb = vec![BLACK.0; (WIDTH * HEIGHT) as usize];
            let mut s = Surface::<Abgr2222>::rect(&mut fb, WIDTH, HEIGHT).unwrap();
            arc(&mut s, a, WHITE);
            // White over black at level n is n on every channel.
            let drawn: Vec<u8> = fb.iter().map(|&b| Abgr2222(b).r()).collect();
            let reference = sampled(a);
            let mut differ = 0;
            for (i, (&d, &r)) in drawn.iter().zip(&reference).enumerate() {
                assert!(d.abs_diff(r) <= 1, "{a:?}: pixel {i} is {d}, sampled {r}");
                differ += usize::from(d != r);
            }
            let inked = reference.iter().filter(|&&r| r > 0).count();
            assert!(differ * 10 <= inked, "{a:?}: {differ} of {inked} pixels differ");
        }
    }

    #[test]
    fn depth_is_half_the_width_on_the_centre_line_and_zero_at_the_edges() {
        let d = direction(270.0);
        for (r, want) in [(112.5, 4.5), (108.0, 0.0), (117.0, 0.0), (120.0, -3.0)] {
            let got = RING.depth(CENTRE + r * d.0, CENTRE + r * d.1);
            assert!((got - want).abs() < 1e-3, "radius {r}: {got}");
        }
        // Past the end, the cap: half the width out along the tangent is its edge.
        let end = direction(RING.end);
        let tangent = direction(RING.end + 90.0);
        let (x, y) = (CENTRE + 112.5 * end.0 + 4.5 * tangent.0, CENTRE + 112.5 * end.1 + 4.5 * tangent.1);
        assert!(RING.depth(x, y).abs() < 1e-3);
    }
}
