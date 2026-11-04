//! The core on the fake chip: the phone's side, end to end.

use wrist_ble::Millis;
use wrist_ble_core::goals::Power;
use wrist_ble_sim::peers::{Os, Phone};
use wrist_ble_sim::sim::{on, Sim};

#[test]
fn the_watch_advertises_and_a_bonded_phone_connects() {
    let mut s = Sim::started(1, on(Power::Battery));
    assert!(s.line().contains("phone=discoverable(995ms)"), "{}", s.line());
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    s.run_for(Millis::secs(5));
    assert!(s.line().contains("phone=connected"), "{}\n{}", s.line(), s.report());
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(1));
    assert!(s.line().contains("phone=discoverable(995ms)"), "{}", s.line());
    s.power(Power::Cable);
    s.run_for(Millis::secs(1));
    assert!(s.line().contains("phone=discoverable(100ms)"), "{}", s.line());
    s.run_for(Millis::mins(2));
}

