//! The allocator against PM0257's own example and the refusals it predicts.

use wrist_ble::{Activities, AdvParams, Admitted, CentralSlot, Change, ConnParams, Duration, Exclusion, Millis, ScanParams, SchedKind, Schedule};
use wrist_bluenrg2::{anchor, native, Scheduler};

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
fn pm0257_figure_14_at_fixed_lengths() {
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
    let offsets: Vec<u32> = a.central.iter().flatten().map(|s| s.offset.0).collect();
    assert_eq!(offsets, [0, 21_500, 73_000]);
    // Slots of 20 and 50 ms with their guards leave 25.5 ms at offset 73: 26 ms does not fit.
    let b = connect(&Activities { central: [a.central[0], a.central[1], None, None, None, None, None, None], ..Activities::IDLE }, link(50.0, 150.0, 42));
    assert_eq!(b, Err(SchedKind::LengthFailed));
}

/// PM0257 §4.2.2.
#[test]
fn a_slot_fits_a_gap_or_the_time_after_the_last() {
    let mut a = Activities::IDLE;
    for ce in [32, 80, 16] {
        a = connect(&a, link(100.0, 100.0, ce)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    assert_eq!(connect(&a, link(100.0, 100.0, 88)), Err(SchedKind::LengthFailed));
    let gap = connect(&a, link(100.0, 100.0, 80)).unwrap();
    assert_eq!(gap.central[1].map(|s| s.offset.0), Some(21_500));
    let after = connect(&a, link(100.0, 100.0, 16)).unwrap();
    assert_eq!(after.central[1].map(|s| s.offset.0), Some(21_500));
    let tail = connect(&gap, link(100.0, 100.0, 16)).unwrap();
    assert_eq!(tail.central[3].map(|s| s.offset.0), Some(84_500));
}

#[test]
fn a_slot_after_the_last_follows_the_latest_in_time() {
    let mut a = Activities::IDLE;
    for _ in 0..3 {
        a = connect(&a, link(100.0, 100.0, 16)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    let a = connect(&a, link(100.0, 100.0, 24)).unwrap();
    assert_eq!(a.central[1].map(|s| s.offset.0), Some(34_500));
    let a = connect(&a, link(100.0, 100.0, 24)).unwrap();
    assert_eq!(a.central[3].map(|s| s.offset.0), Some(51_000));
}

#[test]
fn a_retimed_link_may_keep_its_place() {
    let a = connect(&Activities::IDLE, link(35.0, 35.0, 4)).unwrap();
    let a = a.walk(&Scheduler, &[Change::Retime { slot: 0, params: link(35.0, 35.0, 8) }]).unwrap();
    assert_eq!(a.central[0].map(|s| (s.offset.0, s.ce_len.as_u16())), Some((0, 8)));
}

#[test]
fn a_link_beside_advertising_leaves_it_its_event() {
    let a = Activities { advertising: Some(adv(160)), ..Activities::IDLE };
    assert_eq!(connect(&a, link(105.0, 105.0, 140)), Err(SchedKind::LengthFailed));
    assert!(connect(&a, link(105.0, 105.0, 139)).is_ok());
}

/// PM0257 §4.2.2, case 3.
#[test]
fn a_shorter_anchor_holds_every_slot_already_placed() {
    let mut a = Activities::IDLE;
    for ce in [16, 160, 16] {
        a = connect(&a, link(200.0, 200.0, ce)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    assert_eq!(connect(&a, link(50.0, 100.0, 16)), Err(SchedKind::LengthFailed));
    let a = a.walk(&Scheduler, &[Change::Close { slot: 2 }]).unwrap();
    assert_eq!(anchor(&connect(&a, link(50.0, 100.0, 16)).unwrap()), Some(100 * MS));
}

#[test]
fn a_central_link_beside_advertising_takes_a_submultiple_of_it() {
    let at_100 = Activities { advertising: Some(adv(160)), ..Activities::IDLE };
    let a = connect(&at_100, link(30.0, 50.0, 4)).unwrap();
    assert_eq!(interval_ms(a.central[0].unwrap()), 35.0);
}

#[test]
fn refusals_beside_advertising_and_on_retiming() {
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
fn initiating_excludes_advertising_and_another_initiation() {
    let a = Activities { initiating: Some(link(50.0, 50.0, 4)), ..Activities::IDLE };
    let r = Scheduler.admit(&a, &Change::Advertise(Some(adv(1592))));
    assert_eq!(r, Err(SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating)));
    assert_eq!(native(r.unwrap_err()), 0x0C);
    assert_eq!(Scheduler.admit(&a, &Change::Initiate(Some(link(50.0, 50.0, 4)))), Err(SchedKind::Excluded(Exclusion::NoInitiateWhileInitiating)));
    assert_eq!([SchedKind::NoSlot, SchedKind::NewIntervalFailed, SchedKind::IntervalTooLarge, SchedKind::LengthFailed].map(native), [0x82, 0x84, 0x85, 0x86]);
}

/// A link asked for with no event length takes the whole period, leaving advertising no room.
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
fn a_short_link_beside_a_long_one_rescales_it_by_hundreds() {
    let slow = connect(&Activities::IDLE, link(4000.0, 4000.0, 2)).unwrap();
    let r = Scheduler.admit(&slow, &Change::Initiate(Some(link(10.0, 10.0, 2))));
    assert_eq!(r.map(|a| a.latency_scale), Ok(400));
    assert_eq!(connect(&slow, link(10.0, 10.0, 2)).unwrap().central[0].map(|s| s.latency), Some(400));
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

fn scan(units: u16) -> ScanParams {
    ScanParams { interval: Duration::from_u16(units), window: Duration::from_u16(units), active: false, duration: Millis::secs(5) }
}

#[test]
fn a_scan_adapts_to_the_first_link() {
    let a = Activities { scanning: Some(scan(16)), ..Activities::IDLE };
    let a = connect(&a, link(500.0, 500.0, 4)).unwrap();
    assert_eq!(a.central[0].map(|s| (interval_ms(s), s.latency)), Some((500.0, 1)));
    let a = Activities { scanning: None, ..a };
    assert!(Scheduler.admit(&a, &Change::Advertise(Some(adv(792)))).is_ok());
    let a = Activities { scanning: Some(scan(32)), ..Activities::IDLE };
    assert!(connect(&a, link(50.0, 50.0, 4)).is_ok());
}

#[test]
fn a_first_link_fits_advertising_beside_a_scan() {
    let a = Activities { advertising: Some(adv(792)), scanning: Some(scan(56)), ..Activities::IDLE };
    let a = connect(&a, link(30.0, 50.0, 4)).unwrap();
    assert_eq!(interval_ms(a.central[0].unwrap()), 50.0);
    assert!(Scheduler.admit(&a, &Change::Advertise(Some(adv(792)))).is_ok());
}

#[test]
fn a_lone_link_retimes_beside_a_scan() {
    let a = connect(&Activities::IDLE, link(35.0, 35.0, 4)).unwrap();
    let a = Activities { scanning: Some(scan(160)), ..a };
    let a = a.walk(&Scheduler, &[Change::Retime { slot: 0, params: link(500.0, 500.0, 4) }]).unwrap();
    assert_eq!(interval_ms(a.central[0].unwrap()), 500.0);
}

#[test]
fn a_phone_connecting_ends_advertising() {
    let a = Activities { advertising: Some(adv(160)), ..Activities::IDLE };
    let a = a.walk(&Scheduler, &[Change::PeripheralUp]).unwrap();
    assert_eq!((a.advertising, a.peripheral), (None, 1));
    assert!(connect(&a, link(50.0, 50.0, 4)).is_ok());
}

/// PM0257 §4.3: advertising whose period is a multiple of the scan's runs beside it, as the watch
/// does (HARDWARE.md, a scan beside advertising every 995 ms heard one window per period).
#[test]
fn advertising_a_multiple_of_a_scan_runs_beside_it() {
    let a = Activities { scanning: Some(scan(160)), ..Activities::IDLE };
    assert_eq!(Scheduler.admit(&a, &Change::Advertise(Some(adv(1592)))), Ok(Admitted::AS_ASKED));
}

/// PM0257 §4.2.3, `ble_status.h`'s names: advertising shorter than the anchor fails as a new
/// interval, longer as too large. The short case is unmeasured, a probe cell.
#[test]
fn advertising_shorter_than_the_anchor_is_a_new_interval_refused() {
    let at_500 = connect(&Activities::IDLE, link(500.0, 500.0, 4)).unwrap();
    assert_eq!(Scheduler.admit(&at_500, &Change::Advertise(Some(adv(160)))), Err(SchedKind::NewIntervalFailed));
}

/// Closing one link leaves the time base as it was (PM0257 §4.3: only closing every link resets
/// it), so the period stays the one the closed link set.
#[test]
fn closing_one_link_keeps_the_anchor_it_set() {
    let mut a = Activities::IDLE;
    for (min, max, ce) in [(100.0, 300.0, 32), (250.0, 500.0, 80), (50.0, 150.0, 40)] {
        a = connect(&a, link(min, max, ce)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 2 }]).unwrap();
    assert_eq!(anchor(&a), Some(100 * MS));
}

/// The chip holds eight links in all (`CAPABILITIES.max_links`), the phone's among them.
#[test]
fn a_phone_beside_seven_centrals_is_the_ninth_link_refused() {
    let mut a = Activities { peripheral: 1, ..Activities::IDLE };
    for _ in 0..7 {
        a = connect(&a, link(50.0, 50.0, 2)).unwrap();
    }
    assert_eq!(Scheduler.admit(&a, &Change::PeripheralUp), Err(SchedKind::NoSlot));
    let mut a = Activities::IDLE;
    for _ in 0..8 {
        a = connect(&a, link(50.0, 50.0, 2)).unwrap();
    }
    assert_eq!(Scheduler.admit(&a, &Change::PeripheralUp), Err(SchedKind::NoSlot));
}

/// PM0257 §4.2.2, case 3: a shorter anchor keeps a slot that ends with its guard exactly at the
/// new period, and a link fits the gap before it.
#[test]
fn a_slot_ending_with_its_guard_at_the_period_still_fits() {
    let mut a = Activities::IDLE;
    // 148 slots of 0.625 ms and five guards of 1.5 ms: 100 ms exactly.
    for ce in [30, 30, 30, 30, 28] {
        a = connect(&a, link(200.0, 200.0, ce)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    let b = connect(&a, link(50.0, 100.0, 30)).unwrap();
    assert_eq!(anchor(&b), Some(100 * MS));
    assert_eq!(b.central[1].map(|s| s.offset.0), Some(20_250));
}

/// PM0257 §4.2.2: a link asked for with no event length is given the free space, after the last
/// slot, never a gap of nothing.
#[test]
fn a_link_with_no_length_takes_the_room_after_the_last_slot() {
    let mut a = Activities::IDLE;
    for _ in 0..3 {
        a = connect(&a, link(100.0, 100.0, 16)).unwrap();
    }
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    let b = connect(&a, link(100.0, 100.0, 0)).unwrap();
    let s = b.central[1].unwrap();
    assert_eq!(s.offset.0, 34_500);
    assert!(s.ce_len.as_u16() > 0);
}

/// Core Specification Vol 4 Part E §7.8.12: the interval's minimum is at most its maximum, and
/// not zero. The chip's code for either is unmeasured (F33).
#[test]
fn an_interval_range_upside_down_or_from_zero_is_refused() {
    assert!(connect(&Activities::IDLE, link(50.0, 30.0, 4)).is_err());
    assert!(connect(&Activities::IDLE, link(0.0, 30.0, 4)).is_err());
}

/// A link's interval is a whole number of 1.25 ms (Core Specification Vol 6 Part B §4.5.1), so a
/// period that is not (advertising at 161 × 0.625 ms + 5 ms is 105.625 ms) is never one.
#[test]
fn a_period_off_the_interval_grid_is_never_a_links_interval() {
    let a = Activities { advertising: Some(adv(161)), ..Activities::IDLE };
    assert_eq!(connect(&a, link(100.0, 110.0, 4)), Err(SchedKind::NewIntervalFailed));
    // §4.2.2: a multiple of it can be, with that many periods' latency.
    let b = connect(&a, link(200.0, 250.0, 4)).unwrap().central[0].unwrap();
    assert_eq!((interval_ms(b), b.latency), (211.25, 2));
    // A submultiple must be on the grid too: 1027.5 ms / 4 = 256.875 ms is not.
    let at_1022 = Activities { advertising: Some(adv(1636)), ..Activities::IDLE };
    assert_eq!(connect(&at_1022, link(250.0, 260.0, 4)), Err(SchedKind::NewIntervalFailed));
}
