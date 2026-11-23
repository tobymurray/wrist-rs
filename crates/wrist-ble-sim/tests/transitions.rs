//! Changes of plan taken a step at a time, and every transition between the plans the menu reaches.

use std::sync::atomic::{AtomicU32, Ordering};

use wrist_ble::policy::goals::{Power, MAX_PERIPHERALS};
use wrist_ble::policy::invariants::REACHABLE_GAP;
use wrist_ble::policy::plan::{fingerprint, Refused};
use wrist_ble::policy::transition::{transition, Link, Path, Placed, Plan, Rules, MAX_STEPS, SEARCH_BUDGET};
use wrist_ble::{Activities, AdvParams, Admitted, Change, ConnParams, Duration, OpKind, SchedKind, Schedule, ScanParams};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn adv(units: u16) -> AdvParams {
    AdvParams { interval_min: Duration::from_u16(units), interval_max: Duration::from_u16(units), connectable: true }
}

/// 100 ms, 500 ms and 1 s.
const FAST: u16 = 160;
const BESIDE: u16 = 792;
const SLOW: u16 = 1_592;

fn connecting() -> ConnParams {
    MENU.connecting[0].params
}

fn live() -> ConnParams {
    MENU.live[0].params
}

fn rules(refused: &Refused, reachable: bool) -> Rules<'_> {
    Rules { schedule: &Scheduler, refused, deadline: CAPABILITIES.op_deadline, reachable, settling: false }
}

fn advertising(units: Option<u16>) -> Placed {
    Placed::unlinked(Activities { advertising: units.map(adv), ..Activities::IDLE })
}

fn to(units: Option<u16>) -> Plan {
    Plan { advertising: units.map(adv), scanning: None, links: [None; MAX_PERIPHERALS] }
}

fn steps(p: &Path) -> Vec<Change> {
    p.iter().collect()
}

#[test]
fn what_runs_already_is_no_step_at_all() {
    let from = advertising(Some(SLOW));
    let (path, end) = transition(&from, &from.plan(), &rules(&Refused::EMPTY, true)).unwrap();
    assert!(path.is_empty());
    assert_eq!((path.len(), path.first()), (0, None));
    assert_eq!(end, from);
}

/// The chip refuses advertising over advertising (HARDWARE.md): a new interval is a stop, then a
/// start.
#[test]
fn advertising_is_stopped_before_it_starts_again() {
    let r = rules(&Refused::EMPTY, true);
    let cases = [
        (Some(FAST), Some(SLOW), vec![Change::Advertise(None), Change::Advertise(Some(adv(SLOW)))]),
        (None, Some(SLOW), vec![Change::Advertise(Some(adv(SLOW)))]),
        (Some(SLOW), None, vec![Change::Advertise(None)]),
    ];
    for (from, want, expect) in cases {
        let (path, end) = transition(&advertising(from), &to(want), &r).unwrap();
        assert_eq!(steps(&path), expect, "{from:?} to {want:?}");
        assert_eq!((path.len(), path.first(), path.is_empty()), (expect.len(), expect.first().copied(), false));
        assert_eq!(end.activities.advertising, want.map(adv));
    }
}

/// A scan is stopped and started as advertising is, each after the advertising's steps.
#[test]
fn a_scan_is_stopped_before_it_starts_again() {
    let scan = |ms: u16| ScanParams { interval: Duration::from_u16(ms), window: Duration::from_u16(ms / 2), active: false, duration: wrist_ble::Millis::secs(5) };
    let from = Placed::unlinked(Activities { scanning: Some(scan(160)), ..Activities::IDLE });
    let plan = Plan { scanning: Some(scan(320)), ..from.plan() };
    let (path, end) = transition(&from, &plan, &rules(&Refused::EMPTY, false)).unwrap();
    assert_eq!(steps(&path), [Change::Scan(None), Change::Scan(Some(scan(320)))]);
    assert_eq!(end.activities.scanning, Some(scan(320)));
    let both = Placed::unlinked(Activities { advertising: Some(adv(FAST)), ..from.activities });
    let (path, end) = transition(&both, &Plan { advertising: Some(adv(SLOW)), ..both.plan() }, &rules(&Refused::EMPTY, false)).unwrap();
    assert_eq!(steps(&path), [Change::Advertise(None), Change::Advertise(Some(adv(SLOW)))], "the scan left alone");
    assert_eq!(end.activities.scanning, Some(scan(160)));
}

/// A link cannot join 100 ms advertising, and advertising cannot start while the chip initiates:
/// with no phone linked (I1) advertising comes back before the link opens, with one after it.
#[test]
fn a_link_beside_advertising_keeps_the_watch_reachable() {
    let from = advertising(Some(FAST));
    let plan = Plan { links: [Some(connecting()), None, None], ..to(Some(BESIDE)) };
    let stop = Change::Advertise(None);
    let start = Change::Advertise(Some(adv(BESIDE)));
    let open = [Change::Initiate(Some(connecting())), Change::Connected];
    let (path, end) = transition(&from, &plan, &rules(&Refused::EMPTY, true)).unwrap();
    assert_eq!(steps(&path), [stop, start, open[0], open[1]]);
    assert_eq!(end.links[0].map(|l| (l.slot, l.params)), Some((0, connecting())));
    let (path, _) = transition(&from, &plan, &rules(&Refused::EMPTY, false)).unwrap();
    assert_eq!(steps(&path), [stop, open[0], open[1], start]);
    assert!(Scheduler.admit(&from.activities, &open[0]).is_err(), "the test's premise");
}

/// I1's allowance is the sum of the steps' deadlines while nothing connectable advertises: one
/// advertising start fits in it, a gap a step longer does not.
#[test]
fn a_gap_longer_than_the_allowance_is_no_transition() {
    fn slow_steps(k: OpKind) -> wrist_ble::Millis {
        if k == OpKind::Advertise { REACHABLE_GAP } else { (CAPABILITIES.op_deadline)(k) }
    }
    fn over(k: OpKind) -> wrist_ble::Millis {
        wrist_ble::Millis(slow_steps(k).0 + 1)
    }
    let r = Rules { deadline: slow_steps, ..rules(&Refused::EMPTY, true) };
    assert!(transition(&advertising(Some(FAST)), &to(Some(SLOW)), &r).is_some(), "a gap of exactly the allowance");
    let r = Rules { deadline: over, ..r };
    assert!(transition(&advertising(Some(FAST)), &to(Some(SLOW)), &r).is_none());
    assert!(transition(&advertising(Some(FAST)), &to(Some(SLOW)), &Rules { reachable: false, ..r }).is_some(), "no phone link owed");
    assert!(transition(&advertising(Some(FAST)), &to(None), &r).is_some(), "stopping takes no time unreachable");
}

/// A start the chip refused from a state is not taken from it again; a stop always is.
#[test]
fn a_refused_start_is_not_taken_again_but_a_stop_is() {
    let mut refused = Refused::EMPTY;
    assert!(refused.insert(fingerprint(&Activities::IDLE, &Change::Advertise(Some(adv(SLOW))))));
    assert!(transition(&advertising(Some(FAST)), &to(Some(SLOW)), &rules(&refused, true)).is_none());
    assert!(transition(&advertising(None), &to(Some(SLOW)), &rules(&refused, true)).is_none());
    assert!(transition(&advertising(None), &to(Some(FAST)), &rules(&refused, true)).is_some());
    assert!(refused.insert(fingerprint(&Activities::IDLE, &Change::Advertise(None))));
    assert_eq!(steps(&transition(&advertising(Some(FAST)), &to(None), &rules(&refused, true)).unwrap().0), [Change::Advertise(None)]);
}

/// While the phone's link settles no central link opens; other steps go on.
#[test]
fn no_link_opens_while_the_phone_settles() {
    let r = Rules { settling: true, ..rules(&Refused::EMPTY, false) };
    let plan = Plan { links: [None, Some(live()), None], ..to(None) };
    assert!(transition(&advertising(None), &plan, &r).is_none());
    assert!(transition(&advertising(Some(SLOW)), &to(None), &r).is_some());
}

/// A link is followed to the slot the chip gave it, re-timed in that slot, and closed there.
#[test]
fn each_link_keeps_to_its_own_slot() {
    let r = rules(&Refused::EMPTY, false);
    let (_, one) = transition(&advertising(None), &Plan { links: [None, Some(live()), None], ..to(None) }, &r).unwrap();
    assert_eq!(one.links, [None, Some(Link { slot: 0, params: live() }), None]);
    let both = Plan { links: [Some(connecting()), Some(live()), None], ..to(None) };
    let (_, two) = transition(&one, &both, &r).unwrap();
    assert_eq!(two.links[0].map(|l| l.slot), Some(1));
    assert_eq!(two.plan(), both);
    let (path, _) = transition(&two, &Plan { advertising: Some(adv(SLOW)), ..both }, &r).unwrap();
    assert_eq!(steps(&path), [Change::Advertise(Some(adv(SLOW)))], "links that fit are not re-timed");
    let swapped = Plan { links: [Some(live()), None, None], ..to(None) };
    let (path, end) = transition(&two, &swapped, &r).unwrap();
    assert_eq!(steps(&path), [Change::Close { slot: 0 }, Change::Retime { slot: 1, params: live() }]);
    assert_eq!(end.links, [Some(Link { slot: 1, params: live() }), None, None]);
    assert_eq!(end.activities.central[0], None);
    assert_eq!(end.plan(), swapped);
}

/// Refuses a scan beside advertising.
struct ScanAlone;

impl Schedule for ScanAlone {
    fn admit(&self, a: &Activities, change: &Change) -> Result<Admitted, SchedKind> {
        if matches!(change, Change::Scan(Some(_))) && a.advertising.is_some() {
            return Err(SchedKind::NoSlot);
        }
        Ok(Admitted::AS_ASKED)
    }
}

/// An ordering that meets a refusal is backed out of step by step, and the next one tried.
#[test]
fn a_refused_ordering_is_backed_out_of() {
    let scan = |w: u16| ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(w), active: false, duration: wrist_ble::Millis::secs(5) };
    let from = Placed::unlinked(Activities { advertising: Some(adv(FAST)), scanning: Some(scan(80)), ..Activities::IDLE });
    let plan = Plan { advertising: Some(adv(SLOW)), scanning: Some(scan(40)), ..from.plan() };
    let r = Rules { schedule: &ScanAlone, ..rules(&Refused::EMPTY, false) };
    let (path, end) = transition(&from, &plan, &r).unwrap();
    assert_eq!(steps(&path), [Change::Advertise(None), Change::Scan(None), Change::Scan(Some(scan(40))), Change::Advertise(Some(adv(SLOW)))]);
    assert_eq!(end.plan(), plan);
}

/// Admits everything but a scan's start, counting what it was asked.
struct NoScans(AtomicU32);

impl Schedule for NoScans {
    fn admit(&self, _: &Activities, change: &Change) -> Result<Admitted, SchedKind> {
        self.0.fetch_add(1, Ordering::Relaxed);
        match change {
            Change::Scan(Some(_)) => Err(SchedKind::NoSlot),
            Change::Connected | Change::Retime { .. } => Ok(Admitted { slot: Some(wrist_ble::CentralSlot { interval: live().interval_max, latency: 1, ce_len: live().ce_len, offset: wrist_ble::Micros(0) }), latency_scale: 1 }),
            _ => Ok(Admitted::AS_ASKED),
        }
    }
}

/// A transition with no ordering gives up within its budget rather than try every one.
#[test]
fn a_search_with_no_ordering_ends_within_its_budget() {
    let scan = ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(80), active: false, duration: wrist_ble::Millis::secs(5) };
    let from = Placed::unlinked(Activities { advertising: Some(adv(FAST)), scanning: Some(scan), ..Activities::IDLE });
    let plan = Plan { advertising: Some(adv(SLOW)), scanning: Some(ScanParams { window: Duration::from_u16(40), ..scan }), links: [Some(live()); MAX_PERIPHERALS] };
    let schedule = NoScans(AtomicU32::new(0));
    let r = Rules { schedule: &schedule, ..rules(&Refused::EMPTY, false) };
    assert_eq!(MAX_STEPS, 10);
    assert!(transition(&from, &plan, &r).is_none());
    assert_eq!(schedule.0.load(Ordering::Relaxed), SEARCH_BUDGET);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhoneAt {
    Away,
    /// Linked, within its settle window: no central link starts.
    Settling,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerAt {
    None,
    Connecting,
    Live,
}

#[derive(Clone, Copy, Debug)]
struct Situation {
    power: Power,
    idle: bool,
    phone: PhoneAt,
    peers: [PeerAt; MAX_PERIPHERALS],
}

impl Situation {
    fn all() -> Vec<Self> {
        let at = [PeerAt::None, PeerAt::Connecting, PeerAt::Live];
        let mut all = Vec::new();
        for power in [Power::Cable, Power::Battery] {
            for idle in [false, true] {
                for phone in [PhoneAt::Away, PhoneAt::Settling, PhoneAt::Up] {
                    for a in at {
                        for b in at {
                            for c in at {
                                all.push(Self { power, idle, phone, peers: [a, b, c] });
                            }
                        }
                    }
                }
            }
        }
        all
    }

    /// The menu's options for this situation, most preferred first: advertising only with no
    /// phone link, the options for beside a central link when there is one.
    fn plans(self) -> Vec<Plan> {
        let links = self.peers.map(|p| match p {
            PeerAt::None => None,
            PeerAt::Connecting => Some(connecting()),
            PeerAt::Live => Some(live()),
        });
        let beside = links.iter().any(Option::is_some);
        if self.phone != PhoneAt::Away {
            return vec![Plan { advertising: None, scanning: None, links }];
        }
        MENU.advertising.iter().filter(|c| c.when.matches(self.power, beside, self.idle)).map(|c| Plan { advertising: Some(c.params), scanning: None, links }).collect()
    }

    /// What the planner aims for when nothing stands in the way.
    fn plan(self) -> Plan {
        self.plans()[0]
    }

    fn linked(self) -> bool {
        self.phone != PhoneAt::Away
    }
}

fn rules_into(to: Situation) -> Rules<'static> {
    Rules { schedule: &Scheduler, refused: &Refused::EMPTY, deadline: CAPABILITIES.op_deadline, reachable: !to.linked(), settling: to.phone == PhoneAt::Settling }
}

/// The time without connectable advertising a path spends, judged apart from the search.
fn gap(from: &Activities, path: &Path) -> u32 {
    let mut at = *from;
    let mut gap = 0;
    for c in path.iter() {
        if !at.advertising.is_some_and(|a| a.connectable) {
            gap += match c {
                Change::Advertise(_) => (CAPABILITIES.op_deadline)(OpKind::Advertise).0,
                Change::Initiate(_) => (CAPABILITIES.op_deadline)(OpKind::Connect).0,
                Change::Retime { .. } => (CAPABILITIES.op_deadline)(OpKind::UpdateParams).0,
                Change::Close { .. } => (CAPABILITIES.op_deadline)(OpKind::Disconnect).0,
                Change::Scan(_) => (CAPABILITIES.op_deadline)(OpKind::Scan).0,
                _ => 0,
            };
        }
        at = at.walk(&Scheduler, &[c]).unwrap();
    }
    gap
}

/// The first of `to`'s plans a transition reaches from `from`, as the planner chooses, with which
/// it was.
fn reach(from: &Placed, to: Situation) -> Option<(usize, Path, Placed)> {
    to.plans().iter().enumerate().find_map(|(i, plan)| transition(from, plan, &rules_into(to)).map(|(path, end)| (i, path, end)))
}

/// Every situation the menu plans is reached from the chip's start, and every other from it, by an
/// ordering admitted at each step, within I1 and opening no link while the phone settles; through
/// a less preferred plan at most once.
#[test]
fn every_plan_reaches_every_other_a_step_at_a_time() {
    let all = Situation::all();
    let reached: Vec<Placed> = all
        .iter()
        .map(|s| {
            let start = Placed::unlinked(Activities { peripheral: u8::from(s.linked()), ..Activities::IDLE });
            let boot = Rules { reachable: false, settling: false, ..rules_into(*s) };
            let (_, placed) = transition(&start, &s.plan(), &boot).unwrap_or_else(|| panic!("{s:?} unreached"));
            assert_eq!(placed.plan(), s.plan());
            placed
        })
        .collect();
    let (mut pairs, mut waiting, mut second) = (0, 0, 0);
    for (p, from) in all.iter().zip(&reached) {
        for q in &all {
            let mut from = *from;
            let phone = match (p.linked(), q.linked()) {
                (false, true) => Some(Change::PeripheralUp),
                (true, false) => Some(Change::PeripheralDown),
                _ => None,
            };
            if let Some(c) = phone {
                from.activities = from.activities.walk(&Scheduler, &[c]).unwrap();
            }
            let opens = from.links.iter().zip(&q.plan().links).any(|(had, want)| had.is_none() && want.is_some());
            let found = reach(&from, *q);
            if q.phone == PhoneAt::Settling && opens {
                assert!(found.is_none(), "{p:?} to {q:?} opens a link while the phone settles");
                waiting += 1;
                continue;
            }
            let (choice, path, end) = found.unwrap_or_else(|| panic!("no ordering from {p:?} to {q:?}"));
            assert_eq!(end.plan(), q.plans()[choice], "{p:?} to {q:?}");
            let steps: Vec<Change> = path.iter().collect();
            assert_eq!(from.activities.walk(&Scheduler, &steps), Ok(end.activities), "{p:?} to {q:?}: {steps:?}");
            if !q.linked() {
                assert!(gap(&from.activities, &path) <= REACHABLE_GAP.0, "{p:?} to {q:?}: {steps:?}");
            }
            if choice > 0 {
                let (then, ..) = reach(&end, *q).unwrap();
                assert_eq!(then, 0, "{p:?} to {q:?} stays on option {choice}");
                second += 1;
            }
            pairs += 1;
        }
    }
    assert_eq!(all.len(), 2 * 2 * 3 * 27);
    assert_eq!(pairs + waiting, all.len() * all.len());
    assert!(waiting > 0);
    println!("{pairs} transitions, {second} through a less preferred plan, {waiting} waiting for the phone to settle");
}
