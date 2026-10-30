use crate::{
    AdvParams, AttRef, BluetoothUuid, CharRef, ConnHandle, ConnParams, DisconnectReason, Payload, PeerAddr, SchedKind, ScanParams,
    ServerAttr, ServiceRef,
};

/// One thing the core asks of the radio, ending in [`crate::RadioEvent::Done`] or in the typed
/// event [`OpKind::ends_with_event`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `None` stops advertising.
    Advertise(Option<AdvParams>),
    /// `None` stops a scan early.
    Scan(Option<ScanParams>),
    Connect(PeerAddr, ConnParams),
    CancelConnect,
    Disconnect(ConnHandle, DisconnectReason),
    /// As the central, a link-layer update; as the peripheral, a request to the central.
    UpdateParams(ConnHandle, ConnParams),
    /// As the central, accept or refuse a peer's [`crate::RadioEvent::PeerParamRequest`].
    AnswerParamRequest(ConnHandle, bool),
    RequestSecurity(ConnHandle),
    /// Let a peer whose bond the chip lost pair again.
    AllowRebond(ConnHandle),
    ConfirmNumeric(ConnHandle, bool),
    Passkey(ConnHandle, u32),
    Discover(ConnHandle, DiscoverWhat),
    Read(ConnHandle, AttRef),
    Write(ConnHandle, AttRef, Payload),
    Subscribe(ConnHandle, CharRef, Cccd),
    Notify(ConnHandle, ServerAttr, Payload),
    Bonds(BondOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    Advertise,
    Scan,
    Connect,
    CancelConnect,
    Disconnect,
    UpdateParams,
    AnswerParamRequest,
    RequestSecurity,
    AllowRebond,
    ConfirmNumeric,
    Passkey,
    Discover,
    Read,
    Write,
    Subscribe,
    Notify,
    Bonds,
}

impl Op {
    #[must_use]
    pub const fn kind(&self) -> OpKind {
        match self {
            Op::Advertise(_) => OpKind::Advertise,
            Op::Scan(_) => OpKind::Scan,
            Op::Connect(..) => OpKind::Connect,
            Op::CancelConnect => OpKind::CancelConnect,
            Op::Disconnect(..) => OpKind::Disconnect,
            Op::UpdateParams(..) => OpKind::UpdateParams,
            Op::AnswerParamRequest(..) => OpKind::AnswerParamRequest,
            Op::RequestSecurity(_) => OpKind::RequestSecurity,
            Op::AllowRebond(_) => OpKind::AllowRebond,
            Op::ConfirmNumeric(..) => OpKind::ConfirmNumeric,
            Op::Passkey(..) => OpKind::Passkey,
            Op::Discover(..) => OpKind::Discover,
            Op::Read(..) => OpKind::Read,
            Op::Write(..) => OpKind::Write,
            Op::Subscribe(..) => OpKind::Subscribe,
            Op::Notify(..) => OpKind::Notify,
            Op::Bonds(_) => OpKind::Bonds,
        }
    }

    /// The link the operation acts on, if it acts on one.
    #[must_use]
    pub const fn link(&self) -> Option<ConnHandle> {
        match *self {
            Op::Disconnect(l, _)
            | Op::UpdateParams(l, _)
            | Op::AnswerParamRequest(l, _)
            | Op::RequestSecurity(l)
            | Op::AllowRebond(l)
            | Op::ConfirmNumeric(l, _)
            | Op::Passkey(l, _)
            | Op::Discover(l, _)
            | Op::Read(l, _)
            | Op::Write(l, _, _)
            | Op::Subscribe(l, _, _)
            | Op::Notify(l, _, _) => Some(l),
            Op::Advertise(_) | Op::Scan(_) | Op::Connect(..) | Op::CancelConnect | Op::Bonds(_) => None,
        }
    }
}

impl OpKind {
    /// Kinds whose success is the typed event rather than `Done`: `Connect` ends in
    /// `LinkUp`, `Disconnect` in `LinkDown`, `UpdateParams` in `Params`, `RequestSecurity` in
    /// `Encryption` or a pairing step; a failure of any of them is still `Done` with an error.
    #[must_use]
    pub const fn ends_with_event(self) -> bool {
        matches!(self, OpKind::Connect | OpKind::Disconnect | OpKind::UpdateParams | OpKind::RequestSecurity)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiscoverWhat {
    /// A primary service by UUID: [`Outcome::Service`].
    Service(BluetoothUuid),
    /// A characteristic within a service, with its configuration descriptor:
    /// [`Outcome::Characteristic`].
    Characteristic(ServiceRef, BluetoothUuid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cccd {
    Off,
    Notify,
    Indicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BondOp {
    /// [`Outcome::Bonds`] counts them.
    List,
    Forget(PeerAddr),
    ForgetAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Outcome {
    Done,
    /// `None`: the peer has no such service.
    Service(Option<ServiceRef>),
    /// `None`: the service has no such characteristic.
    Characteristic(Option<CharRef>),
    Value(Payload),
    Bonds(u8),
}

/// A refusal or failure, chip codes left behind in the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Failure {
    pub kind: FailureKind,
    /// The chip's own code, for logs and counters only.
    pub native: u16,
}

/// What [`crate::Radio::submit`] returns when the chip will not start an operation.
pub type Refusal = Failure;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// The chip's scheduler would not fit it.
    Schedule(SchedKind),
    /// Not allowed in the chip's current mode, as advertising while initiating.
    ModeExclusion,
    Busy,
    NotConnected,
    PeerRejected,
    NoResources,
    /// No outcome by the driver's deadline.
    Timeout,
    Security,
    /// The link failed as it was being set up (HCI 0x3E).
    NotEstablished,
    Unknown,
}
