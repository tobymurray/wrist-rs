//! `ble probe`'s readings, one `key=value` line per reply.

use core::fmt;

use bluenrg2::opcode;
use bluenrg2::probe::{updater_version, AnchorPeriod, FirmwareDetails};

/// Microseconds in the 0.625 ms unit of the anchor period and advertising intervals.
const SLOT_US: u64 = 625;
/// Microseconds in the 1.25 ms unit of connection intervals.
const CONN_UNIT_US: u64 = 1250;

/// What else the radio was doing when the anchor period was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    Idle,
    /// Connectable advertising at this interval (0.625 ms units).
    Advertising { interval: u16 },
    /// A central's link at this interval (1.25 ms units) and latency.
    Link { interval: u16, latency: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Firmware,
    Updater,
    Anchor(Beside),
}

impl Probe {
    #[must_use]
    pub const fn opcode(self) -> u16 {
        match self {
            Self::Firmware => opcode::ACI_HAL_GET_FIRMWARE_DETAILS,
            Self::Updater => opcode::ACI_HAL_GET_UPDATER_VERSION,
            Self::Anchor(_) => opcode::ACI_HAL_GET_ANCHOR_PERIOD,
        }
    }
}

/// One probe's line: the command, then its reading, a refusal's status, or why there is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line<'a> {
    pub probe: Probe,
    /// The reply's status and the parameters after it; `None` when no reply came.
    pub reply: Option<(u8, &'a [u8])>,
}

impl fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("probe ")?;
        match self.probe {
            Probe::Firmware => f.write_str("firmware")?,
            Probe::Updater => f.write_str("updater")?,
            Probe::Anchor(Beside::Idle) => f.write_str("anchor beside=idle")?,
            Probe::Anchor(Beside::Advertising { interval }) => {
                write!(f, "anchor beside=advertising adv_interval_us={}", u64::from(interval) * SLOT_US)?;
            }
            Probe::Anchor(Beside::Link { interval, latency }) => {
                write!(f, "anchor beside=link conn_interval_us={} latency={latency}", u64::from(interval) * CONN_UNIT_US)?;
            }
        }
        let p = match self.reply {
            None => return f.write_str(" error=no-reply"),
            Some((status @ 1.., _)) => return write!(f, " status=0x{status:02x}"),
            Some((_, p)) => p,
        };
        let short = |f: &mut fmt::Formatter<'_>| write!(f, " error=short len={}", p.len());
        match self.probe {
            Probe::Firmware => match FirmwareDetails::decode(p) {
                Some(d) => write!(
                    f,
                    " dtm={}.{}.{} dtm_variant={} dtm_build={} stack={}.{}.{} stack_development={} stack_variant=0x{:04x} stack_build={}",
                    d.dtm_version[0],
                    d.dtm_version[1],
                    d.dtm_version[2],
                    d.dtm_variant,
                    d.dtm_build,
                    d.stack_version[0],
                    d.stack_version[1],
                    d.stack_version[2],
                    d.stack_development,
                    d.stack_variant,
                    d.stack_build,
                ),
                None => short(f),
            },
            Probe::Updater => match updater_version(p) {
                Some(v) => write!(f, " version={v}"),
                None => short(f),
            },
            Probe::Anchor(_) => match AnchorPeriod::decode(p) {
                Some(a) => write!(
                    f,
                    " anchor_period_us={} max_free_slot_us={}",
                    u64::from(a.anchor_period) * SLOT_US,
                    u64::from(a.max_free_slot) * SLOT_US
                ),
                None => short(f),
            },
        }
    }
}
