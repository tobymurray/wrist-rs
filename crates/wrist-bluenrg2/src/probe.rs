//! `ble probe`'s readings, one `key=value` line per reply.

use core::fmt;

use bluenrg2::opcode;
use bluenrg2::hal::{updater_version, AnchorPeriod, FirmwareDetails};
use wrist_ble::{AddrKind, Fault, LeConnRole, LinkStatus, Snapshot};

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
            Probe::Anchor(Beside::Link { interval: 0, .. }) => f.write_str("anchor beside=link conn_interval=unknown")?,
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

/// `ble probe snapshot`'s lines: the activities and anchor, or why there is no snapshot; then one
/// per link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken<'a> {
    Snapshot(&'a Result<Snapshot, Fault>),
    Link(&'a LinkStatus),
}

impl fmt::Display for Taken<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("probe snapshot")?;
        match *self {
            Self::Snapshot(Ok(s)) => {
                write!(f, " advertising={} scanning={} initiating={}", u8::from(s.advertising), u8::from(s.scanning), u8::from(s.initiating))?;
                match s.anchor {
                    Some(a) => write!(f, " anchor_period_us={} max_free_slot_us={}", a.period.0, a.max_free_slot.0),
                    None => f.write_str(" anchor=none"),
                }
            }
            Self::Snapshot(Err(Fault::Silent)) => f.write_str(" error=no-reply"),
            Self::Snapshot(Err(Fault::Bus)) => f.write_str(" error=not-taken"),
            Self::Snapshot(Err(Fault::Refused(r))) => write!(f, " status=0x{:02x}", r.native),
            Self::Link(l) => {
                let role = if l.role == LeConnRole::Central { "central" } else { "peripheral" };
                let kind = if l.peer.kind == AddrKind::RANDOM { "random" } else { "public" };
                let a = l.peer.addr.0;
                write!(
                    f,
                    " link=0x{:04x} role={role} peer={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} peer_type={kind}",
                    l.link.raw(),
                    a[5],
                    a[4],
                    a[3],
                    a[2],
                    a[1],
                    a[0]
                )?;
                match l.encrypted {
                    Some(on) => write!(f, " encrypted={}", u8::from(on)),
                    None => f.write_str(" encrypted=unknown"),
                }
            }
        }
    }
}

/// `ble probe matrix`'s advertising intervals, each asked as min = max (DESIGN.md §5.2). The
/// chip advertises 5 ms longer than asked (PM0257 §4.2.3), so all but 975 ms make a round period.
pub const MATRIX_ADVERTISING_MS: [u16; 6] = [95, 100, 495, 995, 2495, 975];

/// The intervals `ble probe matrix` asks of the central for the link, min = max.
pub const MATRIX_LINK_MS: [u16; 5] = [35, 50, 100, 250, 500];

/// The supervision timeout each update asks for: long enough for the longest interval.
pub const MATRIX_TIMEOUT_MS: u16 = 5_000;

/// Which of a cell's two changes comes first; the other is made beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// The link re-timed, then advertising started beside it.
    LinkFirst,
    /// Advertising started, then the link re-timed beside it.
    AdvertisingFirst,
}

/// One cell of the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub index: u8,
    pub order: Order,
    pub adv_ms: u16,
    pub link_ms: u16,
}

impl Cell {
    /// Every cell, both orders.
    #[allow(clippy::cast_possible_truncation, reason = "6 x 5 x 2")]
    pub const COUNT: u8 = (MATRIX_ADVERTISING_MS.len() * MATRIX_LINK_MS.len() * 2) as u8;

    /// The `index`th cell: the order outermost, the link's interval innermost, so that no cell
    /// asks for the interval the link already has.
    #[must_use]
    pub fn nth(index: u8) -> Option<Self> {
        let i = usize::from(index);
        let per_order = MATRIX_ADVERTISING_MS.len() * MATRIX_LINK_MS.len();
        let order = match i / per_order {
            0 => Order::LinkFirst,
            1 => Order::AdvertisingFirst,
            _ => return None,
        };
        let i = i % per_order;
        Some(Self { index, order, adv_ms: MATRIX_ADVERTISING_MS[i / MATRIX_LINK_MS.len()], link_ms: MATRIX_LINK_MS[i % MATRIX_LINK_MS.len()] })
    }

    /// The advertising interval in 0.625 ms units.
    #[must_use]
    pub const fn adv_units(self) -> u16 {
        self.adv_ms * 8 / 5
    }

    /// The link's interval in 1.25 ms units.
    #[must_use]
    pub const fn link_units(self) -> u16 {
        self.link_ms * 4 / 5
    }
}

/// What came of asking the central for the cell's interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// `ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ`'s status; `None` when it had no reply.
    pub status: Option<u8>,
    /// The central's answer.
    pub accepted: Option<bool>,
    /// LE Connection Update Complete: its status, and on success the interval (1.25 ms units)
    /// and latency.
    pub done: Option<(u8, u16, u16)>,
}

/// One cell's line: each change's outcome in the order made, the anchor period read after each,
/// and whether the link was still up at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matrix<'a> {
    pub cell: Cell,
    pub update: Update,
    /// `ACI_GAP_SET_DISCOVERABLE`'s status; `None` when it had no reply.
    pub advertising: Option<u8>,
    /// `ACI_HAL_GET_ANCHOR_PERIOD`'s reply after the first change and after the second.
    pub anchors: [Option<(u8, &'a [u8])>; 2],
    pub link_up: bool,
}

impl fmt::Display for Matrix<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.cell;
        let order = match c.order {
            Order::LinkFirst => "link-first",
            Order::AdvertisingFirst => "adv-first",
        };
        write!(f, "probe matrix cell={} order={order} adv_ms={} link_ms={}", c.index, c.adv_ms, c.link_ms)?;
        let update = |f: &mut fmt::Formatter<'_>| -> fmt::Result {
            let u = &self.update;
            match u.status {
                Some(s) => write!(f, " update=0x{s:02x}")?,
                None => f.write_str(" update=none")?,
            }
            match u.accepted {
                Some(true) => f.write_str(" answer=accepted")?,
                Some(false) => f.write_str(" answer=rejected")?,
                None => f.write_str(" answer=none")?,
            }
            match u.done {
                Some((0, interval, latency)) => write!(f, " done=0x00 interval_us={} latency={latency}", u64::from(interval) * CONN_UNIT_US),
                Some((s, ..)) => write!(f, " done=0x{s:02x}"),
                None => f.write_str(" done=none"),
            }
        };
        let advertising = |f: &mut fmt::Formatter<'_>| match self.advertising {
            Some(s) => write!(f, " adv=0x{s:02x}"),
            None => f.write_str(" adv=none"),
        };
        let anchor = |f: &mut fmt::Formatter<'_>, n: usize| match self.anchors[n - 1] {
            Some((0, p)) => match AnchorPeriod::decode(p) {
                Some(a) => write!(
                    f,
                    " anchor{n}_us={} free{n}_us={}",
                    u64::from(a.anchor_period) * SLOT_US,
                    u64::from(a.max_free_slot) * SLOT_US
                ),
                None => write!(f, " anchor{n}=short"),
            },
            Some((s, _)) => write!(f, " anchor{n}=0x{s:02x}"),
            None => write!(f, " anchor{n}=none"),
        };
        match c.order {
            Order::LinkFirst => {
                update(f)?;
                anchor(f, 1)?;
                advertising(f)?;
            }
            Order::AdvertisingFirst => {
                advertising(f)?;
                anchor(f, 1)?;
                update(f)?;
            }
        }
        anchor(f, 2)?;
        f.write_str(if self.link_up { " link=up" } else { " link=lost" })
    }
}
