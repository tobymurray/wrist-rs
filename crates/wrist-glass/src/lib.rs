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
use inscribed_disc::{Abgr2222, Surface};

pub const WIDTH: u32 = 240;
pub const HEIGHT: u32 = 240;

/// Centred on the buffer, which is centred on the case (`HARDWARE.md`).
pub const RING_INSET: i32 = 2;
pub const RING_STROKE: u32 = 3;
/// How far inside the glass text keeps.
#[allow(clippy::cast_possible_wrap)]
pub const MARGIN: i32 = RING_INSET + RING_STROKE as i32 + 2;

pub fn ring(surface: &mut Surface<'_, Abgr2222>) {
    let _ = Circle::new(
        Point::new(RING_INSET, RING_INSET),
        WIDTH - 2 * RING_INSET.unsigned_abs(),
    )
    .into_styled(PrimitiveStyle::with_stroke(
        Abgr2222::from_levels(0, 2, 3),
        RING_STROKE,
    ))
    .draw(surface);
}
