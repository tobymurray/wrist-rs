//! A start the driver fails after the chip's boot: the core restarts the chip within its budget,
//! then holds it as a fault until its retry or the user's.

mod common;

use common::*;
use wrist_ble::policy::chip::{Action, Chip, FaultCause, RESETS_PER_HOUR};
use wrist_ble::policy::engine::{Core, Input};
use wrist_ble::policy::goals::{Goals, PhoneGoal, Power};
use wrist_ble::policy::status::{status, PhoneStatus};
use wrist_bluenrg2::shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};

fn booted() -> Core<'static> {
    let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
    let goals = Goals { radio_on: true, phone: PhoneGoal { discoverable: true, pairing_open: true }, power: Power::Cable, ..Goals::OFF };
    core.step(t(0), Input::Goals(goals));
    let start = core.state.chip.starting().expect("a start");
    core.step(t(0), Input::Started(start, Ok(INFO)));
    core
}

/// The chip's boot told the core it was up; a later step failing is a failed start all the same.
#[test]
fn a_step_failed_after_the_boot_restarts_the_chip() {
    let mut core = booted();
    assert!(matches!(core.state.chip.state, Chip::Ready(_)));
    let out = core.step(t(1_000), Input::StartFailed);
    assert!(matches!(out.chip, Some(Action::Start(_))), "{out:?}");
}

/// Past the hour's budget the chip is held as a fault, shown with its cause, until its retry.
#[test]
fn starts_failing_past_the_budget_are_a_fault_until_the_retry() {
    let mut core = booted();
    let mut held = None;
    for i in 0..=RESETS_PER_HOUR as u64 {
        let at = t(1_000 + i * 3_000);
        let out = core.step(at, Input::StartFailed);
        if out.chip == Some(Action::Hold) {
            held = Some(at);
            break;
        }
        let start = core.state.chip.starting().expect("a restart");
        core.step(at, Input::Started(start, Ok(INFO)));
    }
    let held = held.expect("held past the budget");
    let Chip::Fault { cause: FaultCause::StartFailed, retry_at } = core.state.chip.state else { panic!("{:?}", core.state.chip.state) };
    assert!(retry_at > held);
    assert!(matches!(status(&core.state, held).phone, PhoneStatus::Fault { .. }));
    assert_eq!(core.step(retry_at, Input::Tick).chip.map(|a| matches!(a, Action::Start(_))), Some(true));
}

/// The user asks again: a fresh start at once.
#[test]
fn the_users_retry_starts_a_held_chip_at_once() {
    let mut core = booted();
    let mut at = 1_000;
    while !matches!(core.state.chip.state, Chip::Fault { .. }) {
        core.step(t(at), Input::StartFailed);
        if let Some(start) = core.state.chip.starting() {
            core.step(t(at), Input::Started(start, Ok(INFO)));
        }
        at += 3_000;
    }
    let out = core.step(t(at), Input::RetryRadio);
    assert!(matches!(out.chip, Some(Action::Start(_))), "{out:?}");
}

#[test]
fn the_records_of_a_failed_start_and_a_retry_are_kept_for_the_replay() {
    use wrist_ble_sim::fixture::keep;
    assert_eq!(keep("[trace] 5000 f").as_deref(), Some("5.000 [trace] 5000 f"));
    assert_eq!(keep("[trace] 9000 y").as_deref(), Some("9.000 [trace] 9000 y"));
}

/// The user's retry has the hour's whole budget: a reset soon after is restarted, not held.
#[test]
fn the_users_retry_starts_with_a_fresh_budget() {
    use wrist_ble::{RadioEvent, ResetCause};
    let mut core = booted();
    let mut at = 1_000;
    while !matches!(core.state.chip.state, Chip::Fault { .. }) {
        core.step(t(at), Input::StartFailed);
        if let Some(start) = core.state.chip.starting() {
            core.step(t(at), Input::Started(start, Ok(INFO)));
        }
        at += 3_000;
    }
    core.step(t(at), Input::RetryRadio);
    let start = core.state.chip.starting().expect("a start");
    core.step(t(at), Input::Started(start, Ok(INFO)));
    let out = core.step(t(at + 600_000), Input::Radio(RadioEvent::ChipReset(ResetCause::Watchdog)));
    assert!(matches!(out.chip, Some(Action::Start(_))), "{out:?}");
}

/// The user's ask in the same instant the fault's own retry started the chip: the start stands,
/// with the whole budget.
#[test]
fn the_users_retry_as_the_hours_retry_starts_keeps_the_fresh_budget() {
    use wrist_ble::{RadioEvent, ResetCause};
    let mut core = booted();
    let mut at = 1_000;
    while !matches!(core.state.chip.state, Chip::Fault { .. }) {
        core.step(t(at), Input::StartFailed);
        if let Some(start) = core.state.chip.starting() {
            core.step(t(at), Input::Started(start, Ok(INFO)));
        }
        at += 3_000;
    }
    let Chip::Fault { retry_at, .. } = core.state.chip.state else { unreachable!() };
    assert!(matches!(core.step(retry_at, Input::Tick).chip, Some(Action::Start(_))));
    assert_eq!(core.step(retry_at, Input::RetryRadio).chip, None);
    let start = core.state.chip.starting().expect("the hour's start stands");
    core.step(retry_at, Input::Started(start, Ok(INFO)));
    let out = core.step(retry_at + wrist_ble::Millis::secs(180), Input::Radio(RadioEvent::ChipReset(ResetCause::Watchdog)));
    assert!(matches!(out.chip, Some(Action::Start(_))), "{out:?}");
}
