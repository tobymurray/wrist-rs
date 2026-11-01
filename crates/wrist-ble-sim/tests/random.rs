//! Seeded runs over random phones, power, resets, lost events and refusals, every step checked.

use proptest::prelude::*;
use wrist_ble::{Change, Millis, ResetCause, SchedKind};
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
    prop_oneof![
        6 => (os, keys, any::<bool>()).prop_map(|(o, k, f)| Act::Arrive(o, k, f)),
        4 => Just(Act::Leave),
        1 => Just(Act::Vanish),
        4 => power.prop_map(Act::Power),
        1 => any::<bool>().prop_map(Act::Transfer),
        1 => any::<bool>().prop_map(Act::Discoverable),
        1 => any::<bool>().prop_map(Act::Radio),
        1 => Just(Act::Reset),
        1 => (1..4usize).prop_map(Act::LoseEvents),
        1 => Just(Act::Unknown),
        2 => any::<bool>().prop_map(Act::Answer),
        1 => prop_oneof![Just(160u16), Just(792), Just(1592), Just(3992)].prop_map(Act::Diverge),
    ]
}

fn play(seed: u64, script: &[(u32, Act)]) {
    let mut s = Sim::started(seed, on(Power::Battery));
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
    s.run_for(Millis::mins(2));
    if s.fake.ready() {
        assert!(s.core.counters.unknown_events <= unknown);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn the_invariants_hold_whatever_happens(seed in any::<u64>(), script in prop::collection::vec((0..20_000u32, act()), 1..40)) {
        play(seed, &script);
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
        ];
        let script: Vec<(u32, Act)> = (0..400).map(|_| (rng.range(0, 60_000), acts[rng.range(0, 9) as usize])).collect();
        play(seed, &script);
    }
}
