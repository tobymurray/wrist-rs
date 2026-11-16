//! The shadow, fed packets and commands as a driver would.

use wrist_ble::{ChipInfo, CoreSpecificationVersion, Instant, LeConnRole, Op, OpKind, Snapshot};
use wrist_ble_core::goals::{Goals, PhoneGoal, Power};
use wrist_ble_core::phone::{Phone, Security};
use wrist_ble_core::State;
use wrist_ble_shadow::verdict::Disagreement;
use wrist_ble_shadow::{DriverOp, Observer, Shadow};
use wrist_ble_shadow::menu::interval_now_ask;

#[derive(Default)]
struct Log {
    drivers: Vec<(u64, DriverOp, &'static str)>,
    ops: Vec<(u64, Op)>,
    snapshots: Vec<Snapshot>,
    found: Vec<Disagreement>,
    steps: usize,
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

    fn stepped(&mut self, _: Instant, _: &State) {
        self.steps += 1;
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
const WATCHDOG: &[u8] = &[0x04, 0xFF, 0x03, 0x01, 0x00, 0x05];
const PHONE: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];
/// A link the watch made as central.
const SENSOR: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x02, 0x08, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];
const ENCRYPTED: &[u8] = &[0x04, 0x08, 0x04, 0x00, 0x01, 0x08, 0x01];
const ACCEPTED: &[u8] = &[0x04, 0xFF, 0x06, 0x00, 0x08, 0x01, 0x08, 0x00, 0x00];
/// A command's reply, which no operation ends with.
const REPLY: &[u8] = &[0x04, 0x0E, 0x04, 0x01, 0x83, 0xFC, 0x00];
/// A packet shorter than its own length, after which the core asks for a snapshot.
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
fn an_action_left_alone_is_due_to_be_judged_once_it_can_no_longer_agree() {
    let mut s = Shadow::new(CHIP, Goals::OFF, Log::default());
    s.command(Instant(300), 0xFC95, &[0x01, 0x08]);
    assert_eq!(s.next(), Some(Instant(601)));
    s.advance(Instant(600));
    assert_eq!(s.observer.found, []);
    s.advance(Instant(601));
    assert_eq!(s.observer.found.len(), 1, "{:?}", s.observer.found);
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
fn the_consoles_ask_is_for_the_phones_link() {
    let mut s = up();
    let ask = Some(interval_now_ask(6, 6));
    s.interval_now(Instant(150), 6, 6);
    assert_ne!(s.phone_want(), ask, "no link to ask");
    s.event(Instant(200), PHONE);
    assert_ne!(s.phone_want(), ask, "nor a link that came up after");
    s.interval_now(Instant(250), 6, 6);
    assert_eq!(s.phone_want(), ask);
    s.event(Instant(300), ENCRYPTED);
    s.event(Instant(400), &disconnected(0x0802));
    assert_eq!(s.phone_want(), ask, "another link's end");
    s.event(Instant(500), &disconnected(0x0801));
    assert_ne!(s.phone_want(), ask, "the phone's link ended");
}

#[test]
fn the_consoles_ask_ends_where_the_drivers_timing_changes_or_the_radio_goes_off() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    let ask = Some(interval_now_ask(6, 6));
    s.interval_now(Instant(250), 6, 6);
    s.timing(Instant(300), Power::Cable, true);
    assert_eq!((s.phone_want(), s.goals().transfer), (ask, true), "a transfer on the cable");
    s.timing(Instant(350), Power::Battery, true);
    assert_eq!((s.phone_want(), s.goals().power), (ask, Power::Battery), "the battery during a transfer");
    s.timing(Instant(400), Power::Battery, false);
    assert_ne!(s.phone_want(), ask, "the battery's own");
    s.interval_now(Instant(420), 6, 6);
    s.timing(Instant(430), Power::Cable, false);
    assert_ne!(s.phone_want(), ask, "the cable's own");
    s.timing(Instant(440), Power::Battery, false);
    s.interval_now(Instant(450), 6, 6);
    s.timing(Instant(500), Power::Battery, true);
    assert_ne!(s.phone_want(), ask, "a transfer on the battery");
    s.interval_now(Instant(550), 6, 6);
    s.radio(Instant(600), true);
    assert_eq!(s.phone_want(), ask, "`ble on` while on");
    s.radio(Instant(650), false);
    assert_ne!(s.phone_want(), ask, "`ble off`");
}

#[test]
fn a_timing_that_changes_nothing_tells_the_core_nothing() {
    let mut s = up();
    let steps = s.observer.steps;
    s.timing(Instant(200), Power::Cable, false);
    assert_eq!(s.observer.steps, steps);
    s.timing(Instant(300), Power::Cable, true);
    assert!(s.observer.steps > steps);
}

#[test]
fn a_chip_that_restarts_ends_the_consoles_ask() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.interval_now(Instant(220), 6, 6);
    s.event(Instant(400), WATCHDOG);
    s.event(Instant(2_500), BOOTED);
    assert_ne!(s.phone_want(), Some(interval_now_ask(6, 6)));
}

#[test]
fn a_transfer_on_battery_asks_the_cables_timing() {
    let mut s = up();
    s.timing(Instant(150), Power::Battery, false);
    s.event(Instant(200), PHONE);
    s.event(Instant(300), ENCRYPTED);
    s.event(Instant(400), ACCEPTED);
    s.timing(Instant(500), Power::Battery, true);
    let asked: Vec<(u16, u16, u16)> = s.observer.ops.iter().filter_map(|(_, op)| match op {
        Op::UpdateParams(_, p) => Some((p.interval_min.as_u16(), p.interval_max.as_u16(), p.latency)),
        _ => None,
    }).collect();
    assert_eq!(asked.last(), Some(&(12, 24, 0)), "{asked:?}");
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

/// A link seen encrypted stays so until it ends, so the snapshot says so and the core keeps it
/// secure; one not seen encrypted may have been behind the lost events, so it can't say.
#[test]
fn a_snapshot_says_the_phones_link_is_encrypted_once_seen_so() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.event(Instant(300), CUT);
    s.event(Instant(400), ENCRYPTED);
    s.event(Instant(500), CUT);
    s.event(Instant(600), &disconnected(0x0801));
    s.event(Instant(700), PHONE);
    s.event(Instant(800), CUT);
    let seen: Vec<_> = s.observer.snapshots.iter().map(|n| (n.at.0, n.links[0].and_then(|l| l.encrypted))).collect();
    assert_eq!(seen, [(300, None), (500, Some(true)), (800, None)]);
    s.event(Instant(900), ENCRYPTED);
    s.event(Instant(1_000), CUT);
    assert!(matches!(s.state().phone, Phone::Up(l) if l.security == Security::Secure), "{:?}", s.state().phone);
}

/// Encryption Change names the link it is for (Core 6.0 Vol 4 Part E §7.7.8), so a sensor's says
/// nothing of the phone's.
#[test]
fn another_links_encryption_is_not_the_phones() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.event(Instant(250), SENSOR);
    s.event(Instant(300), &[0x04, 0x08, 0x04, 0x00, 0x02, 0x08, 0x01]);
    s.event(Instant(400), CUT);
    let n = s.observer.snapshots.last().expect("no snapshot");
    assert_eq!(n.links[0].map(|l| (l.link.raw(), l.encrypted)), Some((0x0801, None)));
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
fn a_chip_that_comes_up_again_holds_no_link() {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.command(Instant(250), 0xFC83, &[0x00, 0xA0, 0x00, 0xA0, 0x00]);
    s.radio(Instant(300), false);
    s.radio(Instant(400), true);
    s.event(Instant(500), BOOTED);
    s.event(Instant(600), CUT);
    let n = s.observer.snapshots.last().expect("no snapshot");
    assert_eq!((n.advertising, n.links[0].map(|l| l.link.raw())), (false, None));
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
    s.timing(Instant(500), Power::Battery, false);
    assert_eq!(asks(&s.observer), first, "a second ask while the first is on air");
    s.event(Instant(600), ACCEPTED);
    assert_eq!(asks(&s.observer).last(), Some(&600));
}

#[test]
fn the_core_takes_the_consoles_ask_where_the_drivers_console_does() {
    for min in 0..=3_300 {
        for max in 0..=3_300 {
            let ask = interval_now_ask(min, max);
            assert_eq!(wrist_ble_core::phone::askable(&ask), wrist_wake::ble::interval_now(min, max), "{min}-{max}");
        }
    }
}

/// The chip's Command Status for `opcode`.
fn command_status(status: u8, opcode: u16) -> Vec<u8> {
    let [lo, hi] = opcode.to_le_bytes();
    vec![0x04, 0x0F, 0x04, status, 0x01, lo, hi]
}

/// The phone up unencrypted and the core's security request asked beside the driver's.
fn asked_to_pair() -> Shadow<Log> {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.advance(Instant(5_200));
    assert_eq!(s.observer.ops.last(), Some(&(5_200, Op::RequestSecurity(wrist_ble::ConnHandle::new(0x0801)))));
    s.command(Instant(5_200), 0xFC8D, &[0x01, 0x08]);
    s
}

/// The driver's security request refused for good fails the core's pairing at once, where any
/// other refusal leaves it waiting for the outcome.
#[test]
fn the_drivers_refusal_is_the_cores_own_answer() {
    for (status, fails) in [(0x40, true), (0x0C, false), (0x00, false)] {
        let mut s = asked_to_pair();
        s.event(Instant(5_201), &command_status(status, 0xFC8D));
        assert_eq!(s.state().phone.may_ask_params(Instant(5_201)), fails, "status {status:02x}");
    }
    let mut s = asked_to_pair();
    s.command(Instant(5_201), 0xFC8D, &[0x02, 0x08]);
    s.event(Instant(5_202), &command_status(0x40, 0xFC8D));
    assert!(!s.state().phone.may_ask_params(Instant(5_202)), "another link's request");
    let mut s = asked_to_pair();
    s.event(Instant(5_201), &command_status(0x40, 0xFC81));
    assert!(!s.state().phone.may_ask_params(Instant(5_201)), "another command's reply");
    let mut s = asked_to_pair();
    s.command(Instant(5_201), 0xFD81, &[0x01, 0x08, 80, 0, 160, 0, 4, 0, 0x58, 0x02]);
    s.event(Instant(5_202), &command_status(0x40, 0xFD81));
    assert!(!s.state().phone.may_ask_params(Instant(5_202)), "another kind of command on the link");
    assert!(s.observer.found.is_empty(), "{:?}", s.observer.found);
}

/// `ACI_L2CAP_CONNECTION_PARAMETER_UPDATE_REQ`'s parameters for the phone's link.
fn ask_params(min: u16, max: u16, latency: u16, timeout: u16) -> Vec<u8> {
    [0x0801u16, min, max, latency, timeout].iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The phone's link encrypted and the core's ask on air beside the driver's, then the console's
/// ask while the central has yet to answer, which the chip answers `status`.
fn console_ask_answered(status: u8) -> Shadow<Log> {
    let mut s = up();
    s.event(Instant(200), PHONE);
    s.event(Instant(300), ENCRYPTED);
    let Some(&(_, Op::UpdateParams(_, p))) = s.observer.ops.last() else { panic!("{:?}", s.observer.ops) };
    s.command(Instant(300), 0xFD81, &ask_params(p.interval_min.as_u16(), p.interval_max.as_u16(), p.latency, p.timeout.as_u16()));
    s.event(Instant(301), &command_status(0x00, 0xFD81));
    s.command(Instant(400), 0xFD81, &ask_params(6, 6, 0, 500));
    s.event(Instant(401), &command_status(status, 0xFD81));
    s.advance(Instant(2_000));
    s.finish();
    s
}

/// An ask the chip refused busy never reached the air, so the core's not making it is no
/// disagreement; one refused otherwise is owed as any ask is.
#[test]
fn a_drivers_ask_the_chip_refused_busy_is_owed_nothing() {
    assert_eq!(console_ask_answered(0x46).observer.found, []);
    let found = console_ask_answered(0x12).observer.found;
    let lines: Vec<String> = found.iter().map(ToString::to_string).collect();
    assert_eq!(lines.first().map(String::as_str), Some("400: the driver asked for 6-6 units, latency 0 and the core did not"), "{lines:?}");
}
