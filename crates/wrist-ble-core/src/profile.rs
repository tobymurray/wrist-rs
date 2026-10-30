use wrist_ble::{Millis, BluetoothUuid};

/// What one kind of peripheral differs by; the peripheral machine is otherwise the same for all.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub kind: Kind,
    pub service: BluetoothUuid,
    pub subscribe: &'static [BluetoothUuid],
    pub security: SecurityNeed,
    pub cadence: Cadence,
    pub silence: Silence,
    /// The unit [`Reading::value`] is shown in.
    pub unit: &'static str,
    /// No reading for this long is a quiet spell.
    pub quiet: Millis,
    /// A link this quiet is dropped and the peripheral looked for again, if silence is a fault.
    pub stale: Millis,
    /// `None` for a notification that is not a reading.
    pub parse: fn(&[u8]) -> Option<Reading>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    HeartRate,
    Ftms(Machine),
    Csc,
    Cps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Machine {
    Rower,
    IndoorBike,
    Treadmill,
}

impl Kind {
    /// The name in the console's status line.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Kind::HeartRate => "hr",
            Kind::Ftms(Machine::Rower) => "rower",
            Kind::Ftms(Machine::IndoorBike) => "bike",
            Kind::Ftms(Machine::Treadmill) => "treadmill",
            Kind::Csc => "csc",
            Kind::Cps => "cps",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecurityNeed {
    None,
    Encrypted,
}

/// How often a working peripheral sends a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cadence {
    Every(Millis),
    /// Only when something happens, as a crank turning.
    Events,
}

/// What a peripheral that sends nothing is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Silence {
    /// Something is wrong: a heart-rate sensor always has a beat to send.
    Fault,
    /// Nothing is happening: a crank at rest, a machine paused.
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reading {
    pub value: u16,
    pub contact: Contact,
}

/// Skin contact, for sensors that report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Contact {
    Unsupported,
    Lost,
    Detected,
}
