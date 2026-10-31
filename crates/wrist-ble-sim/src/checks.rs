//! The core's invariants, checked against the fake's truth after every step.

use std::collections::VecDeque;
use std::mem::{discriminant, Discriminant};

use wrist_ble::{Instant, Millis, Op, OpKind, Status};
use wrist_ble_core::chip::Chip;
use wrist_ble_core::engine::Core;
use wrist_ble_core::invariants::{COMMANDS_PER_MINUTE, CONVERGE_AFTER, REACHABLE_GAP, REACHABLE_SHARE_PERCENT, REACHABLE_WINDOW, STATUS_WITHIN};
use wrist_ble_core::phone::SETTLE;
use wrist_ble_core::plan::PlanHealth;
use wrist_ble_core::status::{status, PhoneStatus};
use wrist_ble_core::State;

use crate::fake::{Fake, Note};

/// How often the status is sampled between steps.
const SAMPLE: Millis = Millis(100);

#[derive(Default)]
pub struct Checks {
    notes: usize,
    /// Planned gaps in advertising: when each began and ended.
    gaps: VecDeque<(Instant, Option<Instant>)>,
    commands: VecDeque<Instant>,
    /// Since when the status has contradicted the truth, by what it claims.
    wrong_since: Option<(Instant, &'static str)>,
    disturbed: Instant,
    phone_up: Option<Instant>,
    /// The state as the last step left it, and when.
    before: Option<(State, Instant)>,
}

fn class(s: PhoneStatus) -> Discriminant<PhoneStatus> {
    discriminant(&s)
}

impl Checks {
    /// Something outside the core changed what it should be doing.
    pub fn disturbed(&mut self, now: Instant) {
        self.disturbed = now;
    }

    /// # Errors
    ///
    /// The status changed class more than a second before the core ran again.
    pub fn before(&mut self, state: &State, now: Instant) -> Result<(), String> {
        if let Some((before, at)) = self.before {
            // Truthful status: between steps the state holds still, so the status may only change
            // class in the last second before the step that follows.
            let first = class(status(&before, at).phone);
            let mut t = at + SAMPLE;
            while t < now {
                if class(status(&before, t).phone) != first {
                    if now.since(t) > STATUS_WITHIN {
                        return Err(format!("truthful status: it became {:?} at {t:?} and the core next ran at {now:?}", status(&before, t).phone));
                    }
                    break;
                }
                t = t + SAMPLE;
            }
        }
        self.before = Some((*state, now));
        Ok(())
    }

    /// # Errors
    ///
    /// The invariant broken.
    pub fn after(&mut self, core: &Core<'_>, fake: &Fake, now: Instant) -> Result<(), String> {
        self.before = Some((core.state, now));
        for &(at, note) in &fake.notes[self.notes..] {
            match note {
                Note::Submitted { op, .. } => {
                    if !matches!(op.kind(), OpKind::Notify | OpKind::Read | OpKind::Write) {
                        self.commands.push_back(at);
                    }
                    let central = matches!(op, Op::Connect(..) | Op::Scan(Some(_)));
                    if central && self.phone_up.is_some_and(|up| at.since(up) < SETTLE) {
                        return Err(format!("phone first: {op:?} within the settle window"));
                    }
                }
                Note::CrossedEncryption(l) => return Err(format!("security order: a procedure crossed encryption on {l:?}")),
                Note::CentralInUpdate(l) => return Err(format!("phone first: a central start during the update on {l:?}")),
                Note::LinkDown { reason, .. } if reason == Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE => return Err("security order: MIC failure".into()),
                Note::LinkDown { reason, .. } if reason == Status::INSTANT_PASSED => return Err("phone first: instant passed".into()),
                _ => {}
            }
        }
        self.notes = fake.notes.len();
        while self.commands.front().is_some_and(|&t| now.since(t) >= Millis::mins(1)) {
            self.commands.pop_front();
        }
        if self.commands.len() > COMMANDS_PER_MINUTE as usize {
            return Err(format!("bounded effort: {} commands in a minute", self.commands.len()));
        }
        let phone = fake.phone_link();
        match (phone, self.phone_up) {
            (Some(_), None) => self.phone_up = Some(now),
            (None, Some(_)) => self.phone_up = None,
            _ => {}
        }
        if let Some(p) = core.pending() {
            if now > p.deadline + Millis(1_000) {
                return Err(format!("nothing silent: {:?} past its deadline", p.op));
            }
        }
        self.reachable(core, fake, now)?;
        self.converges(core, fake, now)?;
        self.truthful(core, fake, now)
    }

    /// With the radio on, ready and wanted discoverable, and no phone link, the watch
    /// advertises, but for short planned gaps.
    fn reachable(&mut self, core: &Core<'_>, fake: &Fake, now: Instant) -> Result<(), String> {
        let g = core.goals();
        let owed = g.radio_on && g.phone.discoverable && fake.ready() && fake.phone_link().is_none() && core.state.plan == PlanHealth::Ok;
        let gap = owed && fake.advertising().is_none();
        let open = matches!(self.gaps.back(), Some((_, None)));
        if gap && !open {
            self.gaps.push_back((now, None));
        }
        if !gap && open {
            if let Some(last) = self.gaps.back_mut() {
                last.1 = Some(now);
            }
        }
        while self.gaps.front().is_some_and(|(_, end)| end.is_some_and(|e| now.since(e) > REACHABLE_WINDOW)) {
            self.gaps.pop_front();
        }
        let len = |(s, e): &(Instant, Option<Instant>)| e.unwrap_or(now).since(*s);
        if let Some(g) = self.gaps.iter().find(|g| len(g) > REACHABLE_GAP) {
            return Err(format!("reachable: a gap of {:?} from {:?}", len(g), g.0));
        }
        let total: u32 = self.gaps.iter().map(|g| len(g).0).sum();
        if total > REACHABLE_WINDOW.0 * REACHABLE_SHARE_PERCENT / 100 {
            return Err(format!("reachable: {total} ms of gaps in a minute"));
        }
        Ok(())
    }

    /// Left alone, the radio reaches the plan, and the core knows it.
    fn converges(&self, core: &Core<'_>, fake: &Fake, now: Instant) -> Result<(), String> {
        let still = now.since(self.disturbed.max(core.settled_since));
        if still < CONVERGE_AFTER || !fake.ready() || core.state.plan != PlanHealth::Ok {
            return Ok(());
        }
        let planned = core.target().activities.advertising;
        let observed = core.state.radio.advertising;
        let truth = fake.activities().advertising;
        if planned != observed || observed != truth {
            return Err(format!("converges: planned {planned:?}, the core sees {observed:?}, the radio runs {truth:?}"));
        }
        Ok(())
    }

    /// What the status claims of the phone holds on air, within a second.
    fn truthful(&mut self, core: &Core<'_>, fake: &Fake, now: Instant) -> Result<(), String> {
        let s = status(&core.state, now);
        let wrong = match s.phone {
            PhoneStatus::Connected if fake.phone_link().is_none_or(|(_, enc)| !enc) => Some("connected"),
            PhoneStatus::Securing | PhoneStatus::Unsecured { .. } | PhoneStatus::Pairing { .. } if fake.phone_link().is_none() => Some("a phone link"),
            PhoneStatus::Discoverable { interval_ms: Some(_) } if fake.advertising().is_none() || fake.phone_link().is_some() => Some("discoverable"),
            PhoneStatus::Unreachable { .. } if matches!(core.state.chip.state, Chip::Ready(_)) && (fake.advertising().is_some() || fake.phone_link().is_some()) => {
                Some("unreachable")
            }
            _ => None,
        };
        match (wrong, self.wrong_since) {
            (None, _) => self.wrong_since = None,
            (Some(w), Some((since, was))) if w == was => {
                if now.since(since) > STATUS_WITHIN {
                    return Err(format!("truthful status: {w} for {:?} against the truth", now.since(since)));
                }
            }
            (Some(w), _) => self.wrong_since = Some((now, w)),
        }
        Ok(())
    }
}
