//! The watch's pairing and address, and the bond list read for `ble forget` and after a pairing.

use bluenrg2::{AddrKind, Address, BdAddr};
use wrist_bluenrg2::bonds::{another_bond, bond_kept, forget, resolvable, static_random_address, Forget, Listing, MOST, PAIRING};
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
    assert!(!bond_kept(&one, Some(1), rpa, None), "a pairing that kept no bond");
    assert!(bond_kept(&two, Some(1), rpa, None), "a private address bonded under its identity");
    assert!(!bond_kept(&two, Some(2), rpa, None), "a private address encrypting again proves nothing");
    assert!(!bond_kept(&two, None, rpa, None));
    assert!(bond_kept(&two, Some(2), Address { kind: AddrKind::PUBLIC, addr: BdAddr::new([0x5F, 0x4E, 0x3D, 0x2C, 0x1B, 0x0A]) }, None));
    assert!(!bond_kept(&one, Some(2), rpa, None), "one forgotten since");
}

/// A `GET_BONDED_DEVICES` reply of test centrals `C0:B0:4D:00:00:nn` (random), `nn` each of `ids`.
fn reply(ids: &[u8]) -> Vec<u8> {
    let mut r = vec![u8::try_from(ids.len()).unwrap()];
    for &id in ids {
        r.extend([1, id, 0, 0, 0x4D, 0xB0, 0xC0]);
    }
    r
}

fn central(id: u8) -> Address {
    Address { kind: AddrKind::RANDOM, addr: BdAddr::new([id, 0, 0, 0x4D, 0xB0, 0xC0]) }
}

#[test]
fn a_pairing_that_would_make_a_fifth_bond_is_refused() {
    assert_eq!(MOST, 4);
    for n in 0..MOST {
        let ids: Vec<u8> = (1..=n).collect();
        assert!(Listing::new(&reply(&ids)).room_for(Some(central(9))), "{n} bonds");
        assert!(Listing::new(&reply(&ids)).room_for(None), "{n} bonds, the peer unknown");
    }
    let four = Listing::new(&reply(&[1, 2, 3, 4]));
    assert!(!four.room_for(Some(central(5))));
    assert!(!four.room_for(None));
    // One of the four pairing again replaces its own bond.
    assert!(four.room_for(Some(central(3))));
    assert!(!four.room_for(Some(Address { kind: AddrKind::PUBLIC, ..central(3) })), "the same bytes, another type");
    let six = Listing::new(&reply(&[1, 2, 3, 4, 5, 6]));
    assert!(!six.room_for(Some(central(7))));
    assert!(six.room_for(Some(central(6))));
}

#[test]
fn a_listing_names_the_bonds_the_next_one_dropped() {
    let before = Listing::new(&reply(&[1, 2, 3, 4, 5, 6]));
    let after = Listing::new(&reply(&[3, 4, 5, 6]));
    assert_eq!(before.dropped(&after).collect::<Vec<_>>(), [central(1), central(2)]);
    assert_eq!(after.dropped(&before).count(), 0, "a bond added drops nothing");
    assert_eq!(before.dropped(&before).count(), 0);
    assert_eq!(before.dropped(&Listing::EMPTY).count(), 6);
    assert_eq!(Listing::EMPTY.dropped(&before).count(), 0);
}

#[test]
fn a_listing_keeps_what_the_reply_holds_and_no_more() {
    let ids: Vec<u8> = (1..=14).collect();
    let long = Listing::new(&reply(&ids));
    assert_eq!(usize::from(long.count()), Listing::CAPACITY);
    assert_eq!(long.params(), &reply(&ids[..Listing::CAPACITY])[..]);
    // A count past the entries the reply carries.
    let mut short = reply(&[1, 2]);
    short[0] = 5;
    assert_eq!(Listing::new(&short).count(), 2);
    assert_eq!(Listing::new(&short).params(), &reply(&[1, 2])[..]);
    assert_eq!(Listing::new(&[]), Listing::EMPTY);
    assert_eq!(Listing::new(&[3, 1, 2]).count(), 0);
    assert_eq!(Listing::EMPTY.params(), [0]);
}

#[test]
fn a_private_address_is_resolved_and_matched_by_its_bytes() {
    let rpa = |top: u8| Address { kind: AddrKind::RANDOM, addr: BdAddr::new([1, 2, 3, 4, 5, top]) };
    assert!(resolvable(rpa(0x4A)));
    assert!(resolvable(rpa(0x7F)));
    assert!(!resolvable(rpa(0xC0)), "static random");
    assert!(!resolvable(rpa(0x3F)), "non-resolvable private");
    assert!(!resolvable(Address { kind: AddrKind::PUBLIC, ..rpa(0x4A) }));
    let four = Listing::new(&reply(&[1, 2, 3, 4]));
    assert_eq!(four.bond_at(central(3).addr), Some(central(3)));
    assert_eq!(four.bond_at(central(5).addr), None);
}

#[test]
fn a_re_pair_on_a_private_address_is_bonded_by_its_resolved_identity() {
    let rpa = Address { kind: AddrKind::RANDOM, addr: BdAddr::new([1, 2, 3, 4, 5, 0x4A]) };
    let four = reply(&[1, 2, 3, 4]);
    assert!(!bond_kept(&four, Some(4), rpa, None), "count unchanged, the private address not listed");
    assert!(bond_kept(&four, Some(4), rpa, Some(central(2))));
    assert!(!bond_kept(&four, Some(4), rpa, Some(central(5))), "an identity no longer listed");
}

#[test]
fn a_listing_less_a_forgotten_bond_is_the_next_read() {
    let four = Listing::new(&reply(&[1, 2, 3, 4]));
    assert_eq!(four.without(central(2)), Listing::new(&reply(&[1, 3, 4])));
    assert_eq!(four.without(central(9)), four);
    assert_eq!(four.without(Address { kind: AddrKind::PUBLIC, ..central(2) }), four, "the same bytes, another type");
    assert_eq!(four.without(central(2)).dropped(&Listing::new(&reply(&[1, 3, 4]))).count(), 0);
    assert_eq!(Listing::EMPTY.without(central(1)), Listing::EMPTY);
}
