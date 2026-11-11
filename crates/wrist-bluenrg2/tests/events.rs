//! The BlueNRG-2's packets as the core's events.

use wrist_ble::{
    AddrKind, BdAddr, ChipInfo, ConnHandle, CoreSpecificationVersion, Failure, FailureKind, Interval, LeConnRole, LinkParams, PairingEvent,
    PairingStep, PeerAddr, RadioEvent, ResetCause, Status, Timeout,
};
use wrist_bluenrg2::events::Translator;

const INFO: ChipInfo = ChipInfo {
    hci_version: CoreSpecificationVersion::VERSION_5_2,
    lmp_version: CoreSpecificationVersion::VERSION_5_2,
    manufacturer: 0x30,
    firmware: 2353,
};
fn link() -> ConnHandle {
    ConnHandle::new(0x0801)
}

const PEER: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

fn hci(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, code, u8::try_from(params.len()).unwrap()];
    p.extend(params);
    p
}

fn le(sub: u8, params: &[u8]) -> Vec<u8> {
    hci(0x3E, &[&[sub], params].concat())
}

fn vendor(code: u16, params: &[u8]) -> Vec<u8> {
    hci(0xFF, &[&code.to_le_bytes()[..], params].concat())
}

/// LE Connection Complete with success: handle 0x0801, the role, a random peer, 30 ms, latency 2,
/// timeout 5 s.
fn connection(role: u8) -> Vec<u8> {
    le(0x01, &[&[0x00, 0x01, 0x08, role, 0x01][..], &PEER, &[24, 0, 2, 0, 0xF4, 0x01, 0x00]].concat())
}

fn event(t: &mut Translator, packet: &[u8]) -> Option<RadioEvent> {
    t.event(packet)
}

fn one(packet: &[u8]) -> Option<RadioEvent> {
    Translator::new(INFO).event(packet)
}

#[test]
fn a_central_connecting_is_a_peripheral_link_with_its_parameters() {
    let params = LinkParams { interval: Interval::from_u16(24), latency: 2, timeout: Timeout::from_u16(500) };
    let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new(PEER) };
    let params = Some(params);
    assert_eq!(one(&connection(0x01)), Some(RadioEvent::LinkUp { link: link(), role: LeConnRole::Peripheral, peer, params }));
    assert_eq!(one(&connection(0x00)), Some(RadioEvent::LinkUp { link: link(), role: LeConnRole::Central, peer, params }));
    let mut public = connection(0x01);
    public[8] = 0x00;
    assert!(matches!(one(&public), Some(RadioEvent::LinkUp { peer: PeerAddr { kind: AddrKind::PUBLIC, .. }, .. })));
}

#[test]
fn a_connection_without_its_timing_is_still_a_link() {
    let short = le(0x01, &[&[0x00, 0x01, 0x08, 0x01, 0x01][..], &PEER].concat());
    let peer = PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new(PEER) };
    assert_eq!(one(&short), Some(RadioEvent::LinkUp { link: link(), role: LeConnRole::Peripheral, peer, params: None }));
}

#[test]
fn a_connection_that_never_formed_is_unknown() {
    // A connection that never formed is the central's to report, which this chip never is here.
    assert_eq!(one(&le(0x01, &[0x3E; 19])), Some(RadioEvent::Unknown { raw_code: 0x3E }));
}

#[test]
fn an_update_gives_the_new_parameters_or_its_refusal() {
    let params = LinkParams { interval: Interval::from_u16(12), latency: 4, timeout: Timeout::from_u16(600) };
    assert_eq!(one(&le(0x03, &[0x00, 0x01, 0x08, 12, 0, 4, 0, 0x58, 0x02])), Some(RadioEvent::Params { link: link(), result: Ok(params) }));
    let refused = Failure { kind: FailureKind::PeerRejected, native: 0x3B };
    assert_eq!(one(&le(0x03, &[0x3B, 0x01, 0x08, 0, 0, 0, 0, 0, 0])), Some(RadioEvent::Params { link: link(), result: Err(refused) }));
}

#[test]
fn a_central_refusing_the_watchs_request_fails_it_and_accepting_waits_for_the_update() {
    let refused = Failure { kind: FailureKind::PeerRejected, native: 0 };
    assert_eq!(one(&vendor(0x0800, &[0x01, 0x08, 0x01, 0x00])), Some(RadioEvent::Params { link: link(), result: Err(refused) }));
    assert_eq!(one(&vendor(0x0800, &[0x01, 0x08, 0x00, 0x00])), None);
    assert_eq!(one(&vendor(0x0800, &[0x01, 0x08, 0x01])), None);
}

#[test]
fn a_disconnection_ends_the_link_and_a_failed_one_is_unknown() {
    assert_eq!(one(&hci(0x05, &[0x00, 0x01, 0x08, 0x13])), Some(RadioEvent::LinkDown { link: link(), reason: Status::new(0x13) }));
    assert_eq!(one(&hci(0x05, &[0x0C, 0x01, 0x08, 0x13])), Some(RadioEvent::Unknown { raw_code: 0x05 }));
}

#[test]
fn encryption_is_on_only_when_it_succeeded_and_was_enabled() {
    assert_eq!(one(&hci(0x08, &[0x00, 0x01, 0x08, 0x01])), Some(RadioEvent::Encryption { link: link(), on: true }));
    assert_eq!(one(&hci(0x08, &[0x00, 0x01, 0x08, 0x00])), Some(RadioEvent::Encryption { link: link(), on: false }));
    assert_eq!(one(&hci(0x08, &[0x06, 0x01, 0x08, 0x01])), Some(RadioEvent::Encryption { link: link(), on: false }));
}

#[test]
fn a_key_refresh_says_the_link_is_encrypted_only_when_it_succeeded() {
    assert_eq!(one(&hci(0x30, &[0x00, 0x01, 0x08])), Some(RadioEvent::Encryption { link: link(), on: true }));
    assert_eq!(one(&hci(0x30, &[0x06, 0x01, 0x08])), Some(RadioEvent::Encryption { link: link(), on: false }));
}

#[test]
fn a_data_length_change_gives_the_octets_each_way() {
    let p = le(0x07, &[0x01, 0x08, 251, 0, 0x48, 0x08, 27, 0, 0x48, 0x01]);
    assert_eq!(one(&p), Some(RadioEvent::DataLength { link: link(), tx: 251, rx: 27 }));
}

#[test]
fn pairing_steps_come_from_the_gap_events() {
    let step = |step| Some(RadioEvent::Pairing(PairingEvent { link: link(), step }));
    assert_eq!(one(&vendor(0x0409, &[0x01, 0x08, 0x40, 0xE2, 0x01, 0x00])), step(PairingStep::Numeric(123_456)));
    assert_eq!(one(&vendor(0x0409, &[0x01, 0x08, 0x40])), None);
    assert_eq!(one(&vendor(0x0402, &[0x01, 0x08])), step(PairingStep::PasskeyWanted));
    assert_eq!(one(&vendor(0x0401, &[0x01, 0x08, 0x00, 0x00])), step(PairingStep::Complete));
    assert_eq!(one(&vendor(0x0401, &[0x01, 0x08, 0x02, 0x0B])), step(PairingStep::Failed));
    assert_eq!(one(&vendor(0x0401, &[0x01, 0x08])), None);
}

#[test]
fn a_lost_bond_names_the_link_a_central_holds() {
    let mut t = Translator::new(INFO);
    let lost = vendor(0x0405, &[]);
    assert_eq!(event(&mut t, &lost), None);
    event(&mut t, &connection(0x00));
    assert_eq!(event(&mut t, &lost), None, "a link the watch leads holds no phone");
    event(&mut t, &connection(0x01));
    assert_eq!(event(&mut t, &lost), Some(RadioEvent::BondLost(link())));
    event(&mut t, &hci(0x05, &[0x00, 0x02, 0x08, 0x13]));
    assert_eq!(event(&mut t, &lost), Some(RadioEvent::BondLost(link())), "another link going leaves the phone's");
    event(&mut t, &hci(0x05, &[0x00, 0x01, 0x08, 0x13]));
    assert_eq!(event(&mut t, &lost), None);
    for reason in [0x01, 0x02] {
        event(&mut t, &connection(0x01));
        event(&mut t, &vendor(0x0001, &[reason]));
        assert_eq!(event(&mut t, &lost), None, "a chip that started again (reason {reason}) holds no phone");
    }
}

#[test]
fn the_mtu_comes_from_the_exchange() {
    assert_eq!(one(&vendor(0x0C03, &[0x01, 0x08, 220, 0])), Some(RadioEvent::Mtu(link(), 220)));
    assert_eq!(one(&vendor(0x0C03, &[0x01, 0x08])), None);
}

#[test]
fn the_chip_starting_is_ready_and_any_other_reset_is_named() {
    assert_eq!(one(&vendor(0x0001, &[0x01])), Some(RadioEvent::Ready(INFO)));
    for (reason, cause) in [
        (0x05, ResetCause::Watchdog),
        (0x06, ResetCause::Lockup),
        (0x07, ResetCause::Brownout),
        (0x08, ResetCause::Crash),
        (0x02, ResetCause::Updater),
        (0x03, ResetCause::Updater),
        (0x04, ResetCause::Updater),
        (0x09, ResetCause::Other),
    ] {
        assert_eq!(one(&vendor(0x0001, &[reason])), Some(RadioEvent::ChipReset(cause)), "reason {reason}");
    }
    assert_eq!(one(&vendor(0x0001, &[])), Some(RadioEvent::ChipReset(ResetCause::Other)));
}

#[test]
fn lost_events_are_reported() {
    assert_eq!(one(&vendor(0x0002, &[0; 8])), Some(RadioEvent::EventsLost));
}

#[test]
fn replies_reports_and_events_the_core_has_no_use_for_are_none() {
    for p in [
        hci(0x0E, &[0x01, 0x83, 0xFC, 0x00]),
        hci(0x0F, &[0x00, 0x01, 0x81, 0xFD]),
        le(0x02, &[0x01, 0x00, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x00, 0xC0]),
        vendor(0x0404, &[0x01, 0x08]),
        vendor(0x0407, &[0x01, 0x00, 0x00]),
        vendor(0x0C01, &[0x01, 0x08, 0x0E, 0x00, 0x00, 0x00, 0x01, 0x00, b'x']),
        vendor(0x0C16, &[]),
        vendor(0x0C17, &[0x01, 0x08]),
    ] {
        assert_eq!(one(&p), None, "{p:02X?}");
    }
}

#[test]
fn anything_else_is_unknown_by_its_code() {
    assert_eq!(one(&vendor(0x0C0F, &[0x01, 0x08])), Some(RadioEvent::Unknown { raw_code: 0x0C0F }));
    assert_eq!(one(&le(0x14, &[0x01, 0x08, 0x01])), Some(RadioEvent::Unknown { raw_code: 0x3E14 }));
    assert_eq!(one(&hci(0x1A, &[0x01])), Some(RadioEvent::Unknown { raw_code: 0x1A }));
    assert_eq!(one(&[0x04, 0x05]), Some(RadioEvent::Unknown { raw_code: 0x05 }));
}

#[test]
fn a_handle_above_0x0eff_is_no_link() {
    for packet in [
        &[0x04, 0x05, 0x04, 0x00, 0x01, 0x0F, 0x13][..],
        &[0x04, 0x3E, 0x13, 0x01, 0x00, 0x01, 0xF8, 0x01, 0x00, 1, 2, 3, 4, 5, 6, 24, 0, 0, 0, 0xF4, 0x01, 0x00][..],
        &[0x04, 0xFF, 0x04, 0x02, 0x04, 0xFF, 0xFF][..],
    ] {
        assert!(matches!(one(packet), None | Some(RadioEvent::Unknown { .. })), "{packet:02x?}");
    }
}
