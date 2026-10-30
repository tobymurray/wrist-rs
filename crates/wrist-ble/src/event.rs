use crate::{
    AdvData, AttRef, Cccd, ConnHandle, CoreSpecificationVersion, Failure, Instant, LeConnRole, LinkParams, Micros, OpId, Outcome,
    ParamRange, Payload, PeerAddr, ServerAttr, Status, MAX_LINKS,
};

/// Everything the radio reports; every chip packet becomes exactly one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioEvent {
    Ready(ChipInfo),
    /// The chip reset itself: every link is gone and every pending operation with it.
    ChipReset(ResetCause),
    Done(OpId, Result<Outcome, Failure>),
    LinkUp { link: ConnHandle, role: LeConnRole, peer: PeerAddr, params: LinkParams },
    LinkDown { link: ConnHandle, reason: Status },
    /// A parameter update completed, with an error too.
    Params { link: ConnHandle, result: Result<LinkParams, Failure> },
    Encryption { link: ConnHandle, on: bool },
    Pairing(PairingEvent),
    /// The peer has no bond for a link the chip had bonded keys for.
    BondLost(ConnHandle),
    Mtu(ConnHandle, u16),
    /// As the central, the peer asked for new parameters; answer with
    /// [`crate::Op::AnswerParamRequest`].
    PeerParamRequest { link: ConnHandle, range: ParamRange },
    Notification { link: ConnHandle, att: AttRef, data: Payload },
    ServerWrite { link: ConnHandle, attr: ServerAttr, data: Payload },
    /// A peer turned notifications or indications of a local characteristic on or off.
    ServerSubscribe { link: ConnHandle, attr: ServerAttr, cccd: Cccd },
    Report(AdvReport),
    ScanDone,
    /// The chip dropped events: the core's view may be wrong until a [`Snapshot`].
    EventsLost,
    Unknown { raw_code: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChipInfo {
    pub hci_version: CoreSpecificationVersion,
    pub lmp_version: CoreSpecificationVersion,
    pub manufacturer: u16,
    pub firmware: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResetCause {
    PowerOn,
    Watchdog,
    Lockup,
    Brownout,
    Crash,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PairingEvent {
    pub link: ConnHandle,
    pub step: PairingStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PairingStep {
    /// Show this six-digit value and answer with [`crate::Op::ConfirmNumeric`].
    Numeric(u32),
    /// Answer with [`crate::Op::Passkey`].
    PasskeyWanted,
    /// Keys exchanged; [`RadioEvent::Encryption`] follows.
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvReport {
    pub peer: PeerAddr,
    pub rssi: i8,
    pub connectable: bool,
    pub scan_response: bool,
    pub data: AdvData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LinkStatus {
    pub link: ConnHandle,
    pub role: LeConnRole,
    pub peer: PeerAddr,
}

/// The central time base, from chips that report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub period: Micros,
    pub max_free_slot: Micros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Snapshot {
    pub at: Instant,
    pub links: [Option<LinkStatus>; MAX_LINKS],
    pub advertising: bool,
    pub scanning: bool,
    pub initiating: bool,
    pub anchor: Option<Anchor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// No answer within the driver's deadline.
    Silent,
    Refused(Failure),
    /// The transport to the chip failed.
    Bus,
}
