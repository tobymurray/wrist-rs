//! The chip's values as the console words them.

use bluenrg2::{bonded_devices, AddrKind, Address, BdAddr, BootReason};
use wrist_bluenrg2::console::{address, boot_reason, parse_address};

#[test]
fn every_boot_reason_is_described_apart() {
    assert_eq!(boot_reason(BootReason::UPDATER_BY_COMMAND), "the updater, by command");
    assert_eq!(boot_reason(BootReason::new(0x0A)), "a reason unknown");
    let known: Vec<_> = (0x01..=0x09).map(|r| boot_reason(BootReason::new(r))).collect();
    for (i, d) in known.iter().enumerate() {
        assert_ne!(*d, boot_reason(BootReason::new(0x0A)), "{:#04x}", i + 1);
        assert!(!known[..i].contains(d), "{d}");
    }
}

#[test]
fn an_address_parses_into_the_chips_byte_order() {
    let public = Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) };
    let random = Address { kind: AddrKind::RANDOM, ..public };
    assert_eq!(parse_address("0A:1B:2C:3D:4E:5F"), Some(public));
    assert_eq!(parse_address(" 0a-1b-2c-3d-4e-5f "), Some(public));
    assert_eq!(parse_address("0A:1B:2C:3D:4E:5F (random)"), Some(random));
    assert_eq!(parse_address("0a-1b-2c-3d-4e-5f(random) "), Some(random));
    for bad in ["", "0A:1B:2C:3D:4E", "0A:1B:2C:3D:4E:5F:00", "0A:1B:2C:3D:4E:5", "0A:1B:2C:3D:4E:5G", "0A1B2C3D4E5F", "(random)",
        "0A:1B:2C:3D:4E:5F (public)", "0A:1B:2C:3D:4E:5F random", "0A:1B:2C:3D:4E:5F (random) (random)", "+A:1B:2C:3D:4E:5F",
        "+1:+2:+3:+4:+5:+6", "0A:1B:2C:3D:4E: F"] {
        assert_eq!(parse_address(bad), None, "{bad:?}");
    }
}

#[test]
fn an_address_is_written_with_its_type_and_reads_back() {
    let public = Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) };
    assert_eq!(address(public).to_string(), "0A:1B:2C:3D:4E:5F");
    assert_eq!(address(Address { kind: AddrKind::RANDOM, ..public }).to_string(), "0A:1B:2C:3D:4E:5F (random)");
    // A resolved identity's type prints as public or random.
    for (raw, kind) in [(2, AddrKind::PUBLIC), (3, AddrKind::RANDOM)] {
        let a = Address { kind: AddrKind::new(raw), ..public };
        assert_eq!(parse_address(&address(a).to_string()), Some(Address { kind, ..public }), "{raw}");
    }
    proptest::proptest!(|(kind in 0u8..2, bytes: [u8; 6])| {
        let a = Address { kind: AddrKind::new(kind), addr: BdAddr::new(bytes) };
        proptest::prop_assert_eq!(parse_address(&address(a).to_string()), Some(a));
    });
}

#[test]
fn a_bond_of_any_type_is_found_again_by_its_printed_form() {
    for (raw, kind) in [(0, AddrKind::PUBLIC), (1, AddrKind::RANDOM), (2, AddrKind::PUBLIC), (3, AddrKind::RANDOM)] {
        let reply = [1, raw, 0xE6, 0xD5, 0xC4, 0xB3, 0xA2, 0xC1];
        let bond = bonded_devices(&reply).next().unwrap();
        assert_eq!(bond.kind, kind);
        let printed = parse_address(&address(bond).to_string()).unwrap();
        assert!(bonded_devices(&reply).any(|bond| bond == printed), "{raw}");
    }
}
