//! An on/off switch drawn under a page's text, legible without it: the knob's side and the
//! track's colour say the state, as a phone's switch does.

use core::ops::Range;

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, CornerRadii, PrimitiveStyle, Rectangle, RoundedRectangle};
use inscribed_disc::Abgr2222;

/// Which way the switch stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
    /// Between the two, the knob in the middle.
    Moving,
}

/// A deep green, as a phone's switch is on: the white knob stays the brightest thing on it.
const ON: Abgr2222 = Abgr2222::from_levels(0, 2, 1);

const TRACK_W: i32 = 96;
const TRACK_H: i32 = 54;
const TRACK_X: i32 = 120 - TRACK_W / 2;
const TRACK_Y: i32 = 116 - TRACK_H / 2;
const STROKE: i32 = 3;
const KNOB_INSET: i32 = TRACK_H / 8;
const KNOB_RADIUS: i32 = TRACK_H / 2 - KNOB_INSET;

/// The panel rows the switch can touch.
pub const ROWS: Range<i32> = TRACK_Y..TRACK_Y + TRACK_H;

/// A baseline for a line under the switch.
pub const BELOW: i32 = 182;

/// Draws `s` on `target`.
pub fn draw<D: DrawTarget<Color = Abgr2222>>(target: &mut D, s: Switch) {
    let (track, knob_cx) = match s {
        Switch::On => (ON, TRACK_X + TRACK_W - KNOB_INSET - KNOB_RADIUS),
        Switch::Off => (Abgr2222::GREY, TRACK_X + KNOB_INSET + KNOB_RADIUS),
        Switch::Moving => (Abgr2222::AMBER, TRACK_X + TRACK_W / 2),
    };
    for (inset, colour) in [(0, Abgr2222::WHITE), (STROKE, track)] {
        let (w, h) = (TRACK_W - 2 * inset, TRACK_H - 2 * inset);
        let _ = RoundedRectangle::new(
            Rectangle::new(Point::new(TRACK_X + inset, TRACK_Y + inset), Size::new(w.unsigned_abs(), h.unsigned_abs())),
            CornerRadii::new(Size::new((h / 2).unsigned_abs(), (h / 2).unsigned_abs())),
        )
        .into_styled(PrimitiveStyle::with_fill(colour))
        .draw(target);
    }
    let _ = Circle::new(Point::new(knob_cx - KNOB_RADIUS, TRACK_Y + TRACK_H / 2 - KNOB_RADIUS), (2 * KNOB_RADIUS).unsigned_abs())
        .into_styled(PrimitiveStyle::with_fill(Abgr2222::WHITE))
        .draw(target);
}
