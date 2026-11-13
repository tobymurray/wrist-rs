//! The shadow, fed packets and commands as a driver would.

use wrist_ble::{ChipInfo, CoreSpecificationVersion, Instant, LeConnRole, Op, OpKind, Snapshot};
use wrist_ble_core::goals::{Goals, PhoneGoal, Power};
use wrist_ble_shadow::verdict::Disagreement;
use wrist_ble_shadow::{DriverOp, Observer, Shadow};
use wrist_ble_shadow::menu::interval_now_ask;

#[derive(Default)]
struct Log {
    drivers: Vec<(u64, DriverOp, &'static str)>,
    ops: Vec<(u64, Op)>,
    snapshots: Vec<Snapshot>,
    found: Vec<Disagreement>,
}

impl Observer for Log {
    fn driver(&mut self, at: Instant, op: DriverOp, name: &'static str, _: &[u8]) {
        self.drivers.push((at.0, op, name));
    }

    fn op(&mut self, at: Instant, op: &Op, _: bool) {
        self.ops.push((at.0, *op));
    }

    fn snapshot(&mut self, s: &Snapshot) {
        self.snapshots.push(*s);
    }

    fn disagreed(&mut self, d: Disagreement) {
        self.found.push(d);
    }
}

/// Any chip: nothing here reads its identity.
const CHIP: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0,
    firmware: 0,
};

const BOOTED: &[u8] = &[0x04, 0xFF, 0x03, 0x01, 0x00, 0x01];
const PHONE: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];
/// A link the watch made as central.
const SENSOR: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];
const ENCRYPTED: &[u8] = &[0x04, 0x08, 0x04, 0x00, 0x01, 0x08, 0x01];
const ACCEPTED: &[u8] = &[0x04, 0xFF, 0x06, 0x00, 0x08, 0x01, 0x08, 0x00, 0x00];
/// A command's reply, which no operation ends with.
const REPLY: &[u8] = &[0x04, 0x0E, 0x04, 0x01, 0x83, 0xFC, 0x00];
/// Too short for its own length: the driver's lost events, after which the core asks for a snapshot.
const CUT: &[u8] = &[0x04, 0x3E, 0x13, 0x01];

fn disconnected(handle: u16) -> Vec<u8> {
    let [lo, hi] = handle.to_le_bytes();
    vec![0x04, 0x05, 0x04, 0x00, lo, hi, 0x13]
}

fn goals() -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF }
}

/// Up and advertising at 100 ms, the driver's advertising beside the core's.
fn up() -> Shadow<Log> {
    let mut s = Shadow::new(CHIP, goals(), Log::default());
    s.set_goals(Instant(0), goals());
    s.event(Instant(100), BOOTED);
    s.command(Instant(100), 0xFC83, &[0x00, 0xA0, 0x00, 0xA0, 0x00]);
    s
}

fn asks(log: &Log) -> Vec<u64> {
    log.ops.iter().filter(|(_, op)| op.kind() == OpKind::UpdateParams).map(|(t, _)| *t).collect()
}

#[test]
fn each_command_that_is_an_operation_is_read_as_one() {
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    for code in [0xFC81, 0xFC83, 0xFC88, 0xFC8D, 0xFC93, 0xFC95, 0xFC97, 0xFC9D, 0xFCA5, 0xFD81, 0xFC17] {
        s.command(Instant(0), code, &[0x01, 0x08, 0x01, 0x00, 0x18, 0x00, 0x04, 0x00, 0x58, 0x02]);
    }
    let read: Vec<(DriverOp, &str)> = s.observer.drivers.iter().map(|&(_, d, n)| (d, n)).collect();
    assert_eq!(
        read,
        [
            (DriverOp::StopAdvertising, "stop advertising"),
            (DriverOp::Advertise(0x0108, 0x1800), "advertise"),
            (DriverOp::Other(OpKind::Passkey), "answer with a passkey"),
            (DriverOp::Other(OpKind::RequestSecurity), "ask the central to pair"),
            (DriverOp::Other(OpKind::Disconnect), "disconnect"),
            (DriverOp::Other(OpKind::AllowRebond), "allow a rebond"),
            (DriverOp::Other(OpKind::Scan), "scan"),
            (DriverOp::Other(OpKind::Scan), "end the scan"),
            (DriverOp::Confirm(true), "answer the comparison"),
            (DriverOp::AskParams(0x0001, 0x0018, 0x0004), "ask the central for parameters"),
        ]
    );
}

#[test]
fn the_core_is_owed_a_tick_once_it_waits_for_one() {
    let mut s = Shadow::new(CHIP, goals(), Log::default());
    assert_eq!(s.next(), None);
    s.set_goals(Instant(10), goals());
    assert!(s.next().is_some_and(|n| n.0 > 10), "{:?}", s.next());
}

#[test]
fn the_end_of_a_record_judges_what_still_waits() {
    let mut s = up();
    assert_eq!(s.observer.found, []);
    s.command(Instant(200), 0xFC95, &[0x01, 0x08]);
    assert_eq!(s.observer.found, []);
    s.finish();
    assert_eq!(s.observer.found.len(), 1, "{:?}", s.observer.found);
}

#[test]
fn the_consoles_ask_holds_until_the_power_source_or_the_link_ends_it() {
    let mut s = up();
    s.interval_now(Instant(200), 6, 6);
    assert_eq!(s.goals().phone_params, Some(interval_now_ask(6, 6)));
    s.power(Instant(300), Power::Battery);
    assert_eq!((s.goals().phone_params, s.goals().power), (None, Power::Battery));

    s.event(Instant(400), PHONE);
    s.interval_now(Instant(500), 6, 6);
    s.event(Instant(600), ENCRYPTED);
    s.event(Instant(700), &disconnected(0x0802));
    assert_eq!(s.goals().phone_params, Some(interval_now_ask(6, 6)), "another link's end");
    s.event(Instant(800), &disconnected(0x0801));
    assert_eq!(s.goals().phone_params, None, "the phone's link ended");
}

#[test]
fn a_snapshot_is_the_chip_as_the_driver_and_the_chip_left_it() {
    let mut s = up();
    s.event(Instant(200), CUT);
    s.event(Instant(300), PHONE);
    s.event(Instant(400), CUT);
    s.event(Instant(500), &disconnected(0x0801));
    s.command(Instant(550), 0xFC83, &[0x00, 0xA0, 0x00, 0xA0, 0x00]);
    s.command(Instant(650), 0xFC81, &[]);
    s.event(Instant(700), CUT);
    let seen: Vec<_> = s.observer.snapshots.iter().map(|n| (n.at.0, n.advertising, n.links[0].map(|l| (l.link.raw(), l.role, l.encrypted)))).collect();
    assert_eq!(seen, [(200, true, None), (400, false, Some((0x0801, LeConnRole::Peripheral, None))), (700, false, None)]);
}

#[test]
fn a_link_the_watch_made_as_central_is_not_the_phone() {
    let mut s = up();
    s.event(Instant(200), SENSOR);
    s.event(Instant(300), CUT);
    let n = s.observer.snapshots.last().expect("no snapshot");
    assert_eq!((n.advertising, n.links[0].map(|l| l.link.raw())), (true, None));
}

#[test]
fn a_watch_reset_forgets_the_link_and_the_advertising() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.command(Instant(250), 0xFC83, &[0x00, 0xA0, 0x00, 0xA0, 0x00]);
    s.restart(Instant(1_000));
    s.set_goals(Instant(1_000), goals());
    s.event(Instant(1_100), BOOTED);
    s.event(Instant(1_200), CUT);
    let n = s.observer.snapshots.last().expect("no snapshot");
    assert_eq!((n.advertising, n.links[0].map(|l| l.link.raw())), (false, None));
}

#[test]
fn the_centrals_acceptance_ends_the_ask_and_nothing_else_does() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.event(Instant(300), ENCRYPTED);
    let first = asks(&s.observer);
    assert_eq!(first.len(), 1, "{:?}", s.observer.ops);
    s.event(Instant(400), REPLY);
    s.power(Instant(500), Power::Battery);
    assert_eq!(asks(&s.observer), first, "a second ask while the first is on air");
    s.event(Instant(600), ACCEPTED);
    assert_eq!(asks(&s.observer).last(), Some(&600));
}
