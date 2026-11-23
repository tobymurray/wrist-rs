//! `ble selftest`: the conformance cases that need no peer, each [`Step`] carried out by the caller
//! and its answer judged against the driver's model ([`Scheduler`], [`anchor_reading`]) and table.

use core::fmt;

use bluenrg2::opcode::{ACI_GAP_CREATE_CONNECTION, ACI_GAP_TERMINATE_GAP_PROC, ACI_HAL_GET_FW_BUILD_NUMBER, HCI_READ_LOCAL_VERSION};
use bluenrg2::{gap_proc, Event, VendorEvent};
use wrist_ble::conformance::{classify, Classified};
use wrist_ble::{Activities, AddrKind, AdvParams, Anchor, Change, ConnParams, Failure, Fault, Millis, Op, Schedule, ServerTable, Snapshot};

use crate::commands::Command;
use crate::driver::{Reply, Unanswered};
use crate::gatt::{Handles, FIRST_HANDLE};
use crate::shadow::menu::MENU;
use crate::{anchor_reading, native, Scheduler, INFO};

/// The central the refusal's initiation looks for: a static random address no device here holds.
pub const NOBODY: [u8; 6] = [0x01, 0x00, 0x00, 0x00, 0x57, 0xC0];

/// The initiation's scan interval and window (0.625 ms units).
const INITIATION_SCAN: u16 = 0x0010;

/// How long the initiation's end is waited for once its cancel is taken.
pub const CANCEL_WITHIN: Millis = Millis(1_000);

/// One case, as the result line names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Start,
    Gatt,
    Advertising(AdvParams),
    Stop,
    Refusal,
    Restart,
}

impl fmt::Display for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::Gatt => f.write_str("gatt"),
            Self::Advertising(p) => {
                // 0.625 ms units, in tenths of a millisecond.
                let tenths = u32::from(p.interval_min.as_u16()) * 625 / 100;
                write!(f, "advertising every {}", tenths / 10)?;
                if tenths % 10 != 0 {
                    write!(f, ".{}", tenths % 10)?;
                }
                f.write_str(" ms")
            }
            Self::Stop => f.write_str("stop"),
            Self::Refusal => f.write_str("refusal"),
            Self::Restart => f.write_str("restart"),
        }
    }
}

/// What the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Step {
    /// Reset the chip and bring it up through the driver's start; answer [`Answer::Started`].
    Start,
    /// Send this command; answer [`Answer::Replied`].
    Command(Command),
    /// Submit this operation through the driver; answer [`Answer::Settled`].
    Op(Op),
    /// Take the driver's snapshot; answer [`Answer::Snapshot`].
    Snapshot,
    /// Hand the chip's packets to [`Selftest::saw`] for up to this long, or until
    /// [`Selftest::waiting`] is false; answer [`Answer::Waited`].
    Wait(Millis),
    /// Nothing more: the result.
    Over(Outcome),
}

/// The caller's answer to the [`Step`] it carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant, reason = "no allocation: a value travels inline")]
pub enum Answer {
    /// The handles the start built the server with, `None` if it stopped short, and whether the
    /// chip took the central role.
    Started { handles: Option<Handles>, central: bool },
    Replied(Result<Reply, Unanswered>),
    /// What the driver made of the chip's answer to the operation.
    Settled(Result<(), Failure>),
    Snapshot(Result<Snapshot, Fault>),
    Waited,
}

/// How a selftest ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every case passed: how many there were.
    Pass(u8),
    /// The first case that failed, and why.
    Fail(Case, Why),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The start stopped short of the chip being up.
    StartStopped,
    /// The chip did not take the central role, which the refusal's initiation needs.
    NoCentral,
    /// The chip is not the one the driver declares.
    Identity { what: &'static str, read: u16, declared: u16 },
    /// The start's handles are not the table's, in order from [`FIRST_HANDLE`].
    Handles,
    Unanswered(Unanswered),
    /// A command the selftest needs taken was refused with this status.
    Status(u8),
    /// A reply too short to read.
    Short,
    /// The chip's answer to a change, beside the model's.
    Model(Classified),
    /// The chip refused as the model predicts, with another code than the model's.
    Code { chip: u16, model: u16 },
    Snapshot(Fault),
    /// A central connected: the selftest needs none.
    Linked,
    /// What runs, as the snapshot read it and as the model has it.
    Running { read: Running, model: Running },
    Anchor { read: Option<Anchor>, model: Option<Anchor> },
    /// An answer to another step than the one handed out.
    OutOfStep,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass(n) => write!(f, "pass: {n} cases"),
            Self::Fail(case, why) => write!(f, "fail: {case}: {why}"),
        }
    }
}

fn write_anchor(f: &mut fmt::Formatter<'_>, a: Option<Anchor>) -> fmt::Result {
    match a {
        Some(a) => write!(f, "{} us with {} us free", a.period.0, a.max_free_slot.0),
        None => f.write_str("none"),
    }
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::StartStopped => f.write_str("the start stopped short"),
            Self::NoCentral => f.write_str("the chip took no central role, which the refusal needs"),
            Self::Identity { what, read, declared } => write!(f, "{what} {read}, the driver declares {declared}"),
            Self::Handles => f.write_str("the start's handles are not the table's"),
            Self::Unanswered(u) => write!(f, "unanswered ({u:?})"),
            Self::Status(s) => write!(f, "refused, status {s:02X}"),
            Self::Short => f.write_str("a reply too short to read"),
            Self::Model(Classified::Unpredicted(c)) => write!(f, "refused {:02X}, which the model admits", c.native),
            Self::Model(Classified::Missed(k)) => write!(f, "taken, which the model refuses ({k:?})"),
            Self::Model(Classified::Otherwise { model, chip }) => write!(f, "refused {:02X} ({:?}), the model {model:?}", chip.native, chip.kind),
            Self::Model(c) => write!(f, "{c:?}"),
            Self::Code { chip, model } => write!(f, "refused {chip:02X}, the model's code {model:02X}"),
            Self::Snapshot(fault) => write!(f, "no snapshot ({fault:?})"),
            Self::Linked => f.write_str("a central connected; run it with none near"),
            Self::Running { read, model } => write!(f, "the snapshot read {read}, the model {model}"),
            Self::Anchor { read, model } => {
                f.write_str("the anchor read ")?;
                write_anchor(f, read)?;
                f.write_str(", the model ")?;
                write_anchor(f, model)
            }
            Self::OutOfStep => f.write_str("answered out of step"),
        }
    }
}

/// Advertising, scanning and initiating, each on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Running {
    pub advertising: bool,
    pub scanning: bool,
    pub initiating: bool,
}

impl fmt::Display for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on = [(self.advertising, "advertising"), (self.scanning, "scanning"), (self.initiating, "initiating")];
        let mut any = false;
        for (_, name) in on.iter().filter(|(on, _)| *on) {
            f.write_str(if any { " and " } else { "" })?;
            f.write_str(name)?;
            any = true;
        }
        if !any {
            f.write_str("idle")?;
        }
        Ok(())
    }
}

/// Where the selftest is: each stage hands out one step and judges its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Start,
    Version,
    Build,
    /// The start's snapshot, then its handles judged.
    Idle,
    /// The menu's advertising option at this index set, read back, then stopped.
    Advertise(usize),
    Read(usize),
    Stop(usize),
    Stopped,
    Initiate,
    Initiating,
    Refuse,
    Cancel,
    Cancelled,
    Ended,
    Restart,
    Restarted,
    /// After a failure: the initiation ended, then advertising stopped and put back, every answer
    /// taken as it comes.
    UndoCancel,
    UndoStop,
    UndoRestore,
    Over,
}

/// One run of `ble selftest`.
#[derive(Clone, Copy, Debug)]
pub struct Selftest<'a> {
    server: &'a ServerTable,
    /// The advertising the radio had, which the restart puts back.
    restore: AdvParams,
    stage: Stage,
    handles: Option<Handles>,
    /// The first failure.
    failed: Option<(Case, Why)>,
    /// A central connected since the selftest began.
    linked: bool,
    /// The initiation may still run.
    initiated: bool,
    /// The chip reported the initiation over.
    cancelled: bool,
}

/// The index of the menu's next advertising option from `from`, each interval once.
fn option(from: usize) -> Option<usize> {
    let a = MENU.advertising;
    (from..a.len()).find(|&i| !a[..i].iter().any(|c| c.params == a[i].params))
}

/// The link the refusal's initiation asks for: the menu's while connecting.
fn connecting() -> ConnParams {
    MENU.connecting[0].params
}

fn advertising(p: AdvParams) -> Activities {
    Activities { advertising: Some(p), ..Activities::IDLE }
}

fn initiating() -> Activities {
    Activities { initiating: Some(connecting()), ..Activities::IDLE }
}

/// `ACI_GAP_CREATE_CONNECTION` to [`NOBODY`] with `p`, from the watch's static random address.
#[must_use]
pub fn initiation(p: &ConnParams) -> Command {
    let [s0, s1] = INITIATION_SCAN.to_le_bytes();
    let random = AddrKind::RANDOM.as_raw();
    let [n0, n1, n2, n3, n4, n5] = NOBODY;
    let [i0, i1] = p.interval_min.as_u16().to_le_bytes();
    let [x0, x1] = p.interval_max.as_u16().to_le_bytes();
    let [l0, l1] = p.latency.to_le_bytes();
    let [t0, t1] = p.timeout.as_u16().to_le_bytes();
    let [c0, c1] = p.ce_len.as_u16().to_le_bytes();
    Command::fixed(
        ACI_GAP_CREATE_CONNECTION,
        [s0, s1, s0, s1, random, n0, n1, n2, n3, n4, n5, random, i0, i1, x0, x1, l0, l1, t0, t1, c0, c1, c0, c1],
    )
}

/// `ACI_GAP_TERMINATE_GAP_PROC` for the initiation.
#[must_use]
pub fn cancel() -> Command {
    Command::fixed(ACI_GAP_TERMINATE_GAP_PROC, [gap_proc::DIRECT_CONNECTION])
}

impl<'a> Selftest<'a> {
    /// A selftest of the driver serving `server`, the radio advertising at `restore`.
    #[must_use]
    pub fn new(server: &'a ServerTable, restore: AdvParams) -> Self {
        Self { server, restore, stage: Stage::Start, handles: None, failed: None, linked: false, initiated: false, cancelled: false }
    }

    /// How many cases a selftest runs: the start, the table, each advertising option, the stop,
    /// the refusal and the restart.
    #[must_use]
    pub fn cases() -> u8 {
        let mut n = 5;
        let mut i = option(0);
        while let Some(at) = i {
            n += 1;
            i = option(at + 1);
        }
        n
    }

    #[must_use]
    pub fn next(&self) -> Step {
        let advertise = |p| Step::Op(Op::Advertise(p));
        match self.stage {
            Stage::Start => Step::Start,
            Stage::Version => Step::Command(Command::fixed(HCI_READ_LOCAL_VERSION, [])),
            Stage::Build => Step::Command(Command::fixed(ACI_HAL_GET_FW_BUILD_NUMBER, [])),
            Stage::Advertise(i) => advertise(Some(MENU.advertising[i].params)),
            Stage::Stop(_) | Stage::UndoStop => advertise(None),
            Stage::Initiate => Step::Command(initiation(&connecting())),
            Stage::Refuse | Stage::Restart | Stage::UndoRestore => advertise(Some(self.restore)),
            Stage::Cancel | Stage::UndoCancel => Step::Command(cancel()),
            Stage::Cancelled => Step::Wait(CANCEL_WITHIN),
            Stage::Idle | Stage::Read(_) | Stage::Stopped | Stage::Initiating | Stage::Ended | Stage::Restarted => Step::Snapshot,
            Stage::Over => Step::Over(self.failed.map_or(Outcome::Pass(Self::cases()), |(c, w)| Outcome::Fail(c, w))),
        }
    }

    /// Whether a [`Step::Wait`] still waits for the chip.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.stage == Stage::Cancelled && !self.cancelled
    }

    /// A packet from the chip, whatever step is under way.
    pub fn saw(&mut self, packet: &[u8]) {
        match bluenrg2::parse(packet) {
            Event::LeConnectionComplete { .. } => self.linked = true,
            Event::Vendor(VendorEvent::GapProcComplete { procedure: gap_proc::DIRECT_CONNECTION, .. }) | Event::LeConnectionFailed { .. } => {
                self.cancelled = true;
            }
            _ => {}
        }
    }

    /// The answer to the step [`Self::next`] handed out.
    pub fn answer(&mut self, a: Answer) {
        if self.failed.is_some() {
            self.stage = self.after();
            return;
        }
        // A central's link explains whatever the chip answered beside it.
        let judged = self.judge(&a);
        match if self.linked { Err(Why::Linked) } else { judged } {
            Ok(()) => self.stage = self.after(),
            Err(why) => self.fail(why),
        }
    }

    /// The case the current stage belongs to.
    fn case(&self) -> Case {
        match self.stage {
            Stage::Start | Stage::Version | Stage::Build | Stage::Idle => Case::Start,
            Stage::Advertise(i) | Stage::Read(i) | Stage::Stop(i) => Case::Advertising(MENU.advertising[i].params),
            Stage::Stopped => Case::Stop,
            Stage::Initiate | Stage::Initiating | Stage::Refuse | Stage::Cancel | Stage::Cancelled | Stage::Ended => Case::Refusal,
            Stage::Restart | Stage::Restarted | Stage::UndoCancel | Stage::UndoStop | Stage::UndoRestore | Stage::Over => Case::Restart,
        }
    }

    fn after(&self) -> Stage {
        match self.stage {
            Stage::Start => Stage::Version,
            Stage::Version => Stage::Build,
            Stage::Build => Stage::Idle,
            Stage::Idle => option(0).map_or(Stage::Stopped, Stage::Advertise),
            Stage::Advertise(i) => Stage::Read(i),
            Stage::Read(i) => Stage::Stop(i),
            Stage::Stop(i) => option(i + 1).map_or(Stage::Stopped, Stage::Advertise),
            Stage::Stopped => Stage::Initiate,
            Stage::Initiate => Stage::Initiating,
            Stage::Initiating => Stage::Refuse,
            Stage::Refuse => Stage::Cancel,
            Stage::Cancel => Stage::Cancelled,
            Stage::Cancelled => Stage::Ended,
            Stage::Ended => Stage::Restart,
            Stage::Restart => Stage::Restarted,
            Stage::UndoCancel if !self.linked => Stage::UndoStop,
            Stage::UndoStop => Stage::UndoRestore,
            Stage::Restarted | Stage::UndoCancel | Stage::UndoRestore | Stage::Over => Stage::Over,
        }
    }

    /// The first failure kept, then undone what a central's link or a chip left down does not forbid.
    fn fail(&mut self, why: Why) {
        let case = if why == Why::Handles { Case::Gatt } else { self.case() };
        self.failed = Some((case, why));
        self.stage = match why {
            Why::StartStopped => Stage::Over,
            _ if self.initiated => Stage::UndoCancel,
            _ if self.linked => Stage::Over,
            _ => Stage::UndoStop,
        };
    }

    fn judge(&mut self, a: &Answer) -> Result<(), Why> {
        match (self.stage, *a) {
            (Stage::Start, Answer::Started { handles, central }) => {
                self.handles = Some(handles.ok_or(Why::StartStopped)?);
                if central {
                    Ok(())
                } else {
                    Err(Why::NoCentral)
                }
            }
            (Stage::Version, Answer::Replied(r)) => {
                let r = taken(&r)?;
                let p = &r.params[..r.len];
                let (Some(&hci), Some(&lmp), Some(m)) = (p.first(), p.get(3), p.get(4..6)) else { return Err(Why::Short) };
                same("HCI version", hci.into(), INFO.hci_version.into_inner().into())?;
                same("LMP version", lmp.into(), INFO.lmp_version.into_inner().into())?;
                same("manufacturer", u16::from_le_bytes([m[0], m[1]]), INFO.manufacturer)
            }
            (Stage::Build, Answer::Replied(r)) => {
                let r = taken(&r)?;
                let b = r.params[..r.len].get(..2).ok_or(Why::Short)?;
                same("firmware build", u16::from_le_bytes([b[0], b[1]]), INFO.firmware)
            }
            (Stage::Idle, Answer::Snapshot(s)) => {
                read(s, &Activities::IDLE)?;
                if self.handles == Handles::expected(self.server, FIRST_HANDLE) {
                    Ok(())
                } else {
                    Err(Why::Handles)
                }
            }
            (Stage::Advertise(i), Answer::Settled(r)) => model(&Activities::IDLE, &Change::Advertise(Some(MENU.advertising[i].params)), r),
            (Stage::Read(i), Answer::Snapshot(s)) => read(s, &advertising(MENU.advertising[i].params)),
            (Stage::Stop(i), Answer::Settled(r)) => model(&advertising(MENU.advertising[i].params), &Change::Advertise(None), r),
            (Stage::Stopped | Stage::Ended, Answer::Snapshot(s)) => read(s, &Activities::IDLE),
            (Stage::Initiate, Answer::Replied(r)) => {
                self.initiated = true;
                taken(&r)?;
                model(&Activities::IDLE, &Change::Initiate(Some(connecting())), Ok(()))
            }
            (Stage::Initiating, Answer::Snapshot(s)) => read(s, &initiating()),
            (Stage::Refuse, Answer::Settled(r)) => match classify(Scheduler.admit(&initiating(), &Change::Advertise(Some(self.restore))), r) {
                Classified::Predicted(k) => match r {
                    Err(f) if f.native != native(k) => Err(Why::Code { chip: f.native, model: native(k) }),
                    _ => Ok(()),
                },
                c => Err(Why::Model(c)),
            },
            (Stage::Cancel, Answer::Replied(r)) => {
                taken(&r)?;
                self.initiated = false;
                Ok(())
            }
            (Stage::Cancelled, Answer::Waited) => Ok(()),
            (Stage::Restart, Answer::Settled(r)) => model(&Activities::IDLE, &Change::Advertise(Some(self.restore)), r),
            (Stage::Restarted, Answer::Snapshot(s)) => read(s, &advertising(self.restore)),
            _ => Err(Why::OutOfStep),
        }
    }
}

fn same(what: &'static str, read: u16, declared: u16) -> Result<(), Why> {
    if read == declared {
        Ok(())
    } else {
        Err(Why::Identity { what, read, declared })
    }
}

fn taken(r: &Result<Reply, Unanswered>) -> Result<&Reply, Why> {
    let r = r.as_ref().map_err(|&u| Why::Unanswered(u))?;
    if r.status == 0 {
        Ok(r)
    } else {
        Err(Why::Status(r.status))
    }
}

/// The chip's answer to `change` beside `at`, which the model must have foreseen.
fn model(at: &Activities, change: &Change, r: Result<(), Failure>) -> Result<(), Why> {
    let c = classify(Scheduler.admit(at, change), r);
    if c.agrees() {
        Ok(())
    } else {
        Err(Why::Model(c))
    }
}

/// The snapshot against `model`: no link, what runs, and the anchor the model reads beside it,
/// except while initiating, which the model has no reading for.
fn read(s: Result<Snapshot, Fault>, model: &Activities) -> Result<(), Why> {
    let s = s.map_err(Why::Snapshot)?;
    if s.links.iter().any(Option::is_some) {
        return Err(Why::Linked);
    }
    let read = Running { advertising: s.advertising, scanning: s.scanning, initiating: s.initiating };
    // The chip reads an initiation as scanning: its link states have none for it (HARDWARE.md).
    let initiating = model.initiating.is_some();
    let want = Running { advertising: model.advertising.is_some(), scanning: model.scanning.is_some() || initiating, initiating: false };
    if read != want {
        return Err(Why::Running { read, model: want });
    }
    let anchor = anchor_reading(model);
    if model.initiating.is_none() && s.anchor != anchor {
        return Err(Why::Anchor { read: s.anchor, model: anchor });
    }
    Ok(())
}
