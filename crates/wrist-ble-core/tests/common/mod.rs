#![allow(dead_code)]

use wrist_ble::{AddrKind, AdvParams, BdAddr, BluetoothUuid, ChipInfo, ConnHandle, CoreSpecificationVersion, Duration, Instant, Millis, PeerAddr};
use wrist_ble_core::chip::{self, ChipMachine};
use wrist_ble_core::goals::{Power, Want};
use wrist_ble_core::peripheral::{Ctx, Event, Peripheral};
use wrist_ble_core::phone::{self, Phone};
use wrist_ble_core::plan::Verdict;
use wrist_ble_core::profile::{Cadence, Contact, Kind, Profile, Reading, SecurityNeed, Silence};
use wrist_ble_core::status::{status, PeerState};
use wrist_ble_core::{Peer, State};

fn no_parse(_: &[u8]) -> Option<Reading> {
    None
}

pub static HR: Profile = Profile {
    kind: Kind::HeartRate,
    service: BluetoothUuid::from_u16(0x180D),
    subscribe: &[BluetoothUuid::from_u16(0x2A37)],
    security: SecurityNeed::None,
    cadence: Cadence::Every(Millis::secs(1)),
    silence: Silence::Fault,
    unit: "bpm",
    quiet: Millis::secs(5),
    stale: Millis::secs(30),
    parse: no_parse,
};

pub static CSC: Profile = Profile {
    kind: Kind::Csc,
    service: BluetoothUuid::from_u16(0x1816),
    subscribe: &[BluetoothUuid::from_u16(0x2A5B)],
    security: SecurityNeed::None,
    cadence: Cadence::Events,
    silence: Silence::Idle,
    unit: "rpm",
    quiet: Millis::secs(5),
    stale: Millis::secs(30),
    parse: no_parse,
};

pub fn peer() -> PeerAddr {
    PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([1, 2, 3, 4, 5, 6]) }
}

pub fn phone() -> ConnHandle {
    ConnHandle::new(0)
}

pub fn sensor() -> ConnHandle {
    ConnHandle::new(1)
}
pub const INFO: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0x30,
    firmware: 2353,
};

/// About once a second, in 0.625 ms units.
pub const ADV_995: AdvParams = AdvParams { interval_min: Duration::from_u16(1592), interval_max: Duration::from_u16(1592), connectable: true };

pub const fn t(ms: u64) -> Instant {
    Instant(ms)
}

pub fn secs(s: u64) -> Instant {
    Instant(s * 1_000)
}

pub fn ctx(profile: &'static Profile) -> Ctx<'static> {
    Ctx { profile, peer: peer(), power: Power::Battery, workout: false }
}

pub fn bpm(value: u16) -> Event {
    Event::Reading(Reading { value, contact: Contact::Detected })
}

/// Chosen at `at` and subscribed by the same instant.
pub fn live(ctx: &Ctx<'_>, at: Instant) -> Peripheral {
    [Event::Want, Event::LinkUp(sensor()), Event::Discovered(true), Event::Subscribed(true)]
        .into_iter()
        .fold(Peripheral::default(), |p, e| p.step(ctx, at, e))
}

/// Ticks at every deadline up to `to`, failing if the machine stops making progress.
pub fn run(mut p: Peripheral, ctx: &Ctx<'_>, from: Instant, to: Instant) -> Peripheral {
    let mut now = from;
    while let Some(due) = p.deadline(ctx, now) {
        if due > to {
            break;
        }
        let next = p.step(ctx, due.max(now), Event::Tick);
        assert!(due > now || next != p, "stuck at {now:?} with {p:?}");
        now = due.max(now);
        p = next;
    }
    p
}

pub fn ready_chip(at: Instant) -> ChipMachine {
    let (c, _) = ChipMachine::OFF.step(at, chip::Event::RadioOn);
    c.step(at, chip::Event::Ready(INFO)).0
}

/// The chip ready and the phone discoverable, nothing advertising yet.
pub fn ready(at: Instant) -> State {
    State {
        chip: ready_chip(at),
        phone: Phone::Off.step(at, phone::Event::RadioOn).0,
        phone_verdict: Verdict::Fits,
        silent_since: at,
        ..State::OFF
    }
}

pub fn phone_up(state: &mut State, at: Instant) {
    state.phone = state.phone.step(at, phone::Event::LinkUp(phone())).0;
}

pub fn with_peer(mut state: State, profile: &'static Profile, machine: Peripheral) -> State {
    state.peers[0] = Some(Peer { want: Want { profile, peer: peer(), priority: 0 }, machine });
    state
}

pub fn line(state: &State, now: Instant) -> String {
    let mut s = String::new();
    status(state, now).write_line(&mut s).unwrap();
    s
}

pub fn peer_state(state: &State, now: Instant) -> PeerState {
    status(state, now).peers[0].unwrap().state
}
