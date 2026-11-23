//! `ble selftest` run against a chip played here: as the model has it, then with one fault each.

use bluenrg2::opcode::{ACI_GAP_CREATE_CONNECTION, ACI_GAP_TERMINATE_GAP_PROC, ACI_HAL_GET_FW_BUILD_NUMBER, HCI_READ_LOCAL_VERSION};
use wrist_ble::conformance::Classified;
use wrist_ble::{Activities, AdvParams, Anchor, CharDecl, Exclusion, Micros, Op, SchedKind, ServiceDecl, Slots, Snapshot, ServerTable, MAX_LINKS};
use wrist_bluenrg2::driver::Reply;
use wrist_bluenrg2::gatt::{Handles, FIRST_HANDLE};
use wrist_bluenrg2::ops::refused;
use wrist_bluenrg2::selftest::{cancel, initiation, Answer, Case, Outcome, Selftest, Step, Why, NOBODY};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::wake::ble::server::{device_information, services};
use wrist_bluenrg2::{anchor_reading, INFO};

static DIS: [CharDecl; 3] = device_information(b"selftest");
static SERVICES: [ServiceDecl; 2] = services(&DIS);
static SERVER: ServerTable = ServerTable { services: &SERVICES };

const RESTORE: AdvParams = AdvParams { interval_min: Slots::from_u16(160), interval_max: Slots::from_u16(160), connectable: true };

/// The chip's end of the initiation.
const DIRECT_DONE: &[u8] = &[0x04, 0xFF, 0x04, 0x07, 0x04, 0x40, 0x00];
/// A central's link, 0x0801.
const LINK_UP: &[u8] = &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0x24, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00];

/// The chip as the driver's model has it, but for the fault each test sets.
#[allow(clippy::struct_excessive_bools, reason = "each is one fault or fact of the chip played")]
struct Chip {
    handles: Option<Handles>,
    central: bool,
    build: u16,
    advertising: Option<AdvParams>,
    initiating: bool,
    /// What it refuses advertising with while initiating; `None` takes it.
    while_initiating: Option<u8>,
    /// Added to every anchor period it reads.
    anchor_off: u32,
    /// A central connects at this step.
    link_at: Option<usize>,
    linked: bool,
    /// The initiation ends only at the second cancel.
    slow_cancel: bool,
    /// Its answer to the next advertising asked for.
    refuse_advertising: Option<u8>,
    /// Its packets still to hand out.
    packets: Vec<&'static [u8]>,
    steps: Vec<Step>,
}

impl Default for Chip {
    fn default() -> Self {
        Self {
            handles: Handles::expected(&SERVER, FIRST_HANDLE),
            central: true,
            build: INFO.firmware,
            advertising: Some(RESTORE),
            initiating: true,
            while_initiating: Some(0x0C),
            anchor_off: 0,
            link_at: None,
            linked: false,
            slow_cancel: false,
            refuse_advertising: None,
            packets: Vec::new(),
            steps: Vec::new(),
        }
    }
}

fn reply(status: u8, params: &[u8]) -> Reply {
    let mut r = Reply { status, params: [0; bluenrg2::RETURN_PARAMS_MAX], len: params.len() };
    r.params[..params.len()].copy_from_slice(params);
    r
}

impl Chip {
    fn answer(&mut self, step: &Step, t: &mut Selftest) -> Option<Outcome> {
        self.steps.push(*step);
        if self.link_at == Some(self.steps.len()) {
            self.linked = true;
            self.advertising = None;
            t.saw(LINK_UP);
        }
        let a = match *step {
            Step::Start => {
                *self = Self { advertising: None, initiating: false, packets: Vec::new(), steps: core::mem::take(&mut self.steps), ..*self };
                Answer::Started { handles: self.handles, central: self.central }
            }
            Step::Command(c) => Answer::Replied(Ok(match c.opcode {
                HCI_READ_LOCAL_VERSION => {
                    let [m0, m1] = INFO.manufacturer.to_le_bytes();
                    reply(0, &[INFO.hci_version.into_inner(), 0, 0, INFO.lmp_version.into_inner(), m0, m1, 0, 0])
                }
                ACI_HAL_GET_FW_BUILD_NUMBER => reply(0, &self.build.to_le_bytes()),
                ACI_GAP_CREATE_CONNECTION if self.initiating => reply(0x0C, &[]),
                ACI_GAP_CREATE_CONNECTION => {
                    self.initiating = true;
                    reply(0, &[])
                }
                ACI_GAP_TERMINATE_GAP_PROC if self.initiating && self.slow_cancel => {
                    self.slow_cancel = false;
                    reply(0, &[])
                }
                ACI_GAP_TERMINATE_GAP_PROC if self.initiating => {
                    self.initiating = false;
                    self.packets.push(DIRECT_DONE);
                    reply(0, &[])
                }
                ACI_GAP_TERMINATE_GAP_PROC => reply(0x0C, &[]),
                other => panic!("an unexpected command {other:04X}"),
            })),
            Step::Op(op) => {
                let status = match op {
                    Op::Advertise(Some(_)) if self.refuse_advertising.is_some() => self.refuse_advertising.take().unwrap_or(0),
                    Op::Advertise(Some(_)) if self.initiating => self.while_initiating.unwrap_or(0),
                    Op::Advertise(Some(_)) if self.advertising.is_some() || self.linked => 0x46,
                    Op::Advertise(Some(p)) => {
                        self.advertising = Some(p);
                        0
                    }
                    Op::Advertise(None) if self.advertising.is_none() => 0x46,
                    Op::Advertise(None) => {
                        self.advertising = None;
                        0
                    }
                    other => panic!("an unexpected operation {other:?}"),
                };
                if let (Op::Advertise(Some(p)), 0, true) = (op, status, self.initiating) {
                    self.advertising = Some(p);
                }
                Answer::Settled(if status == 0 { Ok(()) } else { Err(refused(&op, status)) })
            }
            Step::Snapshot => Answer::Snapshot(Ok(self.snapshot())),
            Step::Wait(_) => {
                for p in self.packets.drain(..) {
                    t.saw(p);
                }
                assert!(!t.waiting() || self.initiating, "the initiation's end was handed out");
                Answer::Waited
            }
            Step::Over(o) => return Some(o),
        };
        t.answer(a);
        None
    }

    fn snapshot(&self) -> Snapshot {
        let mut links = [None; MAX_LINKS];
        if self.linked {
            links[0] = Some(wrist_ble::LinkStatus {
                link: wrist_ble::ConnHandle::new(0x0801),
                role: wrist_ble::LeConnRole::Peripheral,
                peer: wrist_bluenrg2::snapshot::UNSEEN,
                encrypted: None,
            });
        }
        let a = Activities { advertising: self.advertising, ..Activities::IDLE };
        let anchor = anchor_reading(&a).map(|r| Anchor { period: Micros(r.period.0 + self.anchor_off), ..r });
        Snapshot { at: wrist_ble::Instant(0), links, advertising: self.advertising.is_some(), scanning: self.initiating, initiating: false, anchor }
    }
}

/// The selftest run to its end against `chip`.
fn run(chip: &mut Chip) -> Outcome {
    let mut t = Selftest::new(&SERVER, RESTORE);
    for _ in 0..100 {
        if let Some(o) = chip.answer(&t.next(), &mut t) {
            return o;
        }
    }
    panic!("no end after 100 steps: {:?}", chip.steps);
}

fn advertised(chip: &Chip) -> Vec<u16> {
    chip.steps.iter().filter_map(|s| if let Step::Op(Op::Advertise(Some(p))) = s { Some(p.interval_min.as_u16()) } else { None }).collect()
}

#[test]
fn a_chip_as_the_model_has_it_passes_every_case_and_is_left_advertising_as_found() {
    let mut chip = Chip::default();
    assert_eq!(run(&mut chip), Outcome::Pass(Selftest::cases()));
    assert_eq!(Selftest::cases(), 10, "the start, the table, five intervals, the stop, the refusal and the restart");
    assert_eq!(chip.steps[0], Step::Start);
    assert_eq!(chip.advertising, Some(RESTORE));
    assert!(!chip.initiating);
    // Each interval on the menu once, then the refusal's and the restart's at the one found.
    let mut menu: Vec<u16> = MENU.advertising.iter().map(|c| c.params.interval_min.as_u16()).collect();
    menu.dedup();
    assert_eq!(advertised(&chip), [&menu[..], &[160, 160]].concat());
    assert_eq!(Outcome::Pass(10).to_string(), "pass: 10 cases");
}

#[test]
fn an_anchor_off_the_models_fails_the_first_interval_and_the_advertising_is_put_back() {
    let mut chip = Chip { anchor_off: 625, ..Chip::default() };
    let o = run(&mut chip);
    let first = MENU.advertising[0].params;
    let model = anchor_reading(&Activities { advertising: Some(first), ..Activities::IDLE });
    let read = model.map(|m| Anchor { period: Micros(m.period.0 + 625), ..m });
    assert_eq!(o, Outcome::Fail(Case::Advertising(first), Why::Anchor { read, model }));
    assert_eq!(
        o.to_string(),
        "fail: advertising every 100 ms: the anchor read 105625 us with 86875 us free, the model 105000 us with 86875 us free"
    );
    assert_eq!(chip.advertising, Some(RESTORE), "stopped and put back");
    let commands = |op| chip.steps.iter().filter(|s| matches!(s, Step::Command(c) if c.opcode == op)).count();
    assert_eq!((commands(ACI_GAP_CREATE_CONNECTION), commands(ACI_GAP_TERMINATE_GAP_PROC)), (0, 0), "nothing to cancel");
}

#[test]
fn a_chip_that_advertises_while_initiating_fails_the_refusal_and_is_put_back() {
    let mut chip = Chip { while_initiating: None, ..Chip::default() };
    let o = run(&mut chip);
    assert_eq!(o, Outcome::Fail(Case::Refusal, Why::Model(Classified::Missed(SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating)))));
    assert!(!chip.initiating, "the initiation cancelled");
    assert_eq!(chip.advertising, Some(RESTORE));
    let cancel = chip.steps.iter().rposition(|s| matches!(s, Step::Command(c) if c.opcode == ACI_GAP_TERMINATE_GAP_PROC)).unwrap();
    assert!(matches!(chip.steps[cancel + 1], Step::Wait(_)), "its end waited for before advertising: {:?}", &chip.steps[cancel..]);
}

#[test]
fn an_initiation_still_ending_when_the_selftest_fails_is_cancelled_and_waited_for() {
    let mut chip = Chip { slow_cancel: true, ..Chip::default() };
    let o = run(&mut chip);
    let read = wrist_bluenrg2::selftest::Running { advertising: false, scanning: true, initiating: false };
    let idle = wrist_bluenrg2::selftest::Running { advertising: false, scanning: false, initiating: false };
    assert_eq!(o, Outcome::Fail(Case::Refusal, Why::Running { read, model: idle }));
    assert!(!chip.initiating);
    assert_eq!(chip.advertising, Some(RESTORE), "put back once the initiation ended");
}

#[test]
fn a_refusal_of_the_predicted_kind_with_another_code_fails() {
    let mut chip = Chip { while_initiating: Some(0x46), ..Chip::default() };
    assert_eq!(run(&mut chip), Outcome::Fail(Case::Refusal, Why::Code { chip: 0x46, model: 0x0C }));
    assert!(!chip.initiating);
    assert_eq!(chip.advertising, Some(RESTORE));
}

#[test]
fn a_refusal_of_another_kind_is_not_predicted() {
    let mut chip = Chip { while_initiating: Some(0x86), ..Chip::default() };
    let Outcome::Fail(Case::Refusal, Why::Model(Classified::Otherwise { model, chip: f })) = run(&mut chip) else { panic!("{:?}", chip.steps) };
    assert_eq!((model, f.native), (SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating), 0x86));
}

#[test]
fn a_start_that_stopped_short_ends_the_selftest_at_once() {
    let mut chip = Chip { handles: None, ..Chip::default() };
    assert_eq!(run(&mut chip), Outcome::Fail(Case::Start, Why::StartStopped));
    assert_eq!(chip.steps.len(), 2, "the start, then the end: {:?}", chip.steps);
}

#[test]
fn handles_other_than_the_tables_fail_the_gatt_case() {
    let mut other = Handles::default();
    assert!(other.push(0x0C, 0x0E, &DIS[0]));
    let mut chip = Chip { handles: Some(other), ..Chip::default() };
    assert_eq!(run(&mut chip), Outcome::Fail(Case::Gatt, Why::Handles));
    assert_eq!(chip.advertising, Some(RESTORE), "put back after the start left it off");
}

#[test]
fn another_firmware_build_fails_the_start() {
    let mut chip = Chip { build: 2352, ..Chip::default() };
    let o = run(&mut chip);
    assert_eq!(o, Outcome::Fail(Case::Start, Why::Identity { what: "firmware build", read: 2352, declared: 2353 }));
    assert_eq!(o.to_string(), "fail: start: firmware build 2352, the driver declares 2353");
}

#[test]
fn no_central_role_fails_the_start() {
    let mut chip = Chip { central: false, ..Chip::default() };
    assert_eq!(run(&mut chip), Outcome::Fail(Case::Start, Why::NoCentral));
}

#[test]
fn a_central_connecting_fails_the_case_and_nothing_more_is_asked_of_advertising() {
    // At the second interval's set: the link ends advertising, and the selftest leaves it so.
    let mut chip = Chip { link_at: Some(8), ..Chip::default() };
    let o = run(&mut chip);
    assert!(matches!(o, Outcome::Fail(Case::Advertising(_), Why::Linked)), "{o:?}");
    let after = chip.steps.iter().skip(8).filter(|s| matches!(s, Step::Op(_))).count();
    assert_eq!(after, 0, "{:?}", chip.steps);
}

#[test]
fn a_central_connecting_during_the_initiation_still_has_it_cancelled() {
    // The start's four steps, three for each interval, the stop's snapshot, then the initiation.
    let initiate = 4 + 3 * 5 + 2;
    let mut chip = Chip { link_at: Some(initiate + 1), ..Chip::default() };
    let o = run(&mut chip);
    assert_eq!(o, Outcome::Fail(Case::Refusal, Why::Linked));
    assert!(matches!(chip.steps[initiate - 1], Step::Command(c) if c.opcode == ACI_GAP_CREATE_CONNECTION), "{:?}", chip.steps);
    assert!(!chip.initiating);
    let cancel = chip.steps.iter().rposition(|s| matches!(s, Step::Command(c) if c.opcode == ACI_GAP_TERMINATE_GAP_PROC)).unwrap();
    assert!(!chip.steps[cancel..].iter().any(|s| matches!(s, Step::Op(_))), "advertising left to the link: {:?}", &chip.steps[cancel..]);
}

#[test]
fn the_initiation_asks_for_nobody_at_the_menus_connecting_link() {
    let p = MENU.connecting[0].params;
    let c = initiation(&p);
    assert_eq!(c.opcode, ACI_GAP_CREATE_CONNECTION);
    let b = c.params();
    assert_eq!(b.len(), 24);
    assert_eq!(&b[..4], &[0x10, 0x00, 0x10, 0x00], "scan interval and window");
    assert_eq!((b[4], &b[5..11], b[11]), (0x01, &NOBODY[..], 0x01), "random to random");
    assert_eq!(u16::from_le_bytes([b[12], b[13]]), p.interval_min.as_u16());
    assert_eq!(u16::from_le_bytes([b[14], b[15]]), p.interval_max.as_u16());
    assert_eq!(u16::from_le_bytes([b[18], b[19]]), p.timeout.as_u16());
    assert_eq!(NOBODY[5] & 0xC0, 0xC0, "a static random address");
    assert_eq!((cancel().opcode, cancel().params()), (ACI_GAP_TERMINATE_GAP_PROC, &[0x40][..]));
}

#[test]
fn the_tables_handles_are_those_measured_on_the_watch() {
    // HARDWARE.md: Device Information 12–18, the console 19–24.
    let h = Handles::expected(&SERVER, FIRST_HANDLE).unwrap();
    let got: Vec<(u16, u16)> = (0..5).map(|i| h.get(wrist_ble::ServerAttr(i)).unwrap()).collect();
    assert_eq!(got, [(12, 13), (12, 15), (12, 17), (19, 20), (19, 22)]);
    assert_eq!(h.get(wrist_ble::ServerAttr(5)), None);
}

#[test]
fn a_fractional_interval_is_named_to_the_tenth() {
    let p = AdvParams { interval_min: Slots::from_u16(1636), interval_max: Slots::from_u16(1636), connectable: true };
    assert_eq!(Case::Advertising(p).to_string(), "advertising every 1022.5 ms");
}

#[test]
fn advertising_refused_where_the_model_admits_it_fails_the_interval() {
    let mut chip = Chip { refuse_advertising: Some(0x46), ..Chip::default() };
    let o = run(&mut chip);
    let Outcome::Fail(Case::Advertising(p), Why::Model(Classified::Unpredicted(f))) = o else { panic!("{o:?}") };
    assert_eq!((p, f.native), (MENU.advertising[0].params, 0x46));
    assert_eq!(o.to_string(), "fail: advertising every 100 ms: refused 46, which the model admits");
}

#[test]
fn only_the_initiations_cancel_is_waited_on_until_its_end() {
    let mut t = Selftest::new(&SERVER, RESTORE);
    let mut chip = Chip::default();
    for _ in 0..100 {
        if matches!(t.next(), Step::Wait(_)) {
            break;
        }
        assert!(!t.waiting());
        let step = t.next();
        chip.packets.clear();
        assert_eq!(chip.answer(&step, &mut t), None);
    }
    assert!(matches!(t.next(), Step::Wait(_)), "no wait within 100 steps: {:?}", chip.steps);
    assert!(t.waiting(), "the cancel taken, its end not yet seen");
    t.saw(DIRECT_DONE);
    assert!(!t.waiting());
}

#[test]
fn what_runs_is_named() {
    use wrist_bluenrg2::selftest::Running;
    let r = |advertising, scanning, initiating| Running { advertising, scanning, initiating }.to_string();
    assert_eq!(r(false, false, false), "idle");
    assert_eq!(r(true, false, false), "advertising");
    assert_eq!(r(true, true, true), "advertising and scanning and initiating");
    assert_eq!(r(false, false, true), "initiating");
}

static TX_FIRST: [CharDecl; 2] = [wrist_bluenrg2::wake::ble::server::CONSOLE.chars[1], wrist_bluenrg2::wake::ble::server::CONSOLE.chars[0]];
static TX_FIRST_SERVICES: [ServiceDecl; 1] = [ServiceDecl { chars: &TX_FIRST, ..wrist_bluenrg2::wake::ble::server::CONSOLE }];

#[test]
fn a_characteristic_that_notifies_takes_a_third_handle() {
    let table = ServerTable { services: &TX_FIRST_SERVICES };
    let h = Handles::expected(&table, FIRST_HANDLE).unwrap();
    assert_eq!(h.get(wrist_ble::ServerAttr(0)), Some((12, 13)));
    assert_eq!(h.get(wrist_ble::ServerAttr(1)), Some((12, 16)), "after the declaration, the value and the configuration");
}
