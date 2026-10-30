#![no_std]

//! The boundary between the Bluetooth core and a chip driver, above GAP and GATT: the driver
//! takes operations, reports events, and declares what the chip can do.

mod event;
mod gatt;
mod op;
mod schedule;
mod time;

pub use event::{AdvReport, Anchor, ChipInfo, Fault, LinkStatus, PairingEvent, PairingStep, RadioEvent, ResetCause, Snapshot};
pub use gatt::{AttRef, CharDecl, CharRef, Permission, Props, ServerAttr, ServerTable, ServiceDecl, ServiceRef};
pub use op::{BondOp, Cccd, DiscoverWhat, Failure, FailureKind, Op, OpKind, Outcome, Refusal};
pub use schedule::{
    Activities, Admitted, Capabilities, CentralSlot, Change, Exclusion, GattLimits, ParamAnswer, RoleSet, SchedKind, Schedule,
};
pub use time::{Instant, Micros, Millis};

pub use bt_hci;
pub use bt_hci::param::{AddrKind, BdAddr, ConnHandle, CoreSpecificationVersion, DisconnectReason, Duration, LeConnRole, Status};
pub use bt_hci::uuid::BluetoothUuid;

/// The most links any driver reports; a chip's own limit is [`Capabilities::max_links`].
pub const MAX_LINKS: usize = 8;

/// An accepted operation, until its terminal [`RadioEvent::Done`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpId(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerAddr {
    pub kind: AddrKind,
    pub addr: BdAddr,
}

/// Up to `N` bytes held inline.
#[derive(Clone, Copy)]
pub struct Bytes<const N: usize> {
    len: u8,
    data: [u8; N],
}

impl<const N: usize> Bytes<N> {
    pub const EMPTY: Self = Self { len: 0, data: [0; N] };

    /// `None` if `bytes` is longer than `N`.
    #[must_use]
    pub fn new(bytes: &[u8]) -> Option<Self> {
        let len = u8::try_from(bytes.len()).ok().filter(|&l| usize::from(l) <= N)?;
        let mut data = [0; N];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self { len, data })
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

impl<const N: usize> PartialEq for Bytes<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<const N: usize> Eq for Bytes<N> {}

impl<const N: usize> core::fmt::Debug for Bytes<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.as_slice().fmt(f)
    }
}

/// An attribute value: the largest an ATT MTU of 247 carries (Core Vol 3 Part F §3.2.9).
pub type Payload = Bytes<244>;

/// Legacy advertising or scan response data (Core Vol 6 Part B §2.3).
pub type AdvData = Bytes<31>;

/// A connection interval, in its 1.25 ms units.
pub type Interval = Duration<1_250>;

/// A supervision timeout, in its 10 ms units.
pub type Timeout = Duration<10_000>;

/// Advertising and scan timing, and connection event length, in 0.625 ms units.
pub type Slots = Duration<625>;

/// Connection parameters asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnParams {
    pub interval_min: Interval,
    pub interval_max: Interval,
    pub latency: u16,
    pub timeout: Timeout,
    pub ce_len: Slots,
}

/// A link's parameters as granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkParams {
    pub interval: Interval,
    pub latency: u16,
    pub timeout: Timeout,
}

/// What a peer asked for in a connection parameter request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParamRange {
    pub interval_min: Interval,
    pub interval_max: Interval,
    pub latency: u16,
    pub timeout: Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdvParams {
    pub interval_min: Slots,
    pub interval_max: Slots,
    pub connectable: bool,
}

/// A scan; it ends with [`RadioEvent::ScanDone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanParams {
    pub interval: Slots,
    pub window: Slots,
    pub active: bool,
    pub duration: Millis,
}

/// Implemented by each chip driver.
///
/// Every accepted operation reaches a terminal outcome by [`Capabilities::op_deadline`], every
/// chip packet becomes exactly one [`RadioEvent`], and no call holds the executor while the chip
/// works on air.
#[allow(async_fn_in_trait, reason = "single-threaded executor; no Send bound is wanted")]
pub trait Radio {
    /// Fixed once `start` returns.
    fn capabilities(&self) -> &Capabilities;

    fn schedule(&self) -> &dyn Schedule;

    /// Reset, configure and register `server`.
    ///
    /// # Errors
    ///
    /// The chip did not come up, or refused the table.
    async fn start(&mut self, server: &ServerTable) -> Result<ChipInfo, Fault>;

    /// Accepted or refused at once; completion arrives as an event.
    ///
    /// # Errors
    ///
    /// The chip refused the operation without starting it.
    async fn submit(&mut self, op: Op) -> Result<OpId, Refusal>;

    async fn next(&mut self) -> RadioEvent;

    /// The chip's own account of its links, for resync and audit.
    ///
    /// # Errors
    ///
    /// The chip did not answer.
    async fn snapshot(&mut self) -> Result<Snapshot, Fault>;
}
