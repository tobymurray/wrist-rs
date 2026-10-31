//! Every interleaving, within bounds, of what the radio and the user can do to each machine.
//!
//! Time moves to a deadline or by a fixed step, so a run is finite; the radio and the user act a
//! bounded number of times between two moves.

use stateright::{Checker, Model, Property};
use wrist_ble::{AddrKind, BdAddr, BluetoothUuid, ConnHandle, FailureKind, Instant, Millis, PairingStep, PeerAddr, ResetCause, Status};
use wrist_ble_core::chip::{self, Chip, ChipMachine, RESETS_PER_HOUR};
use wrist_ble_core::goals::Power;
use wrist_ble_core::peripheral::{self, Ctx, Event as E, Peripheral, Stage, GIVE_UP};
use wrist_ble_core::phone::{self, Phone, Security};
use wrist_ble_core::plan::Conflict;
use wrist_ble_core::profile::{Cadence, Contact, Kind, Profile, Reading, SecurityNeed, Silence};
use wrist_ble_core::status::{peer_state, PeerState};
use wrist_ble_sim::fake::INFO;

/// Acts of the radio or the user between two moves of time.
const ACTS: u8 = 2;

/// Time moves by this, the machines catching up on any deadline passed, so that every time the
/// model meets is on one grid and the states stay few.
const WAIT: Millis = Millis::secs(30);

fn no_parse(_: &[u8]) -> Option<Reading> {
    None
}

/// A heart-rate strap needing encryption, without a fixed cadence so that the model need not
/// count readings by the block.
static HR: Profile = Profile {
    kind: Kind::HeartRate,
    service: BluetoothUuid::from_u16(0x180D),
    subscribe: &[BluetoothUuid::from_u16(0x2A37)],
    security: SecurityNeed::Encrypted,
    cadence: Cadence::Events,
    silence: Silence::Fault,
    unit: "bpm",
    quiet: Millis::secs(5),
    stale: Millis::secs(30),
    parse: no_parse,
};

fn link() -> ConnHandle {
    ConnHandle::new(1)
}

/// Equal and hashed as its machine with the counts that only the display reads left out, so
/// that paths differing only in those meet.
#[derive(Clone, Copy, Debug)]
struct PeerModel {
    machine: Peripheral,
    now: Instant,
    acts: u8,
    /// Whether it has been Live or Stopped since it was chosen.
    ended: bool,
    /// The radio may be busy once, and free once.
    busy: bool,
    freed: bool,
}

impl PeerModel {
    fn key(&self) -> (Peripheral, Instant, u8, bool, bool, bool) {
        let mut machine = self.machine;
        machine.attempts = 0;
        machine.chosen_at = Instant(0);
        (machine, self.now, self.acts, self.ended, self.busy, self.freed)
    }
}

impl PartialEq for PeerModel {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for PeerModel {}

impl std::hash::Hash for PeerModel {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.key().hash(h);
    }
}

struct Peripherals {
    workout: bool,
    /// The peripheral never delivers a reading.
    silent: bool,
}

impl Peripherals {
    fn ctx(&self) -> Ctx<'static> {
        let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([1, 2, 3, 4, 5, 6]) };
        Ctx { profile: &HR, peer, power: Power::Battery, workout: self.workout }
    }

    /// A workout's slow search looks once a minute, so its model moves a minute at a time.
    fn wait(&self) -> Millis {
        if self.workout { peripheral::SLOW_EVERY } else { WAIT }
    }

    /// Past this, every peripheral has been Live or stopped, its give-up included.
    fn horizon(&self) -> Instant {
        Instant(u64::from(GIVE_UP.0) + if self.workout { u64::from(peripheral::SLOW_FOR.0) } else { 0 } + 120_000)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PeerAct {
    Radio(peripheral::Event),
    Wait,
}

impl Model for Peripherals {
    type State = PeerModel;
    type Action = PeerAct;

    fn init_states(&self) -> Vec<PeerModel> {
        let machine = Peripheral::default().step(&self.ctx(), Instant(0), peripheral::Event::Want);
        vec![PeerModel { machine, now: Instant(0), acts: 0, ended: false, busy: false, freed: false }]
    }

    fn actions(&self, s: &PeerModel, out: &mut Vec<PeerAct>) {
        if s.now >= self.horizon() {
            return;
        }
        // A disconnection the core asks for ends, by its deadline, in the link going down.
        if let Stage::Dropping { link, .. } = s.machine.stage {
            out.push(PeerAct::Radio(peripheral::Event::LinkDown(link, Status::CONN_TERMINATED_BY_LOCAL_HOST)));
            return;
        }
        out.push(PeerAct::Wait);
        // A live peripheral may deliver once at each instant, besides the radio's one act.
        if matches!(s.machine.stage, Stage::Live { .. }) && !self.silent && s.machine.last.is_none_or(|(at, _)| at != s.now) {
            out.push(PeerAct::Radio(peripheral::Event::Reading(Reading { value: 70, contact: Contact::Detected })));
        }
        if s.acts >= 1 {
            return;
        }
        let mut radio = |e| out.push(PeerAct::Radio(e));
        match s.machine.stage {
            Stage::Seeking { .. } => {
                radio(E::LinkUp(link()));
                radio(E::ConnectFailed(FailureKind::NotEstablished));
                radio(E::ConnectFailed(FailureKind::Timeout));
            }
            Stage::Linked { .. } => {
                radio(E::Secured(true));
                radio(E::Secured(false));
                radio(E::Discovered(true));
                radio(E::Discovered(false));
                radio(E::Subscribed(true));
                radio(E::ProcedureFailed);
                radio(E::LinkDown(link(), Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT));
                radio(E::LinkDown(link(), Status::CONN_TIMEOUT));
            }
            Stage::Live { .. } => radio(E::LinkDown(link(), Status::CONN_TIMEOUT)),
            _ => {}
        }
        if !s.busy {
            radio(E::Busy(Conflict::Transfer));
        } else if !s.freed {
            radio(E::Free);
        }
    }

    fn next_state(&self, s: &PeerModel, a: PeerAct) -> Option<PeerModel> {
        let ctx = self.ctx();
        let mut n = *s;
        match a {
            PeerAct::Radio(e) => {
                n.machine = s.machine.step(&ctx, s.now, e);
                n.acts += u8::from(!matches!(e, peripheral::Event::Reading(_)));
                n.busy |= matches!(e, peripheral::Event::Busy(_));
                n.freed |= e == peripheral::Event::Free;
            }
            PeerAct::Wait => {
                n.now = (s.now + self.wait()).min(self.horizon());
                n.machine = s.machine.step(&ctx, n.now, peripheral::Event::Tick);
                n.acts = 0;
            }
        }
        n.ended |= matches!(n.machine.stage, Stage::Live { .. } | Stage::Stopped(_));
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::sometimes("live, with a reading if it sends any", |m: &Peripherals, s: &PeerModel| {
                matches!(s.machine.stage, Stage::Live { .. }) && (m.silent || s.machine.last.is_some())
            }),
            Property::sometimes("stopped", |_, s: &PeerModel| matches!(s.machine.stage, Stage::Stopped(_))),
            Property::always("ends: Live or Stopped by the horizon", |m: &Peripherals, s: &PeerModel| s.now < m.horizon() || s.ended),
            Property::always("a current reading is fresh", |_, s: &PeerModel| match peer_state(&s.machine, &HR, s.now) {
                PeerState::Live { age, current: true, .. } | PeerState::Unstable { age, current: true, .. } => age <= wrist_ble_core::status::CURRENT_FOR,
                _ => true,
            }),
            Property::always("a peripheral never heard is let go at its give-up", |m: &Peripherals, s: &PeerModel| {
                m.workout || s.now < s.machine.heard_at + GIVE_UP + m.wait() || matches!(s.machine.stage, Stage::Stopped(_) | Stage::Off)
            }),
        ]
    }
}

#[test]
fn every_peripheral_ends_live_or_stopped() {
    for silent in [true, false] {
        Peripherals { workout: false, silent }.checker().spawn_bfs().join().assert_properties();
    }
}

/// The slow search's half hour is too many states to visit all of on each run: the first
/// million are.
#[test]
fn every_peripheral_in_a_workout_ends_live_or_stopped() {
    let c = Peripherals { workout: true, silent: true }.checker().target_state_count(1_000_000).spawn_bfs().join();
    c.assert_properties();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PhoneModel {
    phone: Phone,
    now: Instant,
    acts: u8,
    up: bool,
    encrypted: bool,
    told: u8,
    /// A numeric comparison showing since then, not yet answered by the machine.
    showing: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PhoneAct {
    Event(phone::Event),
    Wait,
}

struct Phones;

const PHONE_HORIZON: Instant = Instant(150_000);

impl Model for Phones {
    type State = PhoneModel;
    type Action = PhoneAct;

    fn init_states(&self) -> Vec<PhoneModel> {
        let phone = Phone::Off.step(Instant(0), phone::Event::RadioOn).0;
        vec![PhoneModel { phone, now: Instant(0), acts: 0, up: false, encrypted: false, told: 0, showing: None }]
    }

    fn actions(&self, s: &PhoneModel, out: &mut Vec<PhoneAct>) {
        if s.now >= PHONE_HORIZON {
            return;
        }
        out.push(PhoneAct::Wait);
        if s.acts >= ACTS {
            return;
        }
        if s.up {
            for e in [
                phone::Event::LinkDown(link()),
                phone::Event::Encryption(link(), true),
                phone::Event::Pairing(link(), PairingStep::Numeric(1)),
                phone::Event::Pairing(link(), PairingStep::Failed),
                phone::Event::Pairing(link(), PairingStep::PasskeyWanted),
                phone::Event::BondLost(link()),
                phone::Event::ParamsChanged(link()),
                phone::Event::Answer(true),
            ] {
                out.push(PhoneAct::Event(e));
            }
        } else {
            out.push(PhoneAct::Event(phone::Event::LinkUp(link())));
        }
    }

    fn next_state(&self, s: &PhoneModel, act: PhoneAct) -> Option<PhoneModel> {
        let mut n = *s;
        let (e, now) = match act {
            PhoneAct::Event(e) => {
                n.acts += 1;
                (e, s.now)
            }
            PhoneAct::Wait => {
                n.acts = 0;
                let due = s.phone.deadline().filter(|&d| d > s.now).unwrap_or(s.now + Millis::secs(20));
                (phone::Event::Tick, due.min(PHONE_HORIZON))
            }
        };
        n.now = now;
        let (p, actions) = s.phone.step(now, e);
        n.phone = p;
        match e {
            phone::Event::LinkUp(_) => (n.up, n.encrypted, n.told, n.showing) = (true, false, 0, None),
            phone::Event::LinkDown(_) => (n.up, n.encrypted, n.showing) = (false, false, None),
            phone::Event::Encryption(_, on) => {
                n.encrypted = on;
                n.showing = n.showing.filter(|_| !on);
            }
            phone::Event::Pairing(_, PairingStep::Numeric(_)) => n.showing = Some(now),
            phone::Event::Pairing(_, PairingStep::Failed) => n.showing = None,
            _ => {}
        }
        for a in actions.iter() {
            match a {
                phone::Action::TellBondLost(_) => n.told += 1,
                phone::Action::ConfirmNumeric(..) => n.showing = None,
                _ => {}
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("secure only on an encrypted link", |_, s: &PhoneModel| {
                !matches!(s.phone, Phone::Up(l) if l.security == Security::Secure) || (s.up && s.encrypted)
            }),
            Property::always("the lost bond told at most once a link", |_, s: &PhoneModel| s.told <= 1),
            Property::always("a comparison is answered by its deadline", |_, s: &PhoneModel| {
                s.showing.is_none_or(|at| s.now.since(at) <= phone::NUMERIC_DEADLINE)
            }),
            Property::always("the machine's link is the radio's", |_, s: &PhoneModel| s.up == matches!(s.phone, Phone::Up(_))),
        ]
    }
}

#[test]
fn the_phone_machine_keeps_its_promises() {
    Phones.checker().target_max_depth(60).spawn_bfs().join().assert_properties();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ChipModel {
    chip: ChipMachine,
    now: Instant,
    /// When the machine reset the chip without being asked, newest first.
    starts: [Option<Instant>; 6],
}

struct Chips;

const STEP: Millis = Millis::mins(10);
const CHIP_HORIZON: Instant = Instant(4 * 3_600_000);

impl Model for Chips {
    type State = ChipModel;
    type Action = chip::Event;

    fn init_states(&self) -> Vec<ChipModel> {
        let chip = ChipMachine::OFF.step(Instant(0), chip::Event::RadioOn).0;
        vec![ChipModel { chip, now: Instant(0), starts: [None; 6] }]
    }

    fn actions(&self, s: &ChipModel, out: &mut Vec<chip::Event>) {
        if s.now >= CHIP_HORIZON {
            return;
        }
        out.push(chip::Event::Tick);
        match s.chip.state {
            Chip::Starting { .. } => out.extend([chip::Event::Ready(INFO), chip::Event::StartFailed]),
            Chip::Ready(_) => out.extend([chip::Event::Reset(ResetCause::Watchdog), chip::Event::BaselineRefused]),
            _ => {}
        }
    }

    fn next_state(&self, s: &ChipModel, e: chip::Event) -> Option<ChipModel> {
        let mut n = *s;
        if e == chip::Event::Tick {
            let due = s.chip.deadline().filter(|&d| d > s.now).unwrap_or(s.now + STEP);
            n.now = due.min(s.now + STEP);
        }
        let (c, action) = s.chip.step(n.now, e);
        n.chip = c;
        let unasked = matches!(e, chip::Event::Reset(_) | chip::Event::StartFailed | chip::Event::BaselineRefused)
            || (e == chip::Event::Tick && matches!(s.chip.state, Chip::Starting { .. }));
        if unasked && action == Some(chip::Action::Start) {
            n.starts.rotate_right(1);
            n.starts[0] = Some(n.now);
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("at most three resets in any hour", |_, s: &ChipModel| {
                s.starts.windows(RESETS_PER_HOUR + 1).all(|w| match (w[0], w[RESETS_PER_HOUR]) {
                    (Some(newest), Some(oldest)) => newest.since(oldest) >= Millis::mins(60),
                    _ => true,
                })
            }),
            Property::sometimes("a fault", |_, s: &ChipModel| matches!(s.chip.state, Chip::Fault { .. })),
        ]
    }
}

#[test]
fn the_chip_is_reset_three_times_an_hour_at_most() {
    Chips.checker().target_max_depth(30).spawn_bfs().join().assert_properties();
}
