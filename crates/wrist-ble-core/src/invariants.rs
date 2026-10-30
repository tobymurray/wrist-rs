//! The properties the core keeps, checked on the host and alarmed on the device.

use wrist_ble::Millis;

/// I1: the longest planned gap in connectable advertising with no phone link.
pub const REACHABLE_GAP: Millis = Millis::secs(3);

/// I1: planned gaps may add up to this share of any [`REACHABLE_WINDOW`].
pub const REACHABLE_SHARE_PERCENT: u32 = 5;
pub const REACHABLE_WINDOW: Millis = Millis::secs(60);

/// I1: a gap this long raises the alarm and recovers to the safe baseline.
pub const REACHABLE_ALARM: Millis = Millis::secs(10);

/// I2: chip commands a minute, outside GATT traffic and transfers.
pub const COMMANDS_PER_MINUTE: u32 = 60;

/// I3: with goals and power still this long, the radio has reached the plan or says why not.
pub const CONVERGE_AFTER: Millis = Millis::secs(15);

/// I4: the status reflects a change of state within this.
pub const STATUS_WITHIN: Millis = Millis::secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Invariant {
    Reachable,
    BoundedEffort,
    Converges,
    TruthfulStatus,
    SecurityOrder,
    PhoneFirst,
    NothingSilent,
    NoExecutorHolds,
    PeripheralsEnd,
}
