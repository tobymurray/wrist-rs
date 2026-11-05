//! An on/off switch drawn under a page's text, legible without it: the knob's side and the
//! track's colour say the state, as a phone's switch does.

use core::ops::Range;

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, CornerRadii, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle};
use inscribed_disc::Abgr2222;

/// Which way the switch stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
    /// Between the two, as while the radio starts: the knob in the middle, the track amber.
    Moving,
}

/// A deep green, as a phone's switch is on: the white knob stays the brightest thing on it.
const ON: Abgr2222 = Abgr2222::from_levels(0, 2, 1);

const TRACK_W: i32 = 140;
const TRACK_H: i32 = 64;
const TRACK_X: i32 = 120 - TRACK_W / 2;
const TRACK_Y: i32 = 84;
const STROKE: u32 = 3;
const KNOB_RADIUS: i32 = 24;
const KNOB_INSET: i32 = 8;

/// The panel rows the switch can touch.
pub const ROWS: Range<i32> = TRACK_Y..TRACK_Y + TRACK_H;

/// A baseline for a line under the switch.
pub const BELOW: i32 = TRACK_Y + TRACK_H + 34;

/// Draws `s` on `target`.
pub fn draw<D: DrawTarget<Color = Abgr2222>>(target: &mut D, s: Switch) {
    let (track, knob_cx) = match s {
        Switch::On => (ON, TRACK_X + TRACK_W - KNOB_INSET - KNOB_RADIUS),
        Switch::Off => (Abgr2222::GREY, TRACK_X + KNOB_INSET + KNOB_RADIUS),
        Switch::Moving => (Abgr2222::AMBER, TRACK_X + TRACK_W / 2),
    };
    let style = PrimitiveStyleBuilder::new().fill_color(track).stroke_color(Abgr2222::WHITE).stroke_width(STROKE).build();
    let _ = RoundedRectangle::new(
        Rectangle::new(Point::new(TRACK_X, TRACK_Y), Size::new(TRACK_W.unsigned_abs(), TRACK_H.unsigned_abs())),
        CornerRadii::new(Size::new((TRACK_H / 2).unsigned_abs(), (TRACK_H / 2).unsigned_abs())),
    )
    .into_styled(style)
    .draw(target);
    let _ = Circle::new(Point::new(knob_cx - KNOB_RADIUS, TRACK_Y + TRACK_H / 2 - KNOB_RADIUS), (2 * KNOB_RADIUS).unsigned_abs())
        .into_styled(PrimitiveStyle::with_fill(Abgr2222::WHITE))
        .draw(target);
}
