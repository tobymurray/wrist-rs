#![no_std]

//! The watch face, as bytes for the panel driver.

mod blit;
pub mod dump;
pub mod pages;
pub mod scene;
pub mod stress;
pub mod text;

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle};
use inscribed_disc::Abgr2222;

pub const WIDTH: u32 = 240;
pub const HEIGHT: u32 = 240;

/// Centred on the buffer, which is centred on the case (`HARDWARE.md`).
pub const RING_INSET: i32 = 2;
pub const RING_STROKE: u32 = 3;
/// How far inside the glass text keeps.
#[allow(clippy::cast_possible_wrap)]
pub const MARGIN: i32 = RING_INSET + RING_STROKE as i32 + 2;

pub const RING_COLOR: Abgr2222 = Abgr2222::from_levels(0, 2, 3);

pub fn ring<D: DrawTarget<Color = Abgr2222>>(target: &mut D) {
    let _ = Circle::new(
        Point::new(RING_INSET, RING_INSET),
        WIDTH - 2 * RING_INSET.unsigned_abs(),
    )
    .into_styled(PrimitiveStyle::with_stroke(RING_COLOR, RING_STROKE))
    .draw(target);
}

/// Each row's ring pixels, left run then right, as [`ring`] draws them on the glass.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub static RING_RUNS: [[(u8, u8); 2]; HEIGHT as usize] = {
    let (w, h) = (WIDTH as i32, HEIGHT as i32);
    let diameter = w - 2 * RING_INSET;
    let centre_2x = 2 * RING_INSET + diameter - 1;
    let outer = diameter + 2 * (RING_STROKE as i32 / 2);
    let inner = diameter - 2 * ((RING_STROKE as i32 + 1) / 2);
    let mut runs = [[(0u8, 0u8); 2]; HEIGHT as usize];
    let mut y = 0;
    while y < h {
        let mut run = 0;
        let mut x = 0;
        let mut inside = false;
        while x <= w {
            let (dx, dy) = (2 * x - centre_2x, 2 * y - centre_2x);
            let d2 = dx * dx + dy * dy;
            let on = x < w && d2 < outer * outer && d2 >= inner * inner && inscribed_disc::geometry::is_lit(x, y, w, h);
            if on && !inside && run < 2 {
                runs[y as usize][run].0 = x as u8;
            }
            if !on && inside && run < 2 {
                runs[y as usize][run].1 = x as u8;
                run += 1;
            }
            inside = on;
            x += 1;
        }
        y += 1;
    }
    runs
};
