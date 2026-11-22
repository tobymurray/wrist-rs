//! The alarms the core raises on its own record (I1, I2).

mod common;

use common::*;
use wrist_ble::policy::chip::Action;
use wrist_ble::policy::engine::{Core, Input, Outputs, Tell};
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::invariants::{Invariant, Record, COMMANDS_PER_MINUTE, REACHABLE_ALARM};
use wrist_ble::{AdvParams, Millis, OpId, Outcome, RadioEvent};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn record() -> Record {
    Record {
        ready: true,
        radio_on: true,
        phone_linked: false,
        planned: Some(ADV_995),
        observed: None,
        silent_since: t(1_000),
        settled_since: t(0),
        commands: 0,
    }
}

#[test]
fn unreachable_alarms_once_the_phone_cannot_find_the_watch_for_long_enough() {
    let r = record();
    let at = t(1_000) + REACHABLE_ALARM;
    assert_eq!(r.unreachable_from(), Some(at));
    assert!(!r.broken(Invariant::Reachable, at - Millis(1), false));
    assert!(r.broken(Invariant::Reachable, at, false));
}

/// Hidden on purpose, then discoverable: the gap counts from the change, not from when
/// advertising stopped.
#[test]
fn unreachable_counts_from_the_goals_change_after_hiding() {
    let r = Record { silent_since: t(1_000), settled_since: t(60_000), ..record() };
    assert_eq!(r.unreachable_from(), Some(t(60_000) + REACHABLE_ALARM));
}

#[test]
fn unreachable_needs_every_condition() {
    let unconnectable = AdvParams { connectable: false, ..ADV_995 };
    let fine = [
        Record { ready: false, ..record() },
        Record { radio_on: false, ..record() },
        Record { phone_linked: true, ..record() },
        Record { planned: None, ..record() },
        Record { planned: Some(unconnectable), ..record() },
        Record { observed: Some(ADV_995), ..record() },
    ];
    for r in fine {
        assert_eq!(r.unreachable_from(), None, "{r:?}");
    }
    assert!(Record { observed: Some(unconnectable), ..record() }.unreachable_from().is_some());
}


#[test]
fn too_many_operations_is_its_own_alarm() {
    let late = t(1_000_000);
    let quiet = Record { phone_linked: true, planned: None, ..record() };
    assert!(!Record { commands: COMMANDS_PER_MINUTE, ..quiet }.broken(Invariant::BoundedEffort, late, false));
    assert!(Record { commands: COMMANDS_PER_MINUTE + 1, ..quiet }.broken(Invariant::BoundedEffort, late, false));
    assert!(!quiet.broken(Invariant::Reachable, late, false));
    assert!(!record().broken(Invariant::Converges, late, true), "I3 is not watched");
}

struct Bench {
    core: Core<'static>,
    id: u16,
}

impl Bench {
    fn new() -> Self {
        let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
        core.step(t(0), Input::Goals(goals(true)));
        let start = core.state.chip.starting().expect("a start");
        let mut b = Self { core, id: 0 };
        let out = b.core.step(t(0), Input::Started(start, Ok(INFO)));
        b.carry(t(0), out);
        b
    }

    /// Each operation asked, accepted and done at once; every output seen.
    fn carry(&mut self, at: wrist_ble::Instant, mut out: Outputs) -> Vec<Outputs> {
        let mut seen = vec![out];
        while out.op.is_some() {
            self.id += 1;
            let id = OpId(self.id);
            seen.push(self.core.step(at, Input::Submitted(Ok(id))));
            out = self.core.step(at, Input::Radio(RadioEvent::Done(id, Ok(Outcome::Done))));
            seen.push(out);
        }
        seen
    }

    fn step(&mut self, ms: u64, input: Input) -> Vec<Outputs> {
        let out = self.core.step(t(ms), input);
        self.carry(t(ms), out)
    }
}

fn goals(discoverable: bool) -> Goals {
    Goals { radio_on: true, phone: PhoneGoal { discoverable, pairing_open: true }, power: Power::Cable, ..Goals::OFF }
}

fn alarms(outs: &[Outputs]) -> Vec<Invariant> {
    outs.iter().flat_map(|o| o.tell).flatten().filter_map(|t| if let Tell::Alarm(i) = t { Some(i) } else { None }).collect()
}

/// More operations than I2 allows in a minute: one alarm, told once however long the episode
/// lasts, and neither a resync nor a restart for it.
#[test]
fn an_operation_storm_alarms_once_and_nothing_more() {
    let mut b = Bench::new();
    let mut told = Vec::new();
    let mut resyncs = 0;
    let mut ms = 1_000;
    while b.core.tally.alarms.total() == 0 && ms < 60_000 {
        let outs = b.step(ms, Input::Goals(goals(ms % 1_000 == 0)));
        told.extend(alarms(&outs));
        resyncs += outs.iter().filter(|o| o.snapshot).count();
        ms += 500;
    }
    assert_eq!(told, [Invariant::BoundedEffort], "peak {} at {ms}", b.core.tally.commands_peak);
    assert_eq!(resyncs, 0);
    // Each second for a minute, the breach lasting: no restart, no second alarm or resync.
    for at in (ms..ms + 60_000).step_by(1_000) {
        let outs = b.step(at, Input::Tick);
        told.extend(alarms(&outs));
        resyncs += outs.iter().filter(|o| o.snapshot).count();
        assert!(!outs.iter().any(|o| matches!(o.chip, Some(Action::Start(_)))), "a restart at {at}");
    }
    assert_eq!(told, [Invariant::BoundedEffort]);
    assert_eq!(resyncs, 0);
    assert_eq!(b.core.tally.restarts.total(), 0);
}

/// A core doing its ordinary work raises no alarm.
#[test]
fn ordinary_work_raises_no_alarm() {
    let mut b = Bench::new();
    for (i, ms) in (1_000..600_000).step_by(20_000).enumerate() {
        let outs = b.step(ms, Input::Goals(goals(i % 2 == 0)));
        assert!(alarms(&outs).is_empty(), "{ms} {:?} peak {}", alarms(&outs), b.core.tally.commands_peak);
        let outs = b.step(ms + 10_000, Input::Tick);
        assert_eq!(alarms(&outs), [], "{ms}");
    }
    assert_eq!(b.core.tally.alarms.total(), 0);
}

/// The record the alarms are judged on is the core's: ready only with a feasible plan.
#[test]
fn the_record_is_the_cores() {
    use wrist_ble::policy::plan::{Conflict, PlanHealth};
    let mut b = Bench::new();
    assert!(b.core.record(t(0)).ready);
    b.core.state.plan = PlanHealth::Infeasible(Conflict::Scan);
    assert!(!b.core.record(t(0)).ready);

}


#[test]
fn an_operation_a_whole_minute_old_is_out_of_the_window() {
    let mut tally = wrist_ble::policy::tally::Tally::NEW;
    tally.step(t(0), &Input::Tick, &ready(t(0)), true);
    assert_eq!(tally.commands_within(t(59_999)), 1);
    assert_eq!(tally.commands_within(t(60_000)), 0);
}

#[test]
fn alarms_are_named_on_the_counts_line() {
    let mut tally = wrist_ble::policy::tally::Tally::NEW;
    for i in [Invariant::Reachable, Invariant::BoundedEffort, Invariant::Converges] {
        tally.alarm(i);
    }
    let mut line = String::new();
    tally.write_line(&mut line, t(0)).unwrap();
    assert!(line.contains("; alarms reachable x1 bounded-effort x1 converges x1; "), "{line}");
}

/// A count that hovers at the limit is one alarm: it ends only well under the limit.
#[test]
fn too_many_operations_ends_only_well_under_the_limit() {
    use wrist_ble::policy::invariants::ALARM_HYSTERESIS;
    let mut b = Bench::new();
    for _ in 0..=COMMANDS_PER_MINUTE {
        b.core.tally.step(t(1_000), &Input::Tick, &b.core.state, true);
    }
    assert_eq!(alarms(&b.step(1_000, Input::Tick)), [Invariant::BoundedEffort]);
    // At the limit, then just under the end mark: the same alarm, not told again.
    for ms in [61_000, 61_500] {
        for _ in 0..COMMANDS_PER_MINUTE - ALARM_HYSTERESIS {
            b.core.tally.step(t(ms), &Input::Tick, &b.core.state, true);
        }
    }
    assert_eq!(alarms(&b.step(62_000, Input::Tick)), []);
    assert_eq!(b.core.tally.alarms.total(), 1);
    // Well under it for a whole minute: over, and a new burst is a new alarm.
    assert_eq!(alarms(&b.step(200_000, Input::Tick)), []);
    for _ in 0..=COMMANDS_PER_MINUTE {
        b.core.tally.step(t(201_000), &Input::Tick, &b.core.state, true);
    }
    assert_eq!(alarms(&b.step(201_000, Input::Tick)), [Invariant::BoundedEffort]);
}

/// Another invariant breaking while one is broken is told too.
#[test]
fn a_second_invariant_is_told_while_the_first_lasts() {
    let mut b = Bench::new();
    for _ in 0..=COMMANDS_PER_MINUTE {
        b.core.tally.step(t(20_000), &Input::Tick, &b.core.state, true);
    }
    // The record loses the advertising the plan still wants, long enough ago for I1.
    b.core.state.radio.advertising = None;
    b.core.state.silent_since = t(5_000);
    let out = b.core.step(t(20_000), Input::Tick);
    assert_eq!(alarms(&[out]), [Invariant::BoundedEffort, Invariant::Reachable]);
}

/// Told, too many operations lasts down to the end mark; untold, only over the limit counts.
#[test]
fn too_many_operations_ends_at_its_mark() {
    use wrist_ble::policy::invariants::ALARM_HYSTERESIS;
    let at = |commands| Record { commands, phone_linked: true, planned: None, ..record() };
    let mark = COMMANDS_PER_MINUTE - ALARM_HYSTERESIS;
    assert!(at(mark + 1).broken(Invariant::BoundedEffort, t(0), true));
    assert!(!at(mark).broken(Invariant::BoundedEffort, t(0), true));
    assert!(!at(mark + 1).broken(Invariant::BoundedEffort, t(0), false));
    assert!(at(COMMANDS_PER_MINUTE + 1).broken(Invariant::BoundedEffort, t(0), false));
    assert!(!at(COMMANDS_PER_MINUTE).broken(Invariant::BoundedEffort, t(0), false));
}

/// An alarm whose tell finds both slots taken is told at the next step, and counted once.
#[test]
fn an_alarm_crowded_out_of_its_step_is_told_at_the_next() {
    let mut b = Bench::new();
    for _ in 0..=COMMANDS_PER_MINUTE {
        b.core.tally.step(t(20_000), &Input::Tick, &b.core.state, true);
    }
    b.core.state.radio.advertising = None;
    b.core.state.silent_since = t(5_000);
    // A refusal the model did not predict tells a divergence first, in the same step.
    let asked = b.core.step(t(20_000), Input::Tick);
    let mut told = alarms(&[asked]);
    if told.len() < 2 {
        told.extend(alarms(&[b.core.step(t(20_001), Input::Tick)]));
    }
    assert_eq!(told, [Invariant::BoundedEffort, Invariant::Reachable]);
    assert_eq!(b.core.tally.alarms.total(), 2);
}

/// The operation that tips the minute over is counted in the step that asks it.
#[test]
fn the_operation_over_the_limit_alarms_in_its_own_step() {
    let mut b = Bench::new();
    for _ in 0..COMMANDS_PER_MINUTE {
        b.core.tally.step(t(1_000), &Input::Tick, &b.core.state, true);
    }
    let out = b.core.step(t(60_999), Input::Goals(goals(false)));
    assert!(out.op.is_some());
    assert_eq!(alarms(&[out]), [Invariant::BoundedEffort]);
}
