//! A change of plan as single operations, in an order the driver's [`Schedule`] admits at every
//! step and that keeps the watch reachable.

use crate::{Activities, AdvParams, Change, ConnParams, Millis, OpKind, ScanParams, Schedule};

use crate::policy::goals::MAX_PERIPHERALS;
use crate::policy::invariants::REACHABLE_GAP;
use crate::policy::plan::{fingerprint, Refused};

/// The most steps one transition takes: advertising and a scan stopped and started, and each
/// peripheral's link opened in two.
pub const MAX_STEPS: usize = 4 + 2 * MAX_PERIPHERALS;

/// Orderings tried before a transition is given up as having none.
pub const SEARCH_BUDGET: u32 = 4_096;

/// What the planner wants running, with the parameters asked rather than those granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Plan {
    pub advertising: Option<AdvParams>,
    pub scanning: Option<ScanParams>,
    /// Each wanted peripheral's central link.
    pub links: [Option<ConnParams>; MAX_PERIPHERALS],
}

/// A peripheral's central link: the slot the chip gave it, and what it asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Link {
    pub slot: u8,
    pub params: ConnParams,
}

/// What runs: the activities as the chip admitted them, and whose each central slot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Placed {
    pub activities: Activities,
    pub links: [Option<Link>; MAX_PERIPHERALS],
}

impl Placed {
    /// `activities` with no link of a peripheral's among them.
    #[must_use]
    pub const fn unlinked(activities: Activities) -> Self {
        Self { activities, links: [None; MAX_PERIPHERALS] }
    }

    /// The plan this already is.
    #[must_use]
    pub fn plan(&self) -> Plan {
        Plan { advertising: self.activities.advertising, scanning: self.activities.scanning, links: self.links.map(|l| l.map(|l| l.params)) }
    }
}

/// The steps, in the order to take them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Path {
    steps: [Option<Change>; MAX_STEPS],
}

impl Path {
    pub const EMPTY: Self = Self { steps: [None; MAX_STEPS] };

    #[must_use]
    pub fn first(&self) -> Option<Change> {
        self.steps[0]
    }

    pub fn iter(&self) -> impl Iterator<Item = Change> + '_ {
        self.steps.iter().map_while(|s| *s)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps[0].is_none()
    }

    fn push(&mut self, c: Change) {
        if let Some(s) = self.steps.iter_mut().find(|s| s.is_none()) {
            *s = Some(c);
        }
    }

    fn pop(&mut self) {
        if let Some(s) = self.steps.iter_mut().rev().find(|s| s.is_some()) {
            *s = None;
        }
    }
}

/// What a transition must respect besides the schedule.
#[derive(Clone, Copy)]
pub struct Rules<'a> {
    pub schedule: &'a dyn Schedule,
    /// Starts the chip refused: none is taken again from the state it was refused in.
    pub refused: &'a Refused,
    /// The latest each step's operation ends, which a gap in advertising lasts through.
    pub deadline: fn(OpKind) -> Millis,
    /// No phone link: the transition may leave the watch without connectable advertising for at
    /// most [`REACHABLE_GAP`] in all (I1).
    pub reachable: bool,
    /// The phone's link is settling, so no central link starts.
    pub settling: bool,
}

/// Steps from `from` to `to`, each admitted beside the state before it, with no quarantined start
/// and the gap I1 allows, and what runs after them; `None` if no ordering within
/// [`SEARCH_BUDGET`] has them all.
#[must_use]
pub fn transition(from: &Placed, to: &Plan, rules: &Rules<'_>) -> Option<(Path, Placed)> {
    let (steps, n) = steps(from, to);
    let steps = &steps[..n];
    if rules.settling && steps.iter().flatten().any(|s| matches!(s.change, Change::Initiate(Some(_)))) {
        return None;
    }
    let mut search = Search { rules, steps, path: Path::EMPTY, budget: SEARCH_BUDGET };
    let end = search.from(0, from, 0)?;
    Some((search.path, end))
}

/// One step, and the step it must follow, if any.
#[derive(Clone, Copy, Debug)]
struct Step {
    change: Change,
    after: Option<usize>,
    /// The peripheral whose link this opens, closes or re-times.
    link: Option<usize>,
}

/// The steps a transition is made of, in the order tried first: stops, re-times, opens, starts.
fn steps(from: &Placed, to: &Plan) -> ([Option<Step>; MAX_STEPS], usize) {
    let mut out = [None; MAX_STEPS];
    let mut n = 0;
    let mut add = |change: Change, after: Option<usize>, link: Option<usize>| {
        out[n] = Some(Step { change, after, link });
        n += 1;
        n - 1
    };
    for (i, (had, want)) in from.links.iter().zip(&to.links).enumerate() {
        if let (Some(l), None) = (had, want) {
            add(Change::Close { slot: l.slot }, None, Some(i));
        }
    }
    let advertising = from.activities.advertising != to.advertising;
    let stop_adv = (advertising && from.activities.advertising.is_some()).then(|| add(Change::Advertise(None), None, None));
    let scanning = from.activities.scanning != to.scanning;
    let stop_scan = (scanning && from.activities.scanning.is_some()).then(|| add(Change::Scan(None), None, None));
    for (i, (had, want)) in from.links.iter().zip(&to.links).enumerate() {
        match (had, want) {
            (Some(l), Some(p)) if l.params != *p => {
                add(Change::Retime { slot: l.slot, params: *p }, None, Some(i));
            }
            (None, Some(p)) => {
                let open = add(Change::Initiate(Some(*p)), None, Some(i));
                add(Change::Connected, Some(open), Some(i));
            }
            _ => {}
        }
    }
    if let (true, Some(p)) = (advertising, to.advertising) {
        add(Change::Advertise(Some(p)), stop_adv, None);
    }
    if let (true, Some(p)) = (scanning, to.scanning) {
        add(Change::Scan(Some(p)), stop_scan, None);
    }
    (out, n)
}

/// A stop leads towards the safe baseline, so it is never quarantined: a refused one is the
/// baseline refused.
const fn stops(c: &Change) -> bool {
    matches!(c, Change::Advertise(None) | Change::Scan(None) | Change::Initiate(None) | Change::Close { .. })
}

/// The operation a step is, for its deadline; `None` for what the chip does by itself.
const fn kind(c: &Change) -> Option<OpKind> {
    match c {
        Change::Advertise(_) => Some(OpKind::Advertise),
        Change::Scan(_) => Some(OpKind::Scan),
        Change::Initiate(Some(_)) => Some(OpKind::Connect),
        Change::Initiate(None) => Some(OpKind::CancelConnect),
        Change::Retime { .. } => Some(OpKind::UpdateParams),
        Change::Close { .. } => Some(OpKind::Disconnect),
        Change::Connected | Change::PeripheralUp | Change::PeripheralDown => None,
    }
}

fn connectable(a: &Activities) -> bool {
    a.advertising.is_some_and(|a| a.connectable)
}

struct Search<'a, 'r> {
    rules: &'a Rules<'r>,
    steps: &'a [Option<Step>],
    path: Path,
    budget: u32,
}

impl Search<'_, '_> {
    /// Depth first from `at`, with `done` the steps taken and `gap` the time unreachable so far.
    fn from(&mut self, done: u16, at: &Placed, gap: u32) -> Option<Placed> {
        if usize::try_from(done.count_ones()).ok() == Some(self.steps.len()) {
            return Some(*at);
        }
        for (k, step) in self.steps.iter().enumerate() {
            let Some(step) = step else { continue };
            if done & (1 << k) != 0 || step.after.is_some_and(|a| done & (1 << a) == 0) {
                continue;
            }
            if self.budget == 0 {
                return None;
            }
            self.budget -= 1;
            let Some((next, gap)) = self.take(at, step, gap) else { continue };
            self.path.push(step.change);
            if let Some(end) = self.from(done | (1 << k), &next, gap) {
                return Some(end);
            }
            self.path.pop();
        }
        None
    }

    /// `step` taken from `at`, if the rules allow it there.
    fn take(&self, at: &Placed, step: &Step, gap: u32) -> Option<(Placed, u32)> {
        let r = self.rules;
        let change = step.change;
        let admitted = r.schedule.admit(&at.activities, &change).ok()?;
        if !stops(&change) && r.refused.contains(fingerprint(&at.activities, &change)) {
            return None;
        }
        let activities = at.activities.with(&change, &admitted).ok()?;
        let gap = if r.reachable && !connectable(&at.activities) { gap + kind(&change).map_or(0, |k| (r.deadline)(k).0) } else { gap };
        if gap > REACHABLE_GAP.0 {
            return None;
        }
        let mut links = at.links;
        if let Some(i) = step.link {
            links[i] = match change {
                Change::Connected => {
                    let slot = at.activities.central.iter().position(Option::is_none)?;
                    Some(Link { slot: u8::try_from(slot).ok()?, params: at.activities.initiating? })
                }
                Change::Retime { slot, params } => Some(Link { slot, params }),
                Change::Close { .. } => None,
                _ => links[i],
            };
        }
        Some((Placed { activities, links }, gap))
    }
}
