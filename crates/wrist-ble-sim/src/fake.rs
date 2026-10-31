//! A fake BlueNRG-2 on virtual time, with the phones and sensors it meets.
//!
//! The fake refuses what [`wrist_bluenrg2::Scheduler`] refuses, with the chip's own codes, so the
//! planner and the fake read one model. Around it, each behaviour the watch and the rigs measured
//! is modelled where it is cited: the chip's events per connection, the exclusion while
//! initiating, discovery ending by itself, answers to parameter requests sent as refusals, 0x3E,
//! MIC failures when a procedure crosses encryption, the lost-bond order, ATT round trips that
//! scale with the interval, and 0x28 when a central start lands on a pending update.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use wrist_ble::{
    Activities, AdvParams, AdvReport, AttRef, Bytes, Cccd, Change, ChipInfo, ConnHandle, ConnParams, CoreSpecificationVersion, DiscoverWhat,
    Duration, Failure, FailureKind, Instant, LeConnRole, LinkParams, LinkStatus, Millis, Op, OpId, OpKind, Outcome, PairingEvent,
    PairingStep, PeerAddr, RadioEvent, ResetCause, ScanParams, SchedKind, Schedule, ServerAttr, ServiceRef, CharRef, Snapshot, Status,
    MAX_LINKS,
};
use wrist_bluenrg2::{adv_period, interval_ns, native, Scheduler, CAPABILITIES};

use crate::peers::{Keys, Os, Phone, Sensor};
use crate::rng::Rng;

pub const INFO: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0x30,
    firmware: 2353,
};

/// The console's characteristic in the local table, which a phone subscribes to.
pub const CONSOLE: ServerAttr = ServerAttr(1);

/// The link-layer payloads the chip reports on each link, from the host's side of a capture: the
/// host sends 251 octets and receives 27.
const DATA_LENGTH: (u16, u16) = (27, 251);

/// Connection events before a link that never formed is dropped (Core Vol 6 Part B §4.5.2).
const ESTABLISH_EVENTS: u64 = 6;

/// Connection events from an update's request to its instant.
const INSTANT_EVENTS: u64 = 6;

/// From reset to ready (HARDWARE.md: within 3 s).
const BOOT: (u32, u32) = (200, 600);

/// A refusal the model does not predict, injected to test the core's answer to one.
pub type Divergence = Box<dyn Fn(&Activities, &Change) -> Option<SchedKind>>;

/// What happened on air, for the checks that read the truth rather than the core's view of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "a record kept by tests")]
pub enum Note {
    Submitted { op: Op, accepted: bool },
    /// A link-layer procedure started on a link whose encryption was pending.
    CrossedEncryption(ConnHandle),
    /// A central start while the phone's link had an update pending.
    CentralInUpdate(ConnHandle),
    /// The chip's answer to a peer's parameter request went on air as a refusal.
    RejectedOnAir(ConnHandle),
    LinkDown { link: ConnHandle, reason: Status },
    Reset,
    Unpredicted(Change),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Peer {
    Phone(usize),
    Sensor(usize),
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "each is one fact about the link on air")]
struct Link {
    handle: ConnHandle,
    role: LeConnRole,
    peer: Peer,
    params: LinkParams,
    /// The central slot this link holds in the time base.
    slot: Option<u8>,
    encrypted: bool,
    /// Encryption started on air and not yet done.
    encrypting: bool,
    /// A procedure was in flight or started while encrypting: the link ends in a MIC failure.
    doomed: bool,
    /// An update this device asked for, until its instant.
    updating: Option<OpId>,
    /// The peer's own update, until its instant.
    peer_update_until: Option<Instant>,
    subscribed: bool,
    pairing: bool,
}

#[derive(Clone, Copy, Debug)]
enum Do {
    Ready,
    Done(OpId, Result<Outcome, Failure>),
    Deadline(OpId),
    Emit(RadioEvent),
    PhoneConnects(usize),
    EncryptStart(ConnHandle),
    EncryptDone(ConnHandle),
    Mtu(ConnHandle),
    ConsoleOn(ConnHandle),
    PeerUpdate(ConnHandle, u16),
    PeerUpdateApplied(ConnHandle, u16),
    UpdateApplied(ConnHandle, LinkParams, OpId),
    UpdateFailed(ConnHandle, OpId),
    PairRequest(ConnHandle),
    Confirm(ConnHandle),
    Lost(ConnHandle, Status),
    SensorConnects(usize, OpId),
    SensorAsks(ConnHandle),
    Reading(ConnHandle),
    Report(u64),
    ScanEnds(u64),
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    id: OpId,
    op: Op,
}

#[derive(Clone, Copy, Debug)]
struct SensorState {
    sensor: Sensor,
    on_since: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct PhoneState {
    phone: Phone,
    wants: bool,
    link: Option<ConnHandle>,
    gone: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Power {
    Off,
    Booting,
    Ready,
    /// Reset by itself and waiting to be started again.
    Halted,
}

/// The fake chip and the air around it. Time moves only through [`Fake::advance`].
pub struct Fake {
    now: Instant,
    rng: Rng,
    power: Power,
    acts: Activities,
    links: [Option<Link>; MAX_LINKS],
    next_handle: u16,
    next_op: u16,
    ops: Vec<Pending>,
    agenda: BinaryHeap<Reverse<(Instant, u64, Item)>>,
    seq: u64,
    out: VecDeque<(Instant, RadioEvent)>,
    phones: Vec<PhoneState>,
    sensors: Vec<SensorState>,
    scan_gen: u64,
    initiating: Option<(OpId, PeerAddr, ConnParams)>,
    divergences: Vec<Divergence>,
    /// Events dropped from here on until this many have gone.
    lose: usize,
    pub notes: Vec<(Instant, Note)>,
}

/// An agenda entry, ordered by time then by when it was scheduled.
#[derive(Clone, Copy, Debug)]
struct Item(Do);

impl PartialEq for Item {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Item {
    fn cmp(&self, _: &Self) -> std::cmp::Ordering {
        std::cmp::Ordering::Equal
    }
}

fn ms(n: u32) -> Millis {
    Millis(n)
}

fn interval_ms(p: LinkParams) -> u32 {
    u32::try_from(interval_ns(p.interval) / 1_000_000).unwrap_or(u32::MAX).max(1)
}

fn failure(kind: FailureKind, native: u16) -> Failure {
    Failure { kind, native }
}

impl Fake {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            now: Instant(0),
            rng: Rng::new(seed),
            power: Power::Off,
            acts: Activities::IDLE,
            links: [None; MAX_LINKS],
            next_handle: 0x0801,
            next_op: 1,
            ops: Vec::new(),
            agenda: BinaryHeap::new(),
            seq: 0,
            out: VecDeque::new(),
            phones: Vec::new(),
            sensors: Vec::new(),
            scan_gen: 0,
            initiating: None,
            divergences: Vec::new(),
            lose: 0,
            notes: Vec::new(),
        }
    }

    #[must_use]
    pub fn now(&self) -> Instant {
        self.now
    }

    /// What the chip's scheduler holds now.
    #[must_use]
    pub fn activities(&self) -> &Activities {
        &self.acts
    }

    #[must_use]
    pub fn ready(&self) -> bool {
        self.power == Power::Ready
    }

    /// Connectable advertising on air.
    #[must_use]
    pub fn advertising(&self) -> Option<AdvParams> {
        self.acts.advertising.filter(|a| a.connectable)
    }

    #[must_use]
    pub fn phone_link(&self) -> Option<(ConnHandle, bool)> {
        self.links.iter().flatten().find(|l| l.role == LeConnRole::Peripheral).map(|l| (l.handle, l.encrypted))
    }

    /// Whether `link` has encryption under way on air.
    #[must_use]
    pub fn encrypting(&self, link: ConnHandle) -> bool {
        self.link(link).is_some_and(|l| l.encrypting)
    }

    /// The link's parameters, if it is up.
    #[must_use]
    pub fn params(&self, link: ConnHandle) -> Option<LinkParams> {
        self.link(link).map(|l| l.params)
    }

    /// The earliest time something happens by itself.
    #[must_use]
    pub fn next_at(&self) -> Option<Instant> {
        let queued = self.out.front().map(|(at, _)| *at);
        let agenda = self.agenda.peek().map(|Reverse((at, _, _))| *at);
        [queued, agenda].into_iter().flatten().min()
    }

    /// The next event due by `until`, with its time; the clock moves to it.
    pub fn advance(&mut self, until: Instant) -> Option<(Instant, RadioEvent)> {
        loop {
            if let Some(e) = self.out.pop_front() {
                return Some(e);
            }
            let Reverse((at, _, _)) = *self.agenda.peek()?;
            if at > until {
                self.now = self.now.max(until);
                return None;
            }
            let Reverse((at, _, Item(what))) = self.agenda.pop()?;
            self.now = self.now.max(at);
            self.happen(what);
        }
    }

    // --- The world, scripted by tests ---

    /// The chip is reset and brought up: [`RadioEvent::Ready`] follows.
    pub fn start(&mut self) {
        self.wipe();
        self.power = Power::Booting;
        let boot = self.rng.range(BOOT.0, BOOT.1);
        self.after(ms(boot), Do::Ready);
    }

    /// Held in reset.
    pub fn stop(&mut self) {
        self.wipe();
        self.power = Power::Off;
    }

    /// The chip resets by itself: every link and operation is gone.
    pub fn reset(&mut self, cause: ResetCause) {
        if self.power == Power::Off {
            return;
        }
        self.wipe();
        self.power = Power::Halted;
        self.notes.push((self.now, Note::Reset));
        self.emit(RadioEvent::ChipReset(cause));
    }

    /// The next `n` events are lost; the chip says so.
    pub fn lose_events(&mut self, n: usize) {
        self.lose = n;
    }

    /// A packet the driver does not know.
    pub fn unknown(&mut self, raw_code: u16) {
        self.emit(RadioEvent::Unknown { raw_code });
    }

    pub fn diverge(&mut self, d: Divergence) {
        self.divergences.push(d);
    }

    /// A phone wanting to connect; it connects at the next advertising event.
    pub fn phone_arrives(&mut self, phone: Phone) -> usize {
        let i = self.phones.iter().position(|p| p.phone.addr == phone.addr).unwrap_or_else(|| {
            self.phones.push(PhoneState { phone, wants: false, link: None, gone: false });
            self.phones.len() - 1
        });
        let p = &mut self.phones[i];
        p.phone = phone;
        p.wants = true;
        p.gone = false;
        self.try_phone(i);
        i
    }

    /// The phone hangs up (0x13).
    pub fn phone_leaves(&mut self, i: usize) {
        self.phones[i].wants = false;
        if let Some(h) = self.phones[i].link {
            let at = ms(self.rng.range(5, 30));
            self.after(at, Do::Lost(h, Status::REMOTE_USER_TERMINATED_CONN));
        }
    }

    /// The phone goes out of range: its link times out (0x08).
    pub fn phone_vanishes(&mut self, i: usize) {
        self.phones[i].wants = false;
        self.phones[i].gone = true;
        if let Some(l) = self.phones[i].link.and_then(|h| self.link(h)) {
            self.after(Millis(u32::from(l.params.timeout.as_u16()) * 10), Do::Lost(l.handle, Status::CONN_TIMEOUT));
        }
    }

    pub fn sensor_on(&mut self, sensor: Sensor) -> usize {
        let now = self.now;
        if let Some(i) = self.sensors.iter().position(|s| s.sensor.addr == sensor.addr) {
            self.sensors[i] = SensorState { sensor, on_since: Some(now) };
            return i;
        }
        self.sensors.push(SensorState { sensor, on_since: Some(now) });
        self.sensors.len() - 1
    }

    /// The sensor stops: a link to it times out (0x08).
    pub fn sensor_off(&mut self, i: usize) {
        self.sensors[i].on_since = None;
        let links: Vec<_> = self.links.iter().flatten().filter(|l| l.peer == Peer::Sensor(i)).map(|l| (l.handle, l.params.timeout)).collect();
        for (h, timeout) in links {
            self.after(Millis(u32::from(timeout.as_u16()) * 10), Do::Lost(h, Status::CONN_TIMEOUT));
        }
    }

    // --- The radio ---

    /// Accepted or refused at once, as the trait's `submit`.
    ///
    /// # Errors
    ///
    /// The chip refused it.
    pub fn submit(&mut self, op: Op) -> Result<OpId, Failure> {
        let r = self.take(op);
        self.notes.push((self.now, Note::Submitted { op, accepted: r.is_ok() }));
        r
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let mut links = [None; MAX_LINKS];
        for (out, l) in links.iter_mut().zip(self.links.iter().flatten()) {
            *out = Some(LinkStatus { link: l.handle, role: l.role, peer: self.peer_addr(l.peer) });
        }
        Snapshot {
            at: self.now,
            links,
            advertising: self.acts.advertising.is_some(),
            scanning: self.acts.scanning.is_some(),
            initiating: self.acts.initiating.is_some(),
            anchor: None,
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm an operation")]
    fn take(&mut self, op: Op) -> Result<OpId, Failure> {
        if self.power != Power::Ready {
            return Err(failure(FailureKind::Busy, 0x0C));
        }
        if let Some(l) = op.link() {
            if self.link(l).is_none() {
                return Err(failure(FailureKind::NotConnected, 0x02));
            }
        }
        match op {
            Op::Advertise(p) => {
                self.admit(Change::Advertise(p))?;
                let id = self.accept(op);
                self.set_adv(p);
                self.done_after(id, ms(1), Ok(Outcome::Done));
                Ok(id)
            }
            Op::Scan(Some(p)) => {
                self.admit(Change::Scan(Some(p)))?;
                let id = self.accept(op);
                self.start_scan(p);
                self.done_after(id, ms(1), Ok(Outcome::Done));
                Ok(id)
            }
            Op::Scan(None) => {
                let id = self.accept(op);
                if self.acts.scanning.take().is_some() {
                    self.scan_gen += 1;
                    self.emit_after(ms(2), RadioEvent::ScanDone);
                }
                self.done_after(id, ms(1), Ok(Outcome::Done));
                Ok(id)
            }
            Op::Connect(peer, params) => {
                if self.initiating.is_some() {
                    return Err(failure(FailureKind::Busy, 0x0C));
                }
                self.admit(Change::Initiate(Some(params)))?;
                self.central_start();
                let id = self.accept(op);
                self.acts.initiating = Some(params);
                self.initiating = Some((id, peer, params));
                self.try_sensor_connect();
                Ok(id)
            }
            Op::CancelConnect => {
                let Some((cid, ..)) = self.initiating.take() else { return Err(failure(FailureKind::Busy, 0x0C)) };
                self.acts.initiating = None;
                let id = self.accept(op);
                self.done_after(cid, ms(1), Err(failure(FailureKind::NotConnected, 0x02)));
                self.done_after(id, ms(1), Ok(Outcome::Done));
                Ok(id)
            }
            Op::Disconnect(l, _) => {
                let id = self.accept(op);
                let p = self.link(l).map_or(LinkParams { interval: Duration::from_u16(40), latency: 0, timeout: Duration::from_u16(100) }, |l| l.params);
                let at = interval_ms(p) * self.rng.range(1, 2);
                self.after(ms(at), Do::Lost(l, Status::CONN_TERMINATED_BY_LOCAL_HOST));
                Ok(id)
            }
            Op::UpdateParams(l, params) => self.update(op, l, params),
            Op::AnswerParamRequest(l, _) => {
                let id = self.accept(op);
                self.notes.push((self.now, Note::RejectedOnAir(l)));
                self.done_after(id, ms(1), Ok(Outcome::Done));
                Ok(id)
            }
            Op::RequestSecurity(l) => self.request_security(op, l),
            Op::AllowRebond(_) | Op::Passkey(..) | Op::Bonds(_) | Op::Notify(..) => {
                let id = self.accept(op);
                let outcome = if let Op::Bonds(_) = op { Outcome::Bonds(u8::from(self.phones.iter().any(|p| p.phone.keys == Keys::Bonded))) } else { Outcome::Done };
                self.done_after(id, ms(1), Ok(outcome));
                Ok(id)
            }
            Op::ConfirmNumeric(l, yes) => {
                let id = self.accept(op);
                self.done_after(id, ms(1), Ok(Outcome::Done));
                self.answered(l, yes);
                Ok(id)
            }
            Op::Discover(l, what) => {
                let id = self.accept(op);
                let outcome = self.discover(l, what);
                let trips = match what {
                    DiscoverWhat::Service(_) => 6,
                    DiscoverWhat::Characteristic(..) => 10,
                };
                self.gatt_after(id, l, trips, Ok(outcome));
                Ok(id)
            }
            Op::Read(l, _) | Op::Write(l, ..) => {
                let id = self.accept(op);
                let outcome = if let Op::Read(..) = op { Outcome::Value(Bytes::EMPTY) } else { Outcome::Done };
                self.gatt_after(id, l, 1, Ok(outcome));
                Ok(id)
            }
            Op::Subscribe(l, _, cccd) => {
                let id = self.accept(op);
                self.gatt_after(id, l, 1, Ok(Outcome::Done));
                if let Some(link) = self.link_mut(l) {
                    link.subscribed = cccd != Cccd::Off;
                }
                if cccd != Cccd::Off {
                    let first = self.gatt_time(l, 1) + ms(self.rng.range(0, 1_000));
                    self.after(first, Do::Reading(l));
                }
                Ok(id)
            }
        }
    }

    /// Admitted by the model and by every injected divergence, or refused with the chip's code.
    fn admit(&mut self, change: Change) -> Result<(), Failure> {
        let refused = Scheduler.admit(&self.acts, &change).err().map(|k| (k, false));
        let refused = refused.or_else(|| self.divergences.iter().find_map(|d| d(&self.acts, &change)).map(|k| (k, true)));
        match refused {
            None => Ok(()),
            Some((kind, unpredicted)) => {
                if unpredicted {
                    self.notes.push((self.now, Note::Unpredicted(change)));
                }
                let k = match kind {
                    SchedKind::Excluded(_) => FailureKind::ModeExclusion,
                    k => FailureKind::Schedule(k),
                };
                Err(failure(k, native(kind)))
            }
        }
    }

    fn accept(&mut self, op: Op) -> OpId {
        let id = OpId(self.next_op);
        self.next_op = self.next_op.wrapping_add(1).max(1);
        self.ops.push(Pending { id, op });
        let deadline = (CAPABILITIES.op_deadline)(op.kind());
        self.after(deadline, Do::Deadline(id));
        id
    }

    fn end(&mut self, id: OpId) -> Option<Op> {
        let i = self.ops.iter().position(|p| p.id == id)?;
        Some(self.ops.remove(i).op)
    }

    fn pending(&self, kind: OpKind, link: ConnHandle) -> Option<OpId> {
        self.ops.iter().find(|p| p.op.kind() == kind && p.op.link() == Some(link)).map(|p| p.id)
    }

    fn set_adv(&mut self, p: Option<AdvParams>) {
        self.acts.advertising = p;
        for i in 0..self.phones.len() {
            self.try_phone(i);
        }
    }

    fn start_scan(&mut self, p: ScanParams) {
        self.central_start();
        self.acts.scanning = Some(p);
        self.scan_gen += 1;
        let gen = self.scan_gen;
        let end = CAPABILITIES.discovery_max.map_or(p.duration, |max| p.duration.min(max));
        self.after(end, Do::ScanEnds(gen));
        self.later(20, 200, Do::Report(gen));
    }

    /// A central start beside a phone link whose update is pending loses that link's instant
    /// (PM0257 §4.2.5's round robin; LIKELY: the phone's link was measured ending in 0x28 when a
    /// connection began 0.2 s after it connected).
    fn central_start(&mut self) {
        let now = self.now;
        let hit: Vec<ConnHandle> = self
            .links
            .iter()
            .flatten()
            .filter(|l| l.role == LeConnRole::Peripheral && (l.updating.is_some() || l.peer_update_until.is_some_and(|t| t > now)))
            .map(|l| l.handle)
            .collect();
        for h in hit {
            self.notes.push((now, Note::CentralInUpdate(h)));
            self.after(ms(10), Do::Lost(h, Status::INSTANT_PASSED));
        }
    }

    fn update(&mut self, op: Op, l: ConnHandle, params: ConnParams) -> Result<OpId, Failure> {
        let link = self.link(l).ok_or(failure(FailureKind::NotConnected, 0x02))?;
        if link.updating.is_some() {
            return Err(failure(FailureKind::Busy, 0x0C));
        }
        if let Some(slot) = link.slot {
            self.admit(Change::Retime { slot, params })?;
        }
        self.procedure(l);
        let id = self.accept(op);
        if let Some(link) = self.link_mut(l) {
            link.updating = Some(id);
        }
        let iv = interval_ms(link.params);
        let lead = iv * u32::try_from(INSTANT_EVENTS).unwrap_or(6) + self.rng.range(0, iv);
        match link.peer {
            Peer::Phone(i) if self.phones[i].phone.update_fails => self.after(ms(lead), Do::UpdateFailed(l, id)),
            Peer::Phone(i) => {
                let granted = self.phones[i].phone.grant(params.interval_min, params.interval_max);
                let p = LinkParams { interval: granted, latency: params.latency, timeout: params.timeout };
                self.after(ms(lead), Do::UpdateApplied(l, p, id));
            }
            Peer::Sensor(_) => {
                let slot = link.slot.and_then(|slot| self.acts.walk(&Scheduler, &[Change::Retime { slot, params }]).ok().and_then(|a| a.central[usize::from(slot)]));
                let interval = slot.map_or(params.interval_max, |s| s.interval);
                let p = LinkParams { interval, latency: params.latency, timeout: params.timeout };
                self.after(ms(lead), Do::UpdateApplied(l, p, id));
            }
        }
        Ok(id)
    }

    fn request_security(&mut self, op: Op, l: ConnHandle) -> Result<OpId, Failure> {
        let link = self.link(l).ok_or(failure(FailureKind::NotConnected, 0x02))?;
        self.procedure(l);
        let id = self.accept(op);
        match link.peer {
            Peer::Phone(i) => {
                let phone = self.phones[i].phone;
                if !link.encrypted && !link.encrypting && !link.pairing {
                    match phone.keys {
                        Keys::Bonded => self.later(20, 60, Do::EncryptStart(l)),
                        _ => self.later(20, 100, Do::PairRequest(l)),
                    }
                }
            }
            Peer::Sensor(_) => {
                let iv = interval_ms(link.params);
                self.after(ms(iv), Do::EncryptStart(l));
            }
        }
        Ok(id)
    }

    /// A procedure on `l`: if encryption is under way it crosses it.
    fn procedure(&mut self, l: ConnHandle) {
        let now = self.now;
        if let Some(link) = self.link_mut(l) {
            if link.encrypting {
                link.doomed = true;
                self.notes.push((now, Note::CrossedEncryption(l)));
            }
        }
    }

    fn discover(&self, l: ConnHandle, what: DiscoverWhat) -> Outcome {
        let sensor = self.link(l).and_then(|link| match link.peer {
            Peer::Sensor(i) => Some(self.sensors[i]),
            Peer::Phone(_) => None,
        });
        let has = sensor.filter(|s| s.on_since.is_some_and(|since| self.now.since(since) >= s.sensor.service_after));
        match what {
            DiscoverWhat::Service(uuid) => Outcome::Service(has.filter(|s| s.sensor.service == uuid).map(|_| ServiceRef { start: AttRef(0x10), end: AttRef(0x1F) })),
            DiscoverWhat::Characteristic(_, uuid) => {
                Outcome::Characteristic(has.filter(|s| s.sensor.characteristic == uuid).map(|_| CharRef { value: AttRef(0x12), cccd: Some(AttRef(0x13)) }))
            }
        }
    }

    /// One ATT round trip takes one or two connection events, so discovery at 500 ms takes many seconds.
    fn gatt_time(&mut self, l: ConnHandle, trips: u32) -> Millis {
        let iv = self.link(l).map_or(50, |link| interval_ms(link.params));
        let mut t = 0;
        for _ in 0..trips {
            t += iv * self.rng.range(1, 2);
        }
        ms(t)
    }

    fn gatt_after(&mut self, id: OpId, l: ConnHandle, trips: u32, r: Result<Outcome, Failure>) {
        let t = self.gatt_time(l, trips);
        self.done_after(id, t, r);
    }

    fn answered(&mut self, l: ConnHandle, yes: bool) {
        let Some(link) = self.link(l) else { return };
        if !link.pairing {
            return;
        }
        if yes {
            if let Peer::Phone(i) = link.peer {
                if let Some(after) = self.phones[i].phone.confirms_after {
                    self.after(after, Do::Confirm(l));
                }
            }
        } else {
            self.pairing_failed(l);
        }
    }

    fn pairing_failed(&mut self, l: ConnHandle) {
        if let Some(link) = self.link_mut(l) {
            link.pairing = false;
        }
        self.end_security(l);
        self.emit(RadioEvent::Pairing(PairingEvent { link: l, step: PairingStep::Failed }));
    }

    /// A pending security request ends once its pairing or encryption does.
    fn end_security(&mut self, l: ConnHandle) {
        if let Some(id) = self.pending(OpKind::RequestSecurity, l) {
            self.end(id);
        }
    }

    // --- What happens by itself ---

    #[allow(clippy::too_many_lines, reason = "one arm a happening")]
    fn happen(&mut self, what: Do) {
        match what {
            Do::Ready => {
                if self.power == Power::Booting {
                    self.power = Power::Ready;
                    self.emit(RadioEvent::Ready(INFO));
                }
            }
            Do::Done(id, r) => {
                if self.end(id).is_some() {
                    self.emit(RadioEvent::Done(id, r));
                }
            }
            Do::Deadline(id) => {
                if self.initiating.is_some_and(|(cid, ..)| cid == id) {
                    self.initiating = None;
                    self.acts.initiating = None;
                }
                if self.end(id).is_some() {
                    self.emit(RadioEvent::Done(id, Err(failure(FailureKind::Timeout, 0xFF))));
                }
            }
            Do::Emit(e) => self.emit(e),
            Do::PhoneConnects(i) => self.phone_connects(i),
            Do::EncryptStart(l) => {
                if let Some(link) = self.link_mut(l) {
                    if link.encrypted || link.encrypting {
                        return;
                    }
                    link.encrypting = true;
                    link.doomed |= link.updating.is_some();
                    let crossed = link.doomed;
                    let peer = link.peer;
                    if crossed {
                        self.notes.push((self.now, Note::CrossedEncryption(l)));
                    }
                    let done = match peer {
                        Peer::Phone(i) => {
                            let ((start, _), (lo, hi)) = self.phones[i].phone.encryption();
                            self.rng.range(lo - start, hi - start)
                        }
                        Peer::Sensor(_) => self.link(l).map_or(100, |k| interval_ms(k.params) * 3),
                    };
                    self.after(ms(done), Do::EncryptDone(l));
                }
            }
            Do::EncryptDone(l) => self.encrypted(l),
            Do::Mtu(l) => {
                if self.link(l).is_some() {
                    self.emit(RadioEvent::Mtu(l, CAPABILITIES.gatt.mtu));
                }
            }
            Do::ConsoleOn(l) => {
                if self.link(l).is_some() {
                    self.emit(RadioEvent::ServerSubscribe { link: l, attr: CONSOLE, cccd: Cccd::Notify });
                }
            }
            Do::PeerUpdate(l, units) => {
                let now = self.now;
                let Some(link) = self.link_mut(l) else { return };
                let iv = interval_ms(link.params);
                let until = now + ms(iv * u32::try_from(INSTANT_EVENTS).unwrap_or(6));
                link.peer_update_until = Some(until);
                self.agenda_at(until, Do::PeerUpdateApplied(l, units));
            }
            Do::PeerUpdateApplied(l, units) => {
                let Some(link) = self.link_mut(l) else { return };
                link.peer_update_until = None;
                link.params.interval = Duration::from_u16(units);
                let p = link.params;
                self.emit(RadioEvent::Params { link: l, result: Ok(p) });
            }
            Do::UpdateApplied(l, p, id) => {
                let Some(link) = self.link_mut(l) else { return };
                if link.updating != Some(id) {
                    return;
                }
                link.updating = None;
                link.params = p;
                let slot = link.slot;
                if let Some(s) = slot {
                    if let Some(c) = self.acts.central[usize::from(s)].as_mut() {
                        c.interval = p.interval;
                    }
                }
                self.end(id);
                self.emit(RadioEvent::Params { link: l, result: Ok(p) });
            }
            Do::UpdateFailed(l, id) => {
                let Some(link) = self.link_mut(l) else { return };
                if link.updating != Some(id) {
                    return;
                }
                link.updating = None;
                self.end(id);
                self.emit(RadioEvent::Params { link: l, result: Err(failure(FailureKind::PeerRejected, 0x3B)) });
            }
            Do::PairRequest(l) => self.pair_request(l),
            Do::Confirm(l) => {
                let Some(link) = self.link_mut(l) else { return };
                if !link.pairing {
                    return;
                }
                link.pairing = false;
                if let Some(Peer::Phone(i)) = self.link(l).map(|k| k.peer) {
                    self.phones[i].phone.keys = Keys::Bonded;
                }
                self.later(20, 60, Do::EncryptStart(l));
            }
            Do::Lost(l, reason) => self.link_down(l, reason),
            Do::SensorConnects(i, id) => self.sensor_connects(i, id),
            Do::SensorAsks(l) => {
                let Some(link) = self.link(l) else { return };
                let Peer::Sensor(i) = link.peer else { return };
                if let Some((_, range)) = self.sensors[i].sensor.asks {
                    self.emit(RadioEvent::PeerParamRequest { link: l, range });
                }
            }
            Do::Reading(l) => self.reading(l),
            Do::Report(gen) => self.report(gen),
            Do::ScanEnds(gen) => {
                if gen == self.scan_gen && self.acts.scanning.take().is_some() {
                    self.emit(RadioEvent::ScanDone);
                }
            }
        }
    }

    fn try_phone(&mut self, i: usize) {
        let p = self.phones[i];
        if !p.wants || p.link.is_some() || p.gone || self.power != Power::Ready {
            return;
        }
        if let Some(adv) = self.advertising() {
            let period = u32::try_from(adv_period(&adv) / 1_000_000).unwrap_or(1_000);
            let at = self.rng.range(1, period);
            self.after(ms(at), Do::PhoneConnects(i));
        }
    }

    fn phone_connects(&mut self, i: usize) {
        let p = self.phones[i];
        if !p.wants || p.link.is_some() || p.gone || self.power != Power::Ready || self.advertising().is_none() {
            return;
        }
        if Scheduler.admit(&self.acts, &Change::PeripheralUp).is_err() {
            return;
        }
        let units = match p.phone.os {
            Os::Android => 6,
            Os::Mac => 12,
            Os::Linux => 24,
        };
        let params = LinkParams { interval: Duration::from_u16(units), latency: 0, timeout: Duration::from_u16(500) };
        let Some(h) = self.new_link(LeConnRole::Peripheral, Peer::Phone(i), params, None) else { return };
        self.phones[i].link = Some(h);
        self.acts.advertising = None;
        self.acts.peripheral += 1;
        self.emit(RadioEvent::LinkUp { link: h, role: LeConnRole::Peripheral, peer: p.phone.addr, params });
        self.later(90, 110, Do::Emit(RadioEvent::DataLength { link: h, tx: DATA_LENGTH.0, rx: DATA_LENGTH.1 }));
        match p.phone.keys {
            Keys::Bonded | Keys::ChipLost => {
                let ((lo, hi), _) = p.phone.encryption();
                self.later(lo, hi, Do::EncryptStart(h));
            }
            Keys::PhoneLost => self.later(50, 100, Do::PairRequest(h)),
            Keys::None => {}
        }
        if p.phone.os == Os::Android {
            for (at, units) in [(750, 6), (1_100, 6), (1_900, 36)] {
                self.after(ms(at), Do::PeerUpdate(h, units));
            }
        }
    }

    fn pair_request(&mut self, l: ConnHandle) {
        let Some(link) = self.link(l) else { return };
        if link.pairing || link.encrypted {
            return;
        }
        let Peer::Phone(i) = link.peer else { return };
        let phone = self.phones[i].phone;
        if phone.keys == Keys::PhoneLost {
            let lost = RadioEvent::BondLost(l);
            let failed = RadioEvent::Pairing(PairingEvent { link: l, step: PairingStep::Failed });
            let (a, b) = if phone.bond_lost_first { (lost, failed) } else { (failed, lost) };
            self.emit(a);
            let gap = self.rng.range(5, 2_000);
            self.after(ms(gap), Do::Emit(b));
            self.phones[i].phone.keys = Keys::None;
            self.end_security(l);
            return;
        }
        if let Some(link) = self.link_mut(l) {
            link.pairing = true;
        }
        let value = self.rng.range(0, 999_999);
        self.emit(RadioEvent::Pairing(PairingEvent { link: l, step: PairingStep::Numeric(value) }));
    }

    fn encrypted(&mut self, l: ConnHandle) {
        let Some(link) = self.link_mut(l) else { return };
        if !link.encrypting {
            return;
        }
        link.encrypting = false;
        if link.doomed {
            self.link_down(l, Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE);
            return;
        }
        let peer = link.peer;
        if let Peer::Phone(i) = peer {
            if self.phones[i].phone.keys == Keys::ChipLost {
                self.end_security(l);
                self.emit(RadioEvent::Encryption { link: l, on: false });
                return;
            }
        }
        if let Some(link) = self.link_mut(l) {
            link.encrypted = true;
        }
        self.end_security(l);
        if let Peer::Phone(i) = peer {
            if self.phones[i].phone.keys == Keys::Bonded {
                self.emit(RadioEvent::Pairing(PairingEvent { link: l, step: PairingStep::Complete }));
            }
        }
        self.emit(RadioEvent::Encryption { link: l, on: true });
        if let Peer::Phone(i) = peer {
            self.later(30, 100, Do::Mtu(l));
            if self.phones[i].phone.console {
                self.later(300, 450, Do::ConsoleOn(l));
            }
        }
    }

    fn sensor_connects(&mut self, i: usize, id: OpId) {
        let Some((cid, peer, params)) = self.initiating else { return };
        if cid != id || self.sensors[i].sensor.addr != peer || self.sensors[i].on_since.is_none() {
            return;
        }
        let Ok(admitted) = Scheduler.admit(&self.acts, &Change::Connected) else {
            self.initiating = None;
            self.acts.initiating = None;
            self.end(id);
            self.emit(RadioEvent::Done(id, Err(failure(FailureKind::Schedule(SchedKind::LengthFailed), 0x86))));
            return;
        };
        let Ok(next) = self.acts.with(&Change::Connected, &admitted) else { return };
        let slot = next.central.iter().zip(self.acts.central.iter()).position(|(n, o)| n.is_some() && o.is_none());
        self.acts = next;
        self.initiating = None;
        let interval = admitted.slot.map_or(params.interval_max, |s| s.interval);
        let lp = LinkParams { interval, latency: params.latency, timeout: params.timeout };
        let slot = slot.and_then(|s| u8::try_from(s).ok());
        let already = self.links.iter().flatten().any(|l| l.peer == Peer::Sensor(i));
        let Some(h) = self.new_link(LeConnRole::Central, Peer::Sensor(i), lp, slot) else { return };
        self.end(id);
        let s = self.sensors[i].sensor;
        self.emit(RadioEvent::LinkUp { link: h, role: LeConnRole::Central, peer, params: lp });
        let iv = interval_ms(lp);
        if already || self.rng.chance(s.not_established_permille) {
            self.after(ms(iv * u32::try_from(ESTABLISH_EVENTS).unwrap_or(6)), Do::Lost(h, Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT));
            return;
        }
        let dl = ms(self.rng.range(90, 110).max(iv * 2));
        self.after(dl, Do::Emit(RadioEvent::DataLength { link: h, tx: DATA_LENGTH.0, rx: DATA_LENGTH.1 }));
        if let Some((after, _)) = s.asks {
            self.after(after, Do::SensorAsks(h));
        }
    }

    fn try_sensor_connect(&mut self) {
        let Some((id, peer, _)) = self.initiating else { return };
        let Some(i) = self.sensors.iter().position(|s| s.sensor.addr == peer && s.on_since.is_some()) else { return };
        let every = self.sensors[i].sensor.adv_every.0;
        let at = self.rng.range(1, every.max(1));
        self.after(ms(at), Do::SensorConnects(i, id));
    }

    fn reading(&mut self, l: ConnHandle) {
        let Some(link) = self.link(l) else { return };
        let Peer::Sensor(i) = link.peer else { return };
        if !link.subscribed {
            return;
        }
        let s = self.sensors[i];
        if s.on_since.is_none() {
            return;
        }
        if self.rng.chance(u16::from(s.sensor.delivers_percent) * 10) {
            let data = Bytes::new(&[0x06, s.sensor.value]).unwrap_or(Bytes::EMPTY);
            self.emit(RadioEvent::Notification { link: l, att: AttRef(0x12), data });
        }
        self.after(s.sensor.reading_every, Do::Reading(l));
    }

    fn report(&mut self, gen: u64) {
        if gen != self.scan_gen || self.acts.scanning.is_none() {
            return;
        }
        let on: Vec<Sensor> = self.sensors.iter().filter(|s| s.on_since.is_some()).map(|s| s.sensor).collect();
        for s in on {
            let data = Bytes::new(&[0x03, 0x03, 0x0D, 0x18]).unwrap_or(Bytes::EMPTY);
            self.emit(RadioEvent::Report(AdvReport { peer: s.addr, rssi: -60, connectable: true, scan_response: false, data }));
        }
        let every = self.advertising().map_or(500, |a| u32::try_from(adv_period(&a) / 1_000_000).unwrap_or(1_000));
        self.after(ms(every), Do::Report(gen));
    }

    fn link_down(&mut self, l: ConnHandle, reason: Status) {
        let Some(i) = self.links.iter().position(|k| k.is_some_and(|k| k.handle == l)) else { return };
        let Some(link) = self.links[i].take() else { return };
        if let Some(s) = link.slot {
            self.acts.central[usize::from(s)] = None;
        }
        if link.role == LeConnRole::Peripheral {
            self.acts.peripheral = self.acts.peripheral.saturating_sub(1);
        }
        let ended: Vec<OpId> = self.ops.iter().filter(|p| p.op.link() == Some(l)).map(|p| p.id).collect();
        let disconnect = self.pending(OpKind::Disconnect, l);
        for id in ended {
            self.end(id);
            if Some(id) != disconnect {
                self.emit(RadioEvent::Done(id, Err(failure(FailureKind::NotConnected, 0x02))));
            }
        }
        if let Peer::Phone(p) = link.peer {
            self.phones[p].link = None;
        }
        self.notes.push((self.now, Note::LinkDown { link: l, reason }));
        self.emit(RadioEvent::LinkDown { link: l, reason });
        if let Peer::Phone(p) = link.peer {
            self.try_phone(p);
        }
    }

    // --- Plumbing ---

    fn new_link(&mut self, role: LeConnRole, peer: Peer, params: LinkParams, slot: Option<u8>) -> Option<ConnHandle> {
        let free = self.links.iter().position(Option::is_none)?;
        let handle = ConnHandle::new(self.next_handle);
        self.next_handle = if self.next_handle >= 0x0EFF { 0x0801 } else { self.next_handle + 1 };
        self.links[free] = Some(Link {
            handle,
            role,
            peer,
            params,
            slot,
            encrypted: false,
            encrypting: false,
            doomed: false,
            updating: None,
            peer_update_until: None,
            subscribed: false,
            pairing: false,
        });
        Some(handle)
    }

    fn link(&self, l: ConnHandle) -> Option<Link> {
        self.links.iter().flatten().find(|k| k.handle == l).copied()
    }

    fn link_mut(&mut self, l: ConnHandle) -> Option<&mut Link> {
        self.links.iter_mut().flatten().find(|k| k.handle == l)
    }

    fn peer_addr(&self, p: Peer) -> PeerAddr {
        match p {
            Peer::Phone(i) => self.phones[i].phone.addr,
            Peer::Sensor(i) => self.sensors[i].sensor.addr,
        }
    }

    fn wipe(&mut self) {
        self.acts = Activities::IDLE;
        self.links = [None; MAX_LINKS];
        self.ops.clear();
        self.agenda.clear();
        self.out.clear();
        self.initiating = None;
        self.scan_gen += 1;
        for p in &mut self.phones {
            p.link = None;
        }
    }

    fn emit(&mut self, e: RadioEvent) {
        if self.lose > 0 {
            self.lose -= 1;
            if self.lose == 0 {
                self.out.push_back((self.now, RadioEvent::EventsLost));
            }
            return;
        }
        self.out.push_back((self.now, e));
    }

    fn emit_after(&mut self, d: Millis, e: RadioEvent) {
        self.after(d, Do::Emit(e));
    }

    fn done_after(&mut self, id: OpId, d: Millis, r: Result<Outcome, Failure>) {
        self.after(d, Do::Done(id, r));
    }

    /// After a time drawn from `lo..=hi` ms.
    fn later(&mut self, lo: u32, hi: u32, what: Do) {
        let d = ms(self.rng.range(lo, hi));
        self.after(d, what);
    }

    fn after(&mut self, d: Millis, what: Do) {
        let at = self.now + d;
        self.agenda_at(at, what);
    }

    fn agenda_at(&mut self, at: Instant, what: Do) {
        self.seq += 1;
        self.agenda.push(Reverse((at, self.seq, Item(what))));
    }
}

#[allow(clippy::unused_async_trait_impl, reason = "the trait's shape; the fake never waits")]
impl wrist_ble::Radio for Fake {
    fn capabilities(&self) -> &wrist_ble::Capabilities {
        &CAPABILITIES
    }

    fn schedule(&self) -> &dyn Schedule {
        &Scheduler
    }

    async fn start(&mut self, _: &wrist_ble::ServerTable) -> Result<ChipInfo, wrist_ble::Fault> {
        Fake::start(self);
        while let Some((_, e)) = self.advance(Instant(u64::MAX)) {
            if let RadioEvent::Ready(info) = e {
                return Ok(info);
            }
        }
        Err(wrist_ble::Fault::Silent)
    }

    async fn submit(&mut self, op: Op) -> Result<OpId, Failure> {
        Fake::submit(self, op)
    }

    /// # Panics
    ///
    /// Nothing will ever happen again: a test waiting for it would wait forever.
    async fn next(&mut self) -> RadioEvent {
        self.advance(Instant(u64::MAX)).expect("nothing left to happen").1
    }

    async fn snapshot(&mut self) -> Result<Snapshot, wrist_ble::Fault> {
        Ok(Fake::snapshot(self))
    }
}
