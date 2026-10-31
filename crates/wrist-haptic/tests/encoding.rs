use wrist_haptic::{
    is_drv2625, od_clamp_code, od_clamp_mv, rated_voltage_code, reg, revision, trigger_function,
    vbat_mv, work_mode, Pattern, Status, BRING_UP, MAX_ON_MS, RATED_MV, RTP_FULL,
};

#[test]
fn the_reset_id_is_a_drv2625_and_the_drv2624_is_not() {
    // SLOS879C Table 8-1: 0x13 after reset.
    assert!(is_drv2625(0x13));
    assert_eq!(revision(0x13), 3);
    assert!(is_drv2625(0x12));
    assert!(!is_drv2625(0x03));
    assert!(!is_drv2625(0x00));
    assert!(!is_drv2625(0xFF));
}

#[test]
fn the_clamp_never_exceeds_the_rating() {
    let code = od_clamp_code(RATED_MV);
    assert_eq!(code, 125);
    assert!(od_clamp_mv(code) <= RATED_MV);
    assert!(od_clamp_mv(code + 1) > RATED_MV);
    assert_eq!(od_clamp_code(0), 0);
    assert_eq!(od_clamp_code(10_000), 255);
    // The reset value, 137, is about 2.96 V.
    assert_eq!(od_clamp_mv(137), 2957);
}

#[test]
fn rated_voltage_follows_equation_6() {
    assert_eq!(rated_voltage_code(RATED_MV), 123);
    assert_eq!(rated_voltage_code(100_000), 255);
}

#[test]
fn vbat_reads_against_5_6_v_full_scale() {
    assert_eq!(vbat_mv(0), 0);
    assert_eq!(vbat_mv(255), 5600);
    // A cell at about 3.9 V.
    assert_eq!(vbat_mv(178), 3909);
}

#[test]
fn bring_up_is_rtp_by_go_on_an_open_loop_erm() {
    let get = |r: u8| BRING_UP.iter().find(|&&(a, _)| a == r).map(|&(_, v)| v);
    let mode = get(reg::MODE).unwrap();
    assert_eq!(work_mode(mode), 0, "real-time playback");
    assert_eq!(trigger_function(mode), 0, "the GO bit works only off level trigger");
    assert_eq!(trigger_function(0x44), 1, "the reset value is level trigger");
    let control1 = get(reg::CONTROL1).unwrap();
    assert_eq!(control1 & 0x80, 0, "ERM");
    assert_eq!(control1 & 0x40, 0x40, "open loop");
    assert_eq!(control1 & 0x18, 0, "no brake that needs calibration");
    assert_eq!(get(reg::OD_CLAMP), Some(od_clamp_code(RATED_MV)));
    assert_eq!(get(reg::RATED_VOLTAGE), Some(rated_voltage_code(RATED_MV)));
    assert!(BRING_UP.iter().all(|&(r, _)| r != reg::GO), "bring-up never starts the motor");
}

#[test]
fn status_names_only_the_faults() {
    assert!(!Status(0).faulted());
    assert!(!Status(Status::PROCESS_DONE | Status::DIAG_RESULT).faulted());
    assert_eq!(Status(0).faults().count(), 0);
    let s = Status(Status::UVLO | Status::OC_DETECT | Status::PROCESS_DONE);
    assert!(s.faulted());
    assert!(s.faults().eq(["UVLO", "OC_DETECT"]));
}

#[test]
fn patterns_stay_within_the_rating_and_their_lengths() {
    for p in Pattern::ALL {
        assert_ne!(p.pulses(), []);
        for pulse in p.pulses() {
            assert!(pulse.level <= RTP_FULL, "{p:?}: signed input, positive only");
            assert!(pulse.level > 0);
            assert!(pulse.on_ms > 0 && pulse.on_ms <= MAX_ON_MS);
        }
        assert_eq!(Pattern::parse(p.name()), Some(p));
    }
    assert!(Pattern::Tap.total_ms() <= 100);
    let alarm = Pattern::Alarm.total_ms();
    assert!((2_000..=5_000).contains(&alarm), "a few seconds: {alarm} ms");
    assert!(Pattern::Alarm.pulses().len() > 1, "it repeats");
    assert_eq!(Pattern::parse("buzz"), None);
}
