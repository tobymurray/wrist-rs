//! What the chip is and what its link-layer scheduler holds (PM0257 §4), one `key=value` line
//! per reply. Layouts from ST's `bluenrg1_types.h`.

use core::fmt;

use crate::opcode;

/// Microseconds in the 0.625 ms unit of the anchor period and advertising intervals.
const SLOT_US: u64 = 625;
/// Microseconds in the 1.25 ms unit of connection intervals.
const CONN_UNIT_US: u64 = 1250;

/// `ACI_HAL_GET_FIRMWARE_DETAILS`'s return parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareDetails {
    /// The network-coprocessor (DTM) application: major, minor, patch.
    pub dtm_version: [u8; 3],
    /// 1 UART, 2 SPI.
    pub dtm_variant: u8,
    pub dtm_build: u16,
    pub stack_version: [u8; 3],
    /// 0 an official release, 1 an internal development one.
    pub stack_development: u8,
    /// Privacy 0x01, Secure Connections 0x02, central 0x04, data length extension 0x08, link
    /// layer only 0x10.
    pub stack_variant: u16,
    pub stack_build: u16,
}

impl FirmwareDetails {
    const LEN: usize = 14;

    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let p = p.get(..Self::LEN)?;
        let at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        Some(Self {
            dtm_version: [p[0], p[1], p[2]],
            dtm_variant: p[3],
            dtm_build: at(4),
            stack_version: [p[6], p[7], p[8]],
            stack_development: p[9],
            stack_variant: at(10),
            stack_build: at(12),
        })
    }
}

/// `ACI_HAL_GET_ANCHOR_PERIOD`'s return parameters, in 0.625 ms units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorPeriod {
    pub anchor_period: u32,
    /// The longest slot a new link could still be given.
    pub max_free_slot: u32,
}

impl AnchorPeriod {
    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let p = p.get(..8)?;
        Some(Self {
            anchor_period: u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
            max_free_slot: u32::from_le_bytes([p[4], p[5], p[6], p[7]]),
        })
    }
}

/// `ACI_HAL_GET_UPDATER_VERSION`'s return parameter.
#[must_use]
pub fn updater_version(p: &[u8]) -> Option<u8> {
    p.first().copied()
}

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
