//! A glyph's pixels written straight into the frame: what `Face::draw` puts through a round
//! `Surface`, without a call per pixel.

use core::ops::Range;

use incised_text::{Face, Node};
use inscribed_disc::geometry::is_lit;

use crate::{HEIGHT, WIDTH};

#[allow(clippy::cast_possible_wrap)]
const W: i32 = WIDTH as i32;
#[allow(clippy::cast_possible_wrap)]
const H: i32 = HEIGHT as i32;

/// Each row's first lit column; the row is lit from there to its mirror image.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
static LIT: [u8; HEIGHT as usize] = {
    let mut lit = [0u8; HEIGHT as usize];
    let mut y = 0;
    while y < H {
        let mut x = 0;
        while x < W / 2 && !is_lit(x, y, W, H) {
            x += 1;
        }
        lit[y as usize] = x as u8;
        y += 1;
    }
    lit
};

/// Draws `node` of `face` with its ink's top-left at (`x0`, `y0`), only in `columns`, coverage
/// levels 1 to 3 as `ramp`; `fb` is `WIDTH * HEIGHT` bytes.
#[allow(clippy::cast_sign_loss)]
pub fn node(fb: &mut [u8], face: &Face, node: &Node, (x0, y0): (i32, i32), ramp: [u8; 3], columns: &Range<i32>) {
    let ramp = [0, ramp[0], ramp[1], ramp[2]];
    let (w, h) = (i32::from(node.w), i32::from(node.h));
    let first = (columns.start.max(0) - x0).max(0);
    let last = (columns.end.min(W) - x0).min(w);
    let base = node.off as usize * 4;
    for row in (-y0).max(0)..(H - y0).min(h) {
        let y = y0 + row;
        let lit = i32::from(LIT[y as usize]);
        let from = first.max(lit - x0);
        let to = last.min(W - lit - x0);
        if from >= to {
            continue;
        }
        let out = &mut fb[(y * W + x0 + from) as usize..(y * W + x0 + to) as usize];
        let start = base + (row * w + from) as usize;
        let Some(bytes) = face.data.get(start / 4..=(start + out.len() - 1) / 4) else { continue };
        let mut px = out.iter_mut();
        let mut skip = start % 4;
        for &byte in bytes {
            let mut levels = byte >> (2 * skip);
            let n = 4 - skip;
            skip = 0;
            if levels == 0 {
                px.nth(n - 1);
                continue;
            }
            for p in px.by_ref().take(n) {
                let level = levels & 3;
                levels >>= 2;
                if level != 0 {
                    *p = ramp[usize::from(level)];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;

    use embedded_graphics::draw_target::DrawTargetExt;
    use embedded_graphics::prelude::{Point, Size};
    use embedded_graphics::primitives::Rectangle;
    use incised_text::{Face, Ink, Style};
    use inscribed_disc::{Abgr2222, Surface};

    use inscribed_disc::geometry::{is_lit, lit_start};

    use super::{H, LIT, W};
    use crate::{HEIGHT, WIDTH};
    use crate::pages::{ATKINSON, POPPINS};

    fn faces() -> impl Iterator<Item = &'static Face> {
        [&POPPINS, &ATKINSON]
            .into_iter()
            .flat_map(|f| [f.words, f.id, f.charge, f.time])
            .flat_map(|font| [font.face, font.figures])
    }

    #[test]
    fn the_lit_rows_are_the_disc() {
        for y in 0..H {
            let lit = i32::from(LIT[usize::try_from(y).unwrap()]);
            assert_eq!(lit, lit_start(y, W, H).min(W / 2), "row {y}");
            for x in 0..W {
                assert_eq!((lit..W - lit).contains(&x), is_lit(x, y, W, H), "{x},{y}");
            }
        }
    }

    #[test]
    fn every_glyph_lands_where_incised_text_draws_it() {
        let ink = Abgr2222::from_levels(3, 2, 1);
        let ramp = [1, 2, 3].map(|level| ink.shade(Abgr2222::BLACK, level).0);
        let style = Style::left(ink, Abgr2222::BLACK);
        for face in faces() {
            for n in face.nodes {
                let Some(c) = char::from_u32(u32::from(n.cp)) else { continue };
                let mut text = [0u8; 4];
                let text = c.encode_utf8(&mut text);
                for x in [-40, -3, 0, 5, 101, 196, 230, 239, 250] {
                    for baseline in [-5, 0, 20, 64, 140, 222, 239, 260] {
                        let x0 = x + i32::from(n.left);
                        for columns in [0..W, x0 + 1..x0 + 4, 119..121, -9..3] {
                            let mut want = vec![0x55u8; usize::try_from(W * H).unwrap()];
                            let mut got = want.clone();
                            let width = u32::try_from(columns.end - columns.start).unwrap_or(0);
                            let area = Rectangle::new(Point::new(columns.start, 0), Size::new(width, HEIGHT));
                            let mut surface = Surface::<Abgr2222>::round(&mut want, WIDTH, HEIGHT).unwrap();
                            let _ = face.draw(&mut surface.clipped(&area), text, x, baseline, style);
                            super::node(&mut got, face, n, (x0, baseline - i32::from(n.top)), ramp, &columns);
                            assert!(got == want, "{} px {c:?} at {x},{baseline} in {columns:?}", face.px);
                        }
                    }
                }
            }
        }
    }
}
