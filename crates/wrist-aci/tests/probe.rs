//! `ble probe`'s replies, as Command Complete packets, decoded and printed.

use wrist_aci::opcode::{ACI_HAL_GET_ANCHOR_PERIOD, ACI_HAL_GET_FIRMWARE_DETAILS, ACI_HAL_GET_UPDATER_VERSION};
use wrist_aci::probe::{updater_version, AnchorPeriod, Beside, FirmwareDetails, Line, Probe};
use wrist_aci::{command, parse, Event};

/// `04 0E len 01 opcode status params`.
fn command_complete(opcode: u16, status: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, 0x0E, u8::try_from(4 + params.len()).unwrap(), 0x01];
    p.extend_from_slice(&opcode.to_le_bytes());
    p.push(status);
    p.extend_from_slice(params);
    p
}

/// The line the image prints for a reply packet.
fn line(probe: Probe, packet: &[u8]) -> String {
    let Event::CommandComplete { cmd_opcode, status, return_param_bytes } = parse(packet) else {
        panic!("not a Command Complete");
    };
    assert_eq!(cmd_opcode, probe.opcode());
    Line { probe, reply: Some((status, return_param_bytes)) }.to_string()
}

const FIRMWARE: [u8; 14] = [2, 1, 0, 0x02, 0x34, 0x12, 2, 2, 1, 0, 0x0F, 0x00, 0x31, 0x09];

#[test]
fn the_probe_commands_have_no_parameters() {
    let mut out = [0u8; 8];
    for (probe, bytes) in [
        (Probe::Firmware, [0x01, 0x01, 0xFC, 0x00]),
        (Probe::Updater, [0x01, 0x22, 0xFC, 0x00]),
        (Probe::Anchor(Beside::Idle), [0x01, 0x19, 0xFC, 0x00]),
    ] {
        let n = command(probe.opcode(), &[], &mut out);
        assert_eq!(&out[..n], &bytes);
    }
}

#[test]
fn firmware_details_decode_field_by_field() {
    assert_eq!(
        FirmwareDetails::decode(&FIRMWARE),
        Some(FirmwareDetails {
            dtm_version: [2, 1, 0],
            dtm_variant: 2,
            dtm_build: 0x1234,
            stack_version: [2, 2, 1],
            stack_development: 0,
            stack_variant: 0x000F,
            stack_build: 2353,
        })
    );
    assert_eq!(FirmwareDetails::decode(&FIRMWARE[..13]), None);
}

#[test]
fn firmware_line() {
    let p = command_complete(ACI_HAL_GET_FIRMWARE_DETAILS, 0, &FIRMWARE);
    assert_eq!(
        line(Probe::Firmware, &p),
        "probe firmware dtm=2.1.0 dtm_variant=2 dtm_build=4660 stack=2.2.1 stack_development=0 stack_variant=0x000f stack_build=2353"
    );
}

#[test]
fn updater_line() {
    assert_eq!(updater_version(&[]), None);
    let p = command_complete(ACI_HAL_GET_UPDATER_VERSION, 0, &[3]);
    assert_eq!(line(Probe::Updater, &p), "probe updater version=3");
}

#[test]
fn anchor_period_decodes_little_endian() {
    let a = [0x20, 0x03, 0, 0, 0x80, 0x02, 0, 0];
    assert_eq!(AnchorPeriod::decode(&a), Some(AnchorPeriod { anchor_period: 800, max_free_slot: 640 }));
    assert_eq!(AnchorPeriod::decode(&a[..7]), None);
}

#[test]
fn anchor_lines_say_what_ran_beside_it() {
    let p = command_complete(ACI_HAL_GET_ANCHOR_PERIOD, 0, &[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(line(Probe::Anchor(Beside::Idle), &p), "probe anchor beside=idle anchor_period_us=0 max_free_slot_us=0");
    let p = command_complete(ACI_HAL_GET_ANCHOR_PERIOD, 0, &[0x20, 0x03, 0, 0, 0xE8, 0x02, 0, 0]);
    assert_eq!(
        line(Probe::Anchor(Beside::Advertising { interval: 1592 }), &p),
        "probe anchor beside=advertising adv_interval_us=995000 anchor_period_us=500000 max_free_slot_us=465000"
    );
    let p = command_complete(ACI_HAL_GET_ANCHOR_PERIOD, 0, &[0x38, 0, 0, 0, 0x10, 0, 0, 0]);
    assert_eq!(
        line(Probe::Anchor(Beside::Link { interval: 28, latency: 4 }), &p),
        "probe anchor beside=link conn_interval_us=35000 latency=4 anchor_period_us=35000 max_free_slot_us=10000"
    );
}

#[test]
fn refusals_short_replies_and_silence_are_named() {
    let p = command_complete(ACI_HAL_GET_ANCHOR_PERIOD, 0x0C, &[]);
    assert_eq!(line(Probe::Anchor(Beside::Idle), &p), "probe anchor beside=idle status=0x0c");
    let p = command_complete(ACI_HAL_GET_FIRMWARE_DETAILS, 0, &FIRMWARE[..5]);
    assert_eq!(line(Probe::Firmware, &p), "probe firmware error=short len=5");
    assert_eq!(Line { probe: Probe::Updater, reply: None }.to_string(), "probe updater error=no-reply");
}
