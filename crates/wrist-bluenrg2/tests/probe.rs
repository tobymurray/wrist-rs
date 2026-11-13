//! `ble probe`'s lines from its replies, and the scheduler model against what the chip itself
//! reports of its time base.

use std::collections::HashMap;

use bluenrg2::opcode::{ACI_HAL_GET_ANCHOR_PERIOD, ACI_HAL_GET_FIRMWARE_DETAILS, ACI_HAL_GET_UPDATER_VERSION};
use bluenrg2::{command, parse, Event};
use wrist_ble::{
    Activities, AddrKind, AdvParams, Anchor, BdAddr, Change, ConnHandle, ConnParams, Duration, Failure, FailureKind, Fault, Instant, LeConnRole,
    LinkStatus, Micros, PeerAddr, Snapshot, MAX_LINKS,
};
use wrist_bluenrg2::probe::{Beside, Line, Probe, Taken};
use wrist_bluenrg2::{anchor_reading, Scheduler};

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
    let Event::CommandComplete { cmd_opcode, status: Some(status), return_param_bytes, .. } = parse(packet) else {
        panic!("not a Command Complete");
    };
    assert_eq!(cmd_opcode, probe.opcode());
    Line { probe, reply: Some((status.into_inner(), return_param_bytes)) }.to_string()
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
        let n = command(probe.opcode(), &[], &mut out).unwrap();
        assert_eq!(&out[..n], &bytes);
    }
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
    let p = command_complete(ACI_HAL_GET_UPDATER_VERSION, 0, &[3]);
    assert_eq!(line(Probe::Updater, &p), "probe updater version=3");
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

#[test]
fn the_model_reads_the_anchor_the_chip_reported() {
    let fixture = include_str!("fixtures/probe-anchor.txt");
    let mut seen = 0;
    for line in fixture.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let fields: HashMap<&str, &str> = line.split(' ').filter_map(|f| f.split_once('=')).collect();
        let us = |k: &str| fields[k].parse::<u32>().unwrap();
        let a = match fields["beside"] {
            "idle" => Activities::IDLE,
            "advertising" => {
                let units = u16::try_from(us("adv_interval_us") / 625).unwrap();
                let adv = AdvParams { interval_min: Duration::from_u16(units), interval_max: Duration::from_u16(units), connectable: true };
                Activities { advertising: Some(adv), ..Activities::IDLE }
            }
            "link" => Activities { peripheral: 1, ..Activities::IDLE },
            other => panic!("{other}"),
        };
        let model = anchor_reading(&a).map_or((0, 0), |r| (r.period.0, r.max_free_slot.0));
        assert_eq!(model, (us("anchor_period_us"), us("max_free_slot_us")), "{line}");
        seen += 1;
    }
    assert_eq!(seen, 3);
}

#[test]
fn the_free_slot_is_the_longest_a_new_link_gets() {
    let link = |ce| {
        let u = Duration::from_u16(40);
        ConnParams { interval_min: u, interval_max: u, latency: 0, timeout: Duration::from_u16(600), ce_len: Duration::from_u16(ce) }
    };
    let connect = |a: &Activities, ce| a.walk(&Scheduler, &[Change::Initiate(Some(link(ce))), Change::Connected]);
    let a = connect(&Activities::IDLE, 4).unwrap();
    let a = connect(&a, 4).unwrap().walk(&Scheduler, &[Change::Close { slot: 0 }]).unwrap();
    let r = anchor_reading(&a).unwrap();
    assert_eq!((r.period.0, r.max_free_slot.0), (50_000, 40_000));
    let slots = u16::try_from(r.max_free_slot.0 / 625).unwrap();
    assert!(connect(&a, slots).is_ok());
    assert!(connect(&a, slots + 1).is_err());
    let a = [4, 40, 4].iter().fold(Activities::IDLE, |a, &ce| connect(&a, ce).unwrap());
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    assert_eq!(anchor_reading(&a).map(|r| r.max_free_slot.0), Some(25_000));
    assert!(connect(&a, 40).is_ok());
    assert!(connect(&a, 41).is_err());
}

#[test]
fn a_snapshot_is_a_line_then_one_per_link() {
    let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([0x11, 0x22, 0x33, 0x44, 0x55, 0xC6]) };
    let link = LinkStatus { link: ConnHandle::new(0x0801), role: LeConnRole::Peripheral, peer, encrypted: Some(true) };
    let mut links = [None; MAX_LINKS];
    links[1] = Some(link);
    let anchor = Some(Anchor { period: Micros(105_000), max_free_slot: Micros(86_875) });
    let s = Snapshot { at: Instant(0), links, advertising: true, scanning: false, initiating: false, anchor };
    assert_eq!(Taken::Snapshot(&Ok(s)).to_string(), "probe snapshot advertising=1 scanning=0 initiating=0 anchor_period_us=105000 max_free_slot_us=86875");
    let idle = Snapshot { advertising: false, scanning: true, initiating: true, anchor: None, ..s };
    assert_eq!(Taken::Snapshot(&Ok(idle)).to_string(), "probe snapshot advertising=0 scanning=1 initiating=1 anchor=none");
    assert_eq!(Taken::Link(&link).to_string(), "probe snapshot link=0x0801 role=peripheral peer=c6:55:44:33:22:11 peer_type=random encrypted=1");
    let central = LinkStatus { role: LeConnRole::Central, peer: PeerAddr { kind: AddrKind::PUBLIC, ..peer }, encrypted: Some(false), ..link };
    assert_eq!(Taken::Link(&central).to_string(), "probe snapshot link=0x0801 role=central peer=c6:55:44:33:22:11 peer_type=public encrypted=0");
    let unknown = LinkStatus { encrypted: None, ..link };
    assert!(Taken::Link(&unknown).to_string().ends_with(" encrypted=unknown"));
    let refused = Fault::Refused(Failure { kind: FailureKind::Unknown, native: 0x12 });
    for (fault, line) in [(Fault::Silent, "error=no-reply"), (Fault::Bus, "error=not-taken"), (refused, "status=0x12")] {
        assert_eq!(Taken::Snapshot(&Err(fault)).to_string(), format!("probe snapshot {line}"));
    }
}
