//! The watch's timings for the BlueNRG-2, most preferred first in each situation.
//!
//! What the phone is asked for, and the cable's advertising, come from the driver's policy
//! ([`wrist_wake::ble::timing`]), so the two can't drift apart.

use wrist_ble::{AdvParams, ConnParams, Duration};
use wrist_ble_core::goals::Power;
use wrist_ble_core::plan::{Choice, Menu, When};
use wrist_wake::ble::{self as wake, timing, Timing};

const CABLE: Timing = timing(wake::Power::Cable, false);
const BATTERY: Timing = timing(wake::Power::Battery, false);

const fn adv(ms_x8: u16) -> AdvParams {
    AdvParams { interval_min: Duration::from_u16(ms_x8), interval_max: Duration::from_u16(ms_x8), connectable: true }
}

const fn link(min: u16, max: u16, latency: u16, timeout: u16) -> ConnParams {
    ConnParams { interval_min: Duration::from_u16(min), interval_max: Duration::from_u16(max), latency, timeout: Duration::from_u16(timeout), ce_len: Duration::from_u16(4) }
}

/// `ble interval now <min> <max>`'s ask, at the driver's latency and supervision timeout.
#[must_use]
pub const fn interval_now(min: u16, max: u16) -> ConnParams {
    link(min, max, wake::NOW_LATENCY, wake::NOW_TIMEOUT)
}

/// The phone link a [`Timing`] asks for.
const fn ask(t: Timing) -> ConnParams {
    link(t.interval_min, t.interval_max, t.latency, t.timeout)
}

const fn when(power: Option<Power>, beside_central: Option<bool>, idle: Option<bool>) -> When {
    When { power, beside_central, idle }
}

/// Every advertising interval offered beside a central link fits the menu's central links
/// (PM0257 §4.3, guidelines 2b and 4b). On battery that is 1592 units, not the driver's 1636,
/// which doesn't fit beside them (the driver has no central links).
pub const MENU: Menu = Menu {
    advertising: &[
        Choice { params: adv(CABLE.advertise), when: when(Some(Power::Cable), Some(false), Some(false)), cost_ua: 150 },
        Choice { params: adv(792), when: when(Some(Power::Cable), None, None), cost_ua: 30 },
        Choice { params: adv(1592), when: when(None, None, Some(false)), cost_ua: 15 },
        Choice { params: adv(3992), when: when(None, None, None), cost_ua: 6 },
    ],
    connecting: &[Choice { params: link(40, 40, 0, 400), when: when(None, None, None), cost_ua: 60 }],
    live: &[
        Choice { params: link(400, 400, 0, 600), when: when(None, None, None), cost_ua: 7 },
        Choice { params: link(200, 200, 0, 600), when: when(None, None, None), cost_ua: 14 },
    ],
    phone: &[
        Choice { params: ask(CABLE), when: when(Some(Power::Cable), None, None), cost_ua: 120 },
        Choice { params: ask(BATTERY), when: when(Some(Power::Battery), None, None), cost_ua: 8 },
    ],
};
