//! The watch's pairing and address, and the bond list read for `ble forget` and after a pairing.

use bluenrg2::{AddrKind, Address, BdAddr};
use wrist_bluenrg2::bonds::{another_bond, bond_kept, forget, static_random_address, Forget, PAIRING};
use wrist_bluenrg2::console::{address, parse_address};

#[test]
fn pairing_is_secure_connections_only_with_mitm_and_16_byte_keys() {
    let p = PAIRING.params();
    // Bonding, MITM, SC only, no keypress, key 16 to 16, no fixed pin (the host is asked), pin 0,
    // static random identity.
    assert_eq!(p, [0x01, 0x01, 0x02, 0x00, 0x10, 0x10, 0x01, 0, 0, 0, 0, 0x01]);
}

#[test]
fn the_address_is_static_random_and_never_changes() {
    // Pinned: a different address would orphan every bond.
    let uid = [0x0034_0021, 0x3133_5111, 0x3039_3533];
    assert_eq!(static_random_address(uid), static_random_address(uid));
    assert_eq!(static_random_address(uid), [0x2B, 0x1E, 0xCE, 0x05, 0x69, 0xFF]);
    for x in 0..1_000u32 {
        let a = static_random_address([x, x.rotate_left(7) ^ 0x5A5A, !x]);
        assert_eq!(a[5] & 0xC0, 0xC0, "{a:02X?}");
    }
}

#[test]
fn neighbouring_dies_get_different_addresses() {
    let mut seen = std::collections::HashSet::new();
    for x in 0..64u32 {
        for y in 0..64u32 {
            assert!(seen.insert(static_random_address([(x << 16) | y, 0x3133_5111, 0x3039_3533])));
        }
    }
}

#[test]
fn only_another_bonds_identity_keeps_a_link_through_a_forget() {
    let reply = [2, 1, 0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6, 0, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A];
    let random = Address { kind: AddrKind::RANDOM, addr: BdAddr::new([0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6]) };
    let public = Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) };
    assert!(another_bond(&reply, random, public));
    assert!(!another_bond(&reply, public, public));
    assert!(!another_bond(&reply, random, Address { kind: AddrKind::RANDOM, ..public }));
    // A resolvable private address, as a phone connects with.
    assert!(!another_bond(&reply, random, Address { kind: AddrKind::RANDOM, addr: BdAddr::new([1, 2, 3, 4, 5, 0x4A]) }));
    assert!(!another_bond(&[], random, public));
    // Two bonds that share an address but not its type.
    let twins = [2, 1, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A, 0, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A];
    assert!(another_bond(&twins, Address { kind: AddrKind::RANDOM, ..public }, public));
}

#[test]
fn forget_takes_only_the_bond_of_the_type_given_and_names_one_of_another() {
    let random = Address { kind: AddrKind::RANDOM, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) };
    let public = Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6]) };
    let reply = [2, 1, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A, 0, 0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6];
    assert_eq!(forget(&reply, random), Forget::Bond);
    assert_eq!(forget(&reply, public), Forget::Bond);
    // Typed without its type, as `ble bonds` printed it before the type was added.
    let bare = parse_address("0A:1B:2C:3D:4E:5F").unwrap();
    let Forget::OtherType(bond) = forget(&reply, bare) else { panic!("{:?}", forget(&reply, bare)) };
    assert_eq!(bond, random);
    assert_eq!(address(bond).to_string(), "0A:1B:2C:3D:4E:5F (random)");
    assert_eq!(forget(&reply, Address { kind: AddrKind::RANDOM, ..public }), Forget::OtherType(public));
    assert_eq!(forget(&reply, Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([1, 2, 3, 4, 5, 6]) }), Forget::NotBonded);
    assert_eq!(forget(&[], random), Forget::NotBonded);
    // Two bonds that share an address but not its type: each type names its own.
    let twins = [2, 1, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A, 0, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A];
    assert_eq!(forget(&twins, random), Forget::Bond);
    assert_eq!(forget(&twins, bare), Forget::Bond);
}

#[test]
fn a_pairing_is_bonded_only_when_the_bond_list_shows_it() {
    let mac = [1, 0xEB, 0xDA, 0xC9, 0xB8, 0xA7, 0xC6];
    let phone = [0, 0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A];
    let rpa = Address { kind: AddrKind::RANDOM, addr: BdAddr::new([1, 2, 3, 4, 5, 0x4A]) };
    let one = [&[1u8][..], &phone].concat();
    let two = [&[2u8][..], &phone, &mac].concat();
    assert!(!bond_kept(&one, Some(1), rpa), "a pairing that kept no bond");
    assert!(bond_kept(&two, Some(1), rpa), "a private address bonded under its identity");
    assert!(!bond_kept(&two, Some(2), rpa), "a private address encrypting again proves nothing");
    assert!(!bond_kept(&two, None, rpa));
    assert!(bond_kept(&two, Some(2), Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) }));
    assert!(!bond_kept(&one, Some(2), rpa), "one forgotten since");
}
