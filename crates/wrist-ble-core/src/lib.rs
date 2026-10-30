#![no_std]

//! The Bluetooth subsystem's decisions, pure and sans-IO: goals, the machines for the chip, the
//! phone and each peripheral, the planner's interface, and the status every display renders.

pub mod chip;
pub mod goals;
pub mod invariants;
pub mod peripheral;
pub mod phone;
pub mod plan;
pub mod profile;
pub mod status;

use wrist_ble::{AdvParams, Instant};

use crate::chip::ChipMachine;
use crate::goals::{Want, MAX_PERIPHERALS};
use crate::peripheral::Peripheral;
use crate::phone::Phone;
use crate::plan::{PlanHealth, Verdict};

/// Everything the core knows, which [`status::status`] describes.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub chip: ChipMachine,
    pub phone: Phone,
    pub phone_verdict: Verdict,
    /// As the radio last reported it.
    pub advertising: Option<AdvParams>,
    /// When connectable advertising last stopped, or the chip became ready without it.
    pub silent_since: Instant,
    pub peers: [Option<Peer>; MAX_PERIPHERALS],
    pub plan: PlanHealth,
    /// Since when the radio has differed from the plan.
    pub drift_since: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub want: Want,
    pub machine: Peripheral,
}

impl State {
    pub const OFF: Self = Self {
        chip: ChipMachine::OFF,
        phone: Phone::Off,
        phone_verdict: Verdict::NotWanted,
        advertising: None,
        silent_since: Instant(0),
        peers: [None; MAX_PERIPHERALS],
        plan: PlanHealth::Ok,
        drift_since: None,
    };
}
