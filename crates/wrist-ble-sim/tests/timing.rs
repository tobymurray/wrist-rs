//! The core, on the simulator's menu, asks for the driver's timing (`wrist_wake::ble::timing`):
//! one policy, so changing it changes both.

use wrist_ble::{AddrKind, BdAddr, ConnHandle, Duration, Instant, LeConnRole, LinkParams, Op, OpId, PeerAddr, RadioEvent};
use wrist_ble_core::engine::{Core, Input};
use wrist_ble_core::goals::{Goals, Power};
use wrist_bluenrg2::INFO;
use wrist_ble_shadow::menu::MENU;
use wrist_bluenrg2::{Scheduler, CAPABILITIES};
use wrist_wake::ble::{self as wake, timing};

const CASES: [(Power, wake::Power, bool); 4] = [
    (Power::Cable, wake::Power::Cable, false),
    (Power::Cable, wake::Power::Cable, true),
    (Power::Battery, wake::Power::Battery, false),
    (Power::Battery, wake::Power::Battery, true),
];

/// Every operation the core submits, each accepted at once and every advertising op done.
fn run(core: &mut Core<'static>, ops: &mut Vec<Op>, now: u64, input: Input) {
    let mut out = core.step(Instant(now), input);
    while let Some(op) = out.op {
        ops.push(op);
        let id = OpId(u16::try_from(ops.len()).unwrap());
        out = core.step(Instant(now), Input::Submitted(Ok(id)));
        if matches!(op, Op::Advertise(_)) {
            out = core.step(Instant(now), Input::Radio(RadioEvent::Done(id, Ok(wrist_ble::Outcome::Done))));
        }
    }
}

#[test]
fn the_core_asks_the_phone_for_the_drivers_timing() {
    // A link that fits neither ask, so the core always asks.
    let params = LinkParams { interval: Duration::from_u16(40), latency: 0, timeout: Duration::from_u16(400) };
    let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([9, 9, 9, 9, 9, 0xC0]) };
    let link = ConnHandle::new(0x801);
    for (power, wake_power, transfer) in CASES {
        let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
        let mut ops = Vec::new();
        run(&mut core, &mut ops, 0, Input::Goals(Goals { radio_on: true, power, transfer, ..Goals::OFF }));
        let start = core.state.chip.starting().expect("a start in flight");
        run(&mut core, &mut ops, 0, Input::Started(start, Ok(INFO)));
        run(&mut core, &mut ops, 100, Input::Radio(RadioEvent::LinkUp { link, role: LeConnRole::Peripheral, peer, params: Some(params) }));
        run(&mut core, &mut ops, 2_000, Input::Radio(RadioEvent::Encryption { link, on: true }));
        let asked = ops.iter().find_map(|op| match op {
            Op::UpdateParams(l, p) if *l == link => Some(*p),
            _ => None,
        });
        let asked = asked.unwrap_or_else(|| panic!("{power:?}, transfer {transfer}: no ask in {ops:?}"));
        let t = timing(wake_power, transfer);
        let got = (asked.interval_min.as_u16(), asked.interval_max.as_u16(), asked.latency, asked.timeout.as_u16());
        assert_eq!(got, (t.interval_min, t.interval_max, t.latency, t.timeout), "{power:?}, transfer {transfer}");
    }
}

/// With no central link the core advertises at the driver's interval, on either power source.
#[test]
fn the_core_advertises_at_the_drivers_interval() {
    for (power, wake_power) in [(Power::Cable, wake::Power::Cable), (Power::Battery, wake::Power::Battery)] {
        for transfer in [false, true] {
            let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
            let mut ops = Vec::new();
            run(&mut core, &mut ops, 0, Input::Goals(Goals { radio_on: true, power, transfer, ..Goals::OFF }));
            let start = core.state.chip.starting().expect("a start in flight");
            run(&mut core, &mut ops, 0, Input::Started(start, Ok(INFO)));
            let Some(Op::Advertise(Some(a))) = ops.first() else { panic!("{ops:?}") };
            let want = timing(wake_power, false).advertise;
            assert_eq!((a.interval_min.as_u16(), a.interval_max.as_u16()), (want, want), "{power:?}, transfer {transfer}");
        }
    }
}

/// A transfer is the cable's timing for the phone's link only, not for advertising.
#[test]
fn a_transfer_leaves_advertising_at_the_power_sources_interval() {
    for power in [Power::Cable, Power::Battery] {
        let advertised = |transfer: bool| {
            let mut core = Core::new(&CAPABILITIES, &Scheduler, &MENU);
            let mut ops = Vec::new();
            run(&mut core, &mut ops, 0, Input::Goals(Goals { radio_on: true, power, transfer, ..Goals::OFF }));
            let start = core.state.chip.starting().expect("a start in flight");
            run(&mut core, &mut ops, 0, Input::Started(start, Ok(INFO)));
            let Some(Op::Advertise(Some(a))) = ops.first() else { panic!("{ops:?}") };
            (a.interval_min.as_u16(), a.interval_max.as_u16())
        };
        assert_eq!(advertised(true), advertised(false), "{power:?}");
    }
}
