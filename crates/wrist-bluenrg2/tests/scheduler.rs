//! The allocator against PM0257's own example and every refusal the watch measured.

use wrist_ble::{Activities, AdvParams, Admitted, CentralSlot, Change, ConnParams, Duration, Exclusion, SchedKind, Schedule};
use wrist_bluenrg2::{anchor, native, Scheduler, MENU};

const MS: u64 = 1_000_000;

fn adv(units: u16) -> AdvParams {
    AdvParams { interval_min: Duration::from_u16(units), interval_max: Duration::from_u16(units), connectable: true }
}

/// An interval range in ms, quarters of a ms allowed; connection event length in 0.625 ms units.
fn link(min_ms: f32, max_ms: f32, ce: u16) -> ConnParams {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "small positive test values")]
    let u = |ms: f32| Duration::from_u16((ms / 1.25) as u16);
    ConnParams { interval_min: u(min_ms), interval_max: u(max_ms), latency: 0, timeout: Duration::from_u16(600), ce_len: Duration::from_u16(ce) }
}

fn connect(a: &Activities, p: ConnParams) -> Result<Activities, SchedKind> {
    a.walk(&Scheduler, &[Change::Initiate(Some(p)), Change::Connected]).map_err(|(_, k)| k)
}

fn interval_ms(s: CentralSlot) -> f32 {
    f32::from(s.interval.as_u16()) * 1.25
}

#[test]
fn pm0257_figure_14() {
    let a = connect(&Activities::IDLE, link(100.0, 300.0, 32)).unwrap();
    assert_eq!(anchor(&a), Some(200 * MS));
    let a = connect(&a, link(250.0, 500.0, 80)).unwrap();
    let second = a.central[1].unwrap();
    assert_eq!((interval_ms(second), second.latency), (400.0, 2));
    let a = connect(&a, link(50.0, 150.0, 40)).unwrap();
    assert_eq!(anchor(&a), Some(100 * MS));
    let latencies: Vec<u16> = a.central.iter().flatten().map(|s| s.latency).collect();
    assert_eq!(latencies, [2, 4, 1]);
    assert_eq!(interval_ms(a.central[2].unwrap()), 100.0);
    // Slots of 20 and 50 ms with their guards leave 25.5 ms at offset 73: 26 ms does not fit.
    let b = connect(&Activities { central: [a.central[0], a.central[1], None, None, None, None, None, None], ..Activities::IDLE }, link(50.0, 150.0, 42));
    assert_eq!(b, Err(SchedKind::LengthFailed));
}

#[test]
fn a_central_link_beside_advertising_takes_a_submultiple_of_it() {
    let at_100 = Activities { advertising: Some(adv(160)), ..Activities::IDLE };
    let a = connect(&at_100, link(30.0, 50.0, 4)).unwrap();
    assert_eq!(interval_ms(a.central[0].unwrap()), 35.0);
}

#[test]
fn the_refusals_the_watch_measured() {
    let at_1022 = Activities { advertising: Some(adv(1636)), ..Activities::IDLE };
    assert_eq!(connect(&at_1022, link(400.0, 500.0, 4)), Err(SchedKind::NewIntervalFailed));

    let at_420 = connect(&Activities::IDLE, link(420.0, 420.0, 4)).unwrap();
    assert_eq!(Scheduler.admit(&at_420, &Change::Advertise(Some(adv(1636)))), Err(SchedKind::IntervalTooLarge));

    let at_500 = connect(&Activities::IDLE, link(500.0, 500.0, 4)).unwrap();
    assert!(Scheduler.admit(&at_500, &Change::Advertise(Some(adv(1592)))).is_ok());
    let at_35 = connect(&Activities::IDLE, link(35.0, 35.0, 4)).unwrap();
    assert!(Scheduler.admit(&at_35, &Change::Advertise(Some(adv(160)))).is_ok());

    let both = Activities { advertising: Some(adv(792)), ..at_500 };
    assert_eq!(both.walk(&Scheduler, &[Change::Retime { slot: 0, params: link(35.0, 35.0, 4) }]), Err((0, SchedKind::NewIntervalFailed)));
    let both = at_35.walk(&Scheduler, &[Change::Advertise(Some(adv(1560)))]).unwrap();
    assert_eq!(both.walk(&Scheduler, &[Change::Retime { slot: 0, params: link(500.0, 500.0, 4) }]), Err((0, SchedKind::IntervalTooLarge)));
    let alone = both.walk(&Scheduler, &[Change::Advertise(None), Change::Retime { slot: 0, params: link(500.0, 500.0, 4) }]).unwrap();
    assert_eq!(interval_ms(alone.central[0].unwrap()), 500.0);
}

#[test]
fn advertising_while_initiating_is_excluded() {
    let a = Activities { initiating: Some(link(50.0, 50.0, 4)), ..Activities::IDLE };
    let r = Scheduler.admit(&a, &Change::Advertise(Some(adv(1592))));
    assert_eq!(r, Err(SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating)));
    assert_eq!(native(r.unwrap_err()), 0x0C);
    assert_eq!([SchedKind::NewIntervalFailed, SchedKind::IntervalTooLarge, SchedKind::LengthFailed].map(native), [0x84, 0x85, 0x86]);
}

/// A link asked for with no event length took the whole period (UNVERIFIED), and advertising
/// then has no room at any interval, the 0x86 at every rung measured when a phone left beside a
/// fresh 35 ms link.
#[test]
fn a_link_that_takes_the_period_leaves_advertising_no_room() {
    let fresh = connect(&Activities::IDLE, link(35.0, 35.0, 0)).unwrap();
    for units in [160, 792, 1592, 3992] {
        let period = (u64::from(units) * 625_000 + 5 * MS) % (35 * MS);
        let r = Scheduler.admit(&fresh, &Change::Advertise(Some(adv(units))));
        if period == 0 {
            assert_eq!(r, Err(SchedKind::LengthFailed), "{units}");
        } else {
            assert!(r.is_err(), "{units}");
        }
    }
    let sized = connect(&Activities::IDLE, link(35.0, 35.0, 4)).unwrap();
    assert!(Scheduler.admit(&sized, &Change::Advertise(Some(adv(160)))).is_ok());
}

#[test]
fn closing_every_link_resets_the_time_base() {
    let a = connect(&Activities::IDLE, link(35.0, 35.0, 4)).unwrap();
    assert!(connect(&a, link(500.0, 500.0, 4)).is_err());
    let a = a.walk(&Scheduler, &[Change::Close { slot: 0 }]).unwrap();
    assert_eq!(anchor(&a), None);
    assert!(connect(&a, link(500.0, 500.0, 4)).is_ok());
}

#[test]
fn links_beyond_the_chips_count_are_refused() {
    let mut a = Activities { peripheral: 1, ..Activities::IDLE };
    for _ in 0..7 {
        a = connect(&a, link(50.0, 50.0, 2)).unwrap();
    }
    assert_eq!(connect(&a, link(50.0, 50.0, 2)), Err(SchedKind::NoSlot));
    let two = Activities { peripheral: 2, ..Activities::IDLE };
    assert_eq!(Scheduler.admit(&two, &Change::PeripheralUp), Err(SchedKind::NoSlot));
    assert_eq!(Scheduler.admit(&Activities { peripheral: 1, ..Activities::IDLE }, &Change::PeripheralUp), Ok(Admitted::AS_ASKED));
}

/// Every advertising option on the menu fits beside the menu's own central links, both live
/// intervals and the connecting one, in either order.
#[test]
fn the_menu_fits_its_own_scheduler() {
    let central = |a: &Activities, p: ConnParams| connect(a, p).unwrap();
    for choice in MENU.advertising.iter().filter(|c| c.when.beside_central != Some(false)) {
        let with_adv = Activities { advertising: Some(choice.params), ..Activities::IDLE };
        for link in MENU.connecting.iter().chain(MENU.live) {
            let a = central(&with_adv, link.params);
            assert!(a.central[0].is_some());
            let b = central(&Activities::IDLE, link.params);
            assert!(Scheduler.admit(&b, &Change::Advertise(Some(choice.params))).is_ok(), "{choice:?} beside {link:?}");
        }
        let connecting = central(&with_adv, MENU.connecting[0].params);
        for live in MENU.live {
            assert!(connecting.walk(&Scheduler, &[Change::Retime { slot: 0, params: live.params }]).is_ok(), "{live:?} beside {choice:?}");
        }
    }
}
