//! Seeded runs over random phones, power, resets and restarts, lost events and refusals, every step
//! checked.

use proptest::prelude::*;
use wrist_ble::{Change, Millis, ResetCause, SchedKind};
use wrist_ble_core::chip::{Chip, FaultCause};
use wrist_ble_core::engine::Input;
use wrist_ble_core::goals::{Goals, Power};
use wrist_ble_sim::peers::{Keys, Os, Phone};
use wrist_ble_sim::sim::{on, Sim};

#[derive(Clone, Copy, Debug)]
enum Act {
    Arrive(Os, Keys, bool),
    Leave,
    Vanish,
    Power(Power),
    Transfer(bool),
    Discoverable(bool),
    Radio(bool),
    Reset,
    /// The chip restarts by itself and comes back up unasked, the updater among the reasons.
    Reboot(ResetCause),
    LoseEvents(usize),
    Unknown,
    Answer(bool),
    /// The chip refuses advertising at this many 0.625 ms units, though the model admits it.
    Diverge(u16),
}

fn act() -> impl Strategy<Value = Act> {
    let os = prop_oneof![Just(Os::Mac), Just(Os::Android), Just(Os::Linux)];
    let keys = prop_oneof![4 => Just(Keys::Bonded), 1 => Just(Keys::None), 1 => Just(Keys::PhoneLost), 1 => Just(Keys::ChipLost)];
    let power = prop_oneof![Just(Power::Cable), Just(Power::Battery)];
    let cause = prop_oneof![Just(ResetCause::PowerOn), Just(ResetCause::Watchdog), Just(ResetCause::Brownout), Just(ResetCause::Updater)];
    prop_oneof![
        6 => (os, keys, any::<bool>()).prop_map(|(o, k, f)| Act::Arrive(o, k, f)),
        4 => Just(Act::Leave),
        1 => Just(Act::Vanish),
        4 => power.prop_map(Act::Power),
        1 => any::<bool>().prop_map(Act::Transfer),
        1 => any::<bool>().prop_map(Act::Discoverable),
        1 => any::<bool>().prop_map(Act::Radio),
        1 => Just(Act::Reset),
        1 => cause.prop_map(Act::Reboot),
        1 => (1..4usize).prop_map(Act::LoseEvents),
        1 => Just(Act::Unknown),
        2 => any::<bool>().prop_map(Act::Answer),
        1 => prop_oneof![Just(160u16), Just(792), Just(1592), Just(3992)].prop_map(Act::Diverge),
    ]
}

fn play(seed: u64, script: &[(u32, Act)]) {
    let mut s = Sim::started(seed, on(Power::Battery));
    run(&mut s, script);
    s.run_for(Millis::mins(2));
}

fn run(s: &mut Sim, script: &[(u32, Act)]) {
    let mut unknown = 0;
    for &(wait, a) in script {
        s.run_for(Millis(wait));
        let g = *s.core.goals();
        match a {
            Act::Arrive(os, keys, first) => {
                s.fake.phone_arrives(Phone { keys, bond_lost_first: first, ..Phone::new(os, 1) });
            }
            Act::Leave => s.fake.phone_leaves(0),
            Act::Vanish => s.fake.phone_vanishes(0),
            Act::Power(p) => s.power(p),
            Act::Transfer(t) => s.goals(Goals { transfer: t, ..g }),
            Act::Discoverable(d) => s.goals(Goals { phone: wrist_ble_core::goals::PhoneGoal { discoverable: d, ..g.phone }, ..g }),
            Act::Radio(r) => s.goals(Goals { radio_on: r, ..g }),
            Act::Reset => s.fake.reset(ResetCause::Crash),
            Act::Reboot(cause) => s.fake.reboot(cause),
            Act::LoseEvents(n) => s.fake.lose_events(n),
            Act::Unknown => {
                s.fake.unknown(0x0C99);
                unknown += 1;
            }
            Act::Answer(yes) => s.input(Input::Answer(yes)),
            Act::Diverge(units) => s.fake.diverge(Box::new(move |_, c| match c {
                Change::Advertise(Some(p)) if p.interval_min.as_u16() == units => Some(SchedKind::LengthFailed),
                _ => None,
            })),
        }
    }
    if s.fake.ready() {
        assert!(s.core.counters.unknown_events <= unknown);
    }
}

/// Whatever came before, a chip that restarts by itself under a phone's link, once in the hour, is
/// brought up again: advertising or linked, and not given up on as one that kept resetting.
fn restart_under_a_link(seed: u64, script: &[(u32, Act)], cause: ResetCause) {
    let mut s = Sim::started(seed, on(Power::Battery));
    let calm: Vec<(u32, Act)> = script.iter().copied().filter(|(_, a)| !matches!(a, Act::Reset | Act::Reboot(_) | Act::Radio(_) | Act::Diverge(_))).collect();
    run(&mut s, &calm);
    s.goals(on(Power::Battery));
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(10));
    s.fake.phone_arrives(Phone::new(Os::Android, 2));
    s.run_for(Millis::secs(30));
    assert!(s.fake.phone_link().is_some(), "seed {seed}: no link to restart under: {}", s.line());
    let resets = s.core.counters.resets;
    s.fake.reboot(cause);
    s.run_for(Millis::mins(2));
    assert!(s.fake.advertising().is_some() || s.fake.phone_link().is_some(), "seed {seed} {cause:?}: silent: {}", s.line());
    assert!(!matches!(s.core.state.chip.state, Chip::Fault { cause: FaultCause::KeptResetting, .. }), "seed {seed} {cause:?}: {}", s.line());
    assert!(s.core.counters.resets > resets, "seed {seed} {cause:?}: the restart went uncounted");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn the_invariants_hold_whatever_happens(seed in any::<u64>(), script in prop::collection::vec((0..20_000u32, act()), 1..40)) {
        play(seed, &script);
    }

    #[test]
    fn a_restart_under_a_link_is_recovered(
        seed in any::<u64>(),
        script in prop::collection::vec((0..20_000u32, act()), 0..20),
        cause in prop_oneof![Just(ResetCause::PowerOn), Just(ResetCause::Watchdog), Just(ResetCause::Brownout), Just(ResetCause::Updater)],
    ) {
        restart_under_a_link(seed, &script, cause);
    }
}

/// Fixed seeds, so that every run covers the same ground as well as new.
#[test]
fn the_invariants_hold_over_hours() {
    for seed in 0..8 {
        let mut rng = wrist_ble_sim::rng::Rng::new(seed);
        let acts = [
            Act::Arrive(Os::Linux, Keys::Bonded, true),
            Act::Arrive(Os::Mac, Keys::Bonded, true),
            Act::Arrive(Os::Android, Keys::None, false),
            Act::Leave,
            Act::Leave,
            Act::Power(Power::Cable),
            Act::Power(Power::Battery),
            Act::Answer(true),
            Act::Reset,
            Act::LoseEvents(2),
            Act::Reboot(ResetCause::Updater),
            Act::Reboot(ResetCause::Watchdog),
        ];
        let last = u32::try_from(acts.len() - 1).unwrap_or(0);
        let script: Vec<(u32, Act)> = (0..400).map(|_| (rng.range(0, 60_000), acts[rng.range(0, last) as usize])).collect();
        play(seed, &script);
    }
}

/// Found by the random runs: a chip that restarts by itself while the core is starting it again
/// after a reset, so that start is never answered.
#[test]
fn a_reboot_under_a_start_is_started_again_at_once() {
    play(73_839_474_304_694_654, &[(0, Act::Reset), (0, Act::Reboot(ResetCause::Watchdog))]);
}
