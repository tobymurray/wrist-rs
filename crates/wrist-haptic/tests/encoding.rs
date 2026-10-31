use wrist_haptic::{
    is_drv2625, od_clamp_code, od_clamp_mv, rated_voltage_code, reg, revision, trigger_function,
    vbat_mv, work_mode, Ending, Pattern, Status, ALARM_FOR_MS, BRING_UP, MAX_ON_MS, RATED_MV,
    RTP_FULL,
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
    // OD_CLAMP's reset value (SLOS879C Table 8-1).
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
        let pulse = p.pulse();
        assert!(pulse.level <= RTP_FULL, "{p:?}: signed input, positive only");
        assert!(pulse.level > 0);
        assert!(pulse.on_ms > 0 && pulse.on_ms <= MAX_ON_MS);
        assert_eq!(p.next_pulse(0, 0), Some(pulse));
        assert_eq!(Pattern::parse(p.name()), Some(p));
    }
    assert_eq!(Pattern::Tap.total_ms(), 40);
    assert_eq!(Pattern::Tap.next_pulse(1, 40), None, "a tap is one pulse");
    assert_eq!(Pattern::parse("buzz"), None);
}

/// The pulses begun, and when the last pause ends, if each cycle runs `late_ms` long.
fn ring(late_ms: u32) -> (u32, u32) {
    let (mut played, mut ms) = (0, 0);
    while let Some(p) = Pattern::Alarm.next_pulse(played, ms) {
        played += 1;
        ms += u32::from(p.on_ms) + u32::from(p.off_ms) + late_ms;
    }
    (played, ms)
}

#[test]
fn the_alarm_repeats_its_cycle_for_the_ring() {
    assert_eq!(ALARM_FOR_MS, 60_000);
    let p = Pattern::Alarm.pulse();
    assert_eq!((p.on_ms, p.off_ms), (350, 250));
    assert_eq!(ring(0), (100, 60_000));
    assert_eq!(Pattern::Alarm.total_ms(), ALARM_FOR_MS);
    assert_eq!(Pattern::Alarm.next_pulse(6, 3_600), Some(p), "past the old six");
}

#[test]
fn the_alarm_never_runs_past_its_cap() {
    assert_eq!(Pattern::Alarm.next_pulse(99, 59_400), Some(Pattern::Alarm.pulse()));
    assert_eq!(Pattern::Alarm.next_pulse(99, 59_401), None);
    assert_eq!(Pattern::Alarm.next_pulse(0, ALARM_FOR_MS), None);
    assert_eq!(Pattern::Alarm.next_pulse(0, u32::MAX), None);
    for late_ms in [1, 5, 50, 600] {
        let (_, ends) = ring(late_ms);
        assert!(ends - late_ms <= ALARM_FOR_MS, "late {late_ms}: the last cycle ends by the cap");
    }
}

#[test]
fn a_fault_after_a_stop_is_a_stop() {
    assert_eq!(Ending::of(true, true, 0, 0), Ending::Stopped);
    assert_eq!(Ending::of(true, false, 0, 0), Ending::Stopped);
    assert_eq!(Ending::of(false, true, 0, 0), Ending::Failed);
    assert_eq!(Ending::of(false, false, 0, 0), Ending::Played);
}

#[test]
fn a_pattern_fails_only_if_no_pulse_drove() {
    assert_eq!(Ending::of(false, false, 1, 1), Ending::Failed, "a tap the bus skipped");
    assert_eq!(Ending::of(false, false, 1, 0), Ending::Played);
    assert_eq!(Ending::of(false, false, 99, 98), Ending::Played, "one buzz of an alarm");
    assert_eq!(Ending::of(true, false, 3, 3), Ending::Stopped);
}
