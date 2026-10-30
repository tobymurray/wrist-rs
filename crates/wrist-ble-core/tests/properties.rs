//! The status's properties over generated traces of the machines.

mod common;

use core::mem::discriminant;

use common::*;
use proptest::prelude::*;
use wrist_ble::{FailureKind, Instant, Millis, PairingStep, Status};
use wrist_ble_core::peripheral::{fresh_for, Ctx, Event, Peripheral, Stage, StopReason, Then, GIVE_UP};
use wrist_ble_core::phone::{self, Phone, Security};
use wrist_ble_core::plan::Conflict;
use wrist_ble_core::profile::{Contact, Profile, Reading};
use wrist_ble_core::status::{peer_state, status, PeerState, PhoneStatus, HIDE_AFTER};

const SAMPLE: Millis = Millis(100);

/// Within any window this long a peripheral's status row changes at most [`MAX_CHANGES`] times.
const FLICKER_WINDOW: Millis = Millis::secs(10);
const MAX_CHANGES: usize = 3;

/// One step of a trace: the time before it, and two draws that pick its event.
#[derive(Clone, Copy, Debug)]
struct Step {
    gap: Millis,
    roll: u8,
    pick: u8,
}

/// Mostly a strap's second, sometimes a missed reading or a long silence.
fn gap() -> impl Strategy<Value = Millis> {
    prop_oneof![
        70 => 800..1_200u32,
        15 => 1_500..4_500u32,
        10 => 5_000..9_000u32,
        5 => 10_000..25_000u32,
    ]
    .prop_map(Millis)
}

fn steps(len: usize) -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec((gap(), 0..100u8, any::<u8>()).prop_map(|(gap, roll, pick)| Step { gap, roll, pick }), len)
}

fn check(p: &Peripheral, profile: &Profile, now: Instant) -> Result<(), TestCaseError> {
    let s = peer_state(p, profile, now);
    let stopped = match p.stage {
        Stage::Stopped(r) | Stage::Dropping { then: Then::Stop(r), .. } => Some(r),
        _ => None,
    };
    match s {
        PeerState::Live { current, age, .. } | PeerState::Unstable { current, age, value: Some(_) } => {
            prop_assert!(!current || age <= fresh_for(profile), "{s:?} at {now:?}");
            prop_assert!(age <= HIDE_AFTER, "{s:?} at {now:?}");
        }
        PeerState::Quiet { value: Some(_), age } => prop_assert!(age <= HIDE_AFTER, "{s:?} at {now:?}"),
        _ => {}
    }
    if matches!(s, PeerState::Live { .. }) {
        prop_assert!(p.last.is_some_and(|(at, _)| now.since(at) <= profile.quiet), "{s:?} at {now:?}");
    }
    if matches!(s, PeerState::Live { .. } | PeerState::Unstable { .. } | PeerState::Idle | PeerState::NoContact | PeerState::Quiet { .. }) {
        prop_assert!(matches!(p.stage, Stage::Live { .. }), "{s:?} from {p:?}");
    }
    match stopped {
        Some(StopReason::Incompatible) => prop_assert_eq!(s, PeerState::Incompatible),
        Some(r) => prop_assert_eq!(s, PeerState::Stopped(r)),
        None => prop_assert!(!matches!(s, PeerState::Stopped(_) | PeerState::Incompatible), "{s:?} from {p:?}"),
    }
    if p.stage == Stage::Off {
        prop_assert_eq!(s, PeerState::Off);
    }
    Ok(())
}

/// Ticks at every deadline and checks the status every [`SAMPLE`] up to `to`.
fn advance(mut p: Peripheral, ctx: &Ctx<'_>, mut now: Instant, to: Instant) -> Result<Peripheral, TestCaseError> {
    while now < to {
        let sample = (now + SAMPLE).min(to);
        while let Some(due) = p.deadline(ctx, now).filter(|&d| d <= sample) {
            let next = p.step(ctx, due.max(now), Event::Tick);
            prop_assert!(due > now || next != p, "stuck at {now:?} with {p:?}");
            now = due.max(now);
            p = next;
            check(&p, ctx.profile, now)?;
        }
        now = sample;
        check(&p, ctx.profile, now)?;
    }
    Ok(p)
}

fn event(step: Step, p: &Peripheral, readings: bool) -> Option<Event> {
    let Step { roll, pick, .. } = step;
    let link = sensor();
    let reason = [Status::CONN_TIMEOUT, Status::REMOTE_USER_TERMINATED_CONN, Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT][usize::from(pick % 3)];
    Some(match p.stage {
        _ if roll < 2 => Event::Busy(Conflict::Transfer),
        _ if roll < 3 => Event::Unwant,
        Stage::Off => Event::Want,
        Stage::Stopped(StopReason::RadioBusy(_)) if roll < 50 => Event::Free,
        Stage::Stopped(_) if roll < 10 => Event::Retry,
        Stage::Seeking { .. } if roll < 40 => Event::LinkUp(link),
        Stage::Seeking { .. } if roll < 60 => Event::ConnectFailed([FailureKind::NotEstablished, FailureKind::Timeout][usize::from(pick % 2)]),
        Stage::Linked { .. } if roll < 10 => Event::LinkDown(link, reason),
        Stage::Linked { .. } if roll < 13 => Event::ProcedureFailed,
        Stage::Linked { .. } => [Event::Secured(roll < 90), Event::Discovered(roll < 85), Event::Subscribed(roll < 90)][usize::from(pick % 3)],
        Stage::Live { .. } if roll < 6 => Event::LinkDown(link, reason),
        Stage::Live { .. } if readings => {
            let contact = if roll < 10 { Contact::Lost } else { Contact::Detected };
            Event::Reading(Reading { value: 60 + u16::from(pick), contact })
        }
        Stage::Dropping { .. } => Event::LinkDown(link, Status::CONN_TERMINATED_BY_LOCAL_HOST),
        _ => return None,
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn peripheral_status_follows_its_state(idle in any::<bool>(), workout in any::<bool>(), trace in steps(400)) {
        let profile: &'static Profile = if idle { &CSC } else { &HR };
        let c = Ctx { workout, ..ctx(profile) };
        let mut p = Peripheral::default();
        let mut now = t(0);
        for step in trace {
            let to = now + step.gap;
            p = advance(p, &c, now, to)?;
            now = to;
            if let Some(e) = event(step, &p, true) {
                p = p.step(&c, now, e);
                check(&p, profile, now)?;
            }
        }
    }

    #[test]
    fn a_peripheral_that_never_delivers_stops_in_bounded_time(trace in steps(2_000)) {
        let c = ctx(&HR);
        let mut p = Peripheral::default().step(&c, t(0), Event::Want);
        let mut now = t(0);
        let limit = t(0) + GIVE_UP + Millis::secs(26);
        for step in trace {
            if matches!(p.stage, Stage::Stopped(_)) {
                break;
            }
            let to = now + step.gap;
            p = advance(p, &c, now, to)?;
            now = to;
            match event(step, &p, false) {
                Some(Event::Busy(_) | Event::Unwant | Event::Retry | Event::Want) | None => {}
                Some(e) => p = p.step(&c, now, e),
            }
            let reached = matches!(p.stage, Stage::Stopped(_) | Stage::Dropping { then: Then::Stop(_), .. });
            prop_assert!(reached || now <= limit, "{p:?} at {now:?}");
        }
    }

    #[test]
    fn phone_connected_only_when_up_and_encrypted(trace in prop::collection::vec((0..8_000u32, 0..9u8, any::<u32>()), 300)) {
        let mut state = ready(t(0));
        let mut now = t(0);
        for (gap, kind, value) in trace {
            now = now + Millis(gap);
            let e = match kind {
                0 => phone::Event::LinkUp(phone()),
                1 => phone::Event::LinkDown(phone()),
                2 => phone::Event::Encryption(phone(), value % 4 != 0),
                3 => phone::Event::Pairing(phone(), PairingStep::Numeric(value % 1_000_000)),
                4 => phone::Event::Pairing(phone(), PairingStep::Failed),
                5 => phone::Event::BondLost(phone()),
                6 => phone::Event::Answer(value % 2 == 0),
                _ => phone::Event::Tick,
            };
            state.phone = state.phone.step(now, e).0;
            if state.phone.deadline().is_some_and(|due| due <= now) {
                state.phone = state.phone.step(now, phone::Event::Tick).0;
                prop_assert!(state.phone.deadline().is_none_or(|d| d > now), "stuck {:?}", state.phone);
            }
            let s = status(&state, now).phone;
            if s == PhoneStatus::Connected {
                prop_assert!(matches!(state.phone, Phone::Up(l) if l.security == Security::Secure), "{s:?}");
            }
            if matches!(s, PhoneStatus::Pairing { .. }) {
                prop_assert!(matches!(state.phone, Phone::Up(l) if matches!(l.security, Security::Pairing { .. })), "{s:?}");
            }
            if matches!(state.phone, Phone::Up(_)) {
                prop_assert!(!matches!(s, PhoneStatus::Discoverable { .. } | PhoneStatus::Unreachable { .. }), "{s:?}");
            }
        }
    }

    #[test]
    fn a_live_link_does_not_flicker_whatever_its_readings_do(gaps in prop::collection::vec(gap(), 1_800)) {
        let c = ctx(&HR);
        let mut p = live(&c, t(0));
        let mut changes: Vec<Instant> = Vec::new();
        let mut last = discriminant(&peer_state(&p, &HR, t(0)));
        let mut now = t(0);
        let mut gaps = gaps.into_iter();
        let mut next_reading = gaps.next().map(|g| now + g);
        while let Some(reading) = next_reading {
            if !matches!(p.stage, Stage::Live { .. }) {
                break;
            }
            now = now + SAMPLE;
            if now >= reading {
                p = p.step(&c, reading, bpm(70));
                next_reading = gaps.next().map(|g| reading + g);
            }
            if let Some(due) = p.deadline(&c, now).filter(|&d| d <= now) {
                p = p.step(&c, due, Event::Tick);
            }
            let class = discriminant(&peer_state(&p, &HR, now));
            if class != last {
                last = class;
                changes.push(now);
                let recent = changes.iter().filter(|&&at| now.since(at) < FLICKER_WINDOW).count();
                prop_assert!(recent <= MAX_CHANGES, "{recent} changes in 10 s by {now:?}: {:?}", peer_state(&p, &HR, now));
            }
        }
    }
}
