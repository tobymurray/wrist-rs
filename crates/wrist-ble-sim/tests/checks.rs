//! Each check fails when the truth is broken behind the core's back.

use wrist_ble::{AdvParams, Duration, Millis, Op, ScanParams, Status};
use wrist_ble_core::goals::Power;
use wrist_ble_sim::fake::Note;
use wrist_ble_sim::peers::{Os, Phone};
use wrist_ble_sim::sim::{on, Sim};

fn noted(note: Note) {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.notes.push((s.now, note));
    s.run_for(Millis(100));
}

fn phone_up() -> Sim {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    s.run_for(Millis::secs(5));
    s
}

#[test]
#[should_panic(expected = "security order: a procedure crossed encryption")]
fn a_procedure_across_encryption() {
    let s = phone_up();
    let l = s.fake.phone_link().unwrap().0;
    noted(Note::CrossedEncryption(l));
}

#[test]
#[should_panic(expected = "phone first: a central start during the update")]
fn a_central_start_during_an_update() {
    let s = phone_up();
    noted(Note::CentralInUpdate(s.fake.phone_link().unwrap().0));
}

#[test]
#[should_panic(expected = "security order: MIC failure")]
fn a_mic_failure() {
    let s = phone_up();
    noted(Note::LinkDown { link: s.fake.phone_link().unwrap().0, reason: Status::CONN_TERMINATED_DUE_TO_MIC_FAILURE });
}

#[test]
#[should_panic(expected = "phone first: instant passed")]
fn an_instant_passed() {
    let s = phone_up();
    noted(Note::LinkDown { link: s.fake.phone_link().unwrap().0, reason: Status::INSTANT_PASSED });
}

#[test]
#[should_panic(expected = "bounded effort")]
fn too_many_commands() {
    let mut s = Sim::started(1, on(Power::Battery));
    for _ in 0..=60 {
        s.fake.notes.push((s.now, Note::Submitted { op: Op::Advertise(None), accepted: true }));
    }
    s.run_for(Millis(100));
}

#[test]
#[should_panic(expected = "phone first: Scan")]
fn a_scan_as_the_phone_connects() {
    let mut s = phone_up();
    s.fake.phone_leaves(0);
    s.run_for(Millis::secs(3));
    s.fake.phone_arrives(Phone::new(Os::Linux, 1));
    while s.fake.phone_link().is_none() {
        s.run_for(Millis(10));
    }
    let scan = ScanParams { interval: Duration::from_u16(160), window: Duration::from_u16(32), active: false, duration: Millis::secs(10) };
    s.fake.notes.push((s.now, Note::Submitted { op: Op::Scan(Some(scan)), accepted: true }));
    s.run_for(Millis(10));
}

#[test]
#[should_panic(expected = "reachable: a gap")]
fn advertising_stopped() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.submit(Op::Advertise(None)).unwrap();
    s.run_for(Millis::secs(5));
}

#[test]
#[should_panic(expected = "truthful status: discoverable")]
fn advertising_stopped_while_the_status_says_discoverable() {
    let mut s = Sim::started(1, on(Power::Battery));
    s.fake.submit(Op::Advertise(None)).unwrap();
    for _ in 0..50 {
        s.run_for(Millis(100));
    }
}

#[test]
#[should_panic(expected = "converges")]
fn advertising_retimed() {
    let mut s = Sim::started(1, on(Power::Battery));
    let fast = AdvParams { interval_min: Duration::from_u16(160), interval_max: Duration::from_u16(160), connectable: true };
    s.fake.submit(Op::Advertise(Some(fast))).unwrap();
    s.run_for(Millis::secs(20));
}
