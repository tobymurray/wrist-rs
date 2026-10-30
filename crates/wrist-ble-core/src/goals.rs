use wrist_ble::{Millis, PeerAddr};

use crate::profile::Profile;

/// The most peripherals wanted at once.
pub const MAX_PERIPHERALS: usize = 3;

/// What the user and the rest of the device want of Bluetooth, never chip state.
#[derive(Clone, Copy, Debug)]
pub struct Goals {
    pub radio_on: bool,
    pub phone: PhoneGoal,
    pub peripherals: [Option<Want>; MAX_PERIPHERALS],
    pub scan: Option<ScanGoal>,
    pub transfer: bool,
    pub power: Power,
    pub workout: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Power {
    Cable,
    Battery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhoneGoal {
    pub discoverable: bool,
    pub pairing_open: bool,
}

/// One peripheral the user chose.
#[derive(Clone, Copy, Debug)]
pub struct Want {
    pub profile: &'static Profile,
    pub peer: PeerAddr,
    /// Lower is kept first when not all fit.
    pub priority: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanGoal {
    pub duration: Millis,
}

impl Goals {
    pub const OFF: Self = Self {
        radio_on: false,
        phone: PhoneGoal { discoverable: true, pairing_open: false },
        peripherals: [None; MAX_PERIPHERALS],
        scan: None,
        transfer: false,
        power: Power::Battery,
        workout: false,
    };
}
