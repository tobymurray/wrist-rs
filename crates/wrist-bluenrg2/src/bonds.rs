//! The watch's identity and pairing, and what a `GET_BONDED_DEVICES` reply says about a central.

use bluenrg2::{bonded_devices, static_random, Address, AuthenticationRequirement};

/// The watch's pairing.
pub const PAIRING: AuthenticationRequirement = AuthenticationRequirement {
    bonding: true,
    mitm: true,
    secure_connections: 2,
    keypress_notifications: false,
    min_key_size: 16,
    max_key_size: 16,
    fixed_pin: None,
    identity_address_type: bluenrg2::address::RANDOM,
};

/// The watch's static random address from the MCU's 96-bit unique id (FNV-1a).
#[must_use]
pub const fn static_random_address(uid: [u32; 3]) -> [u8; 6] {
    // 64-bit FNV-1a's offset basis and prime.
    const OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01B3;
    let mut h = OFFSET;
    let mut w = 0;
    while w < 3 {
        let bytes = uid[w].to_le_bytes();
        let mut i = 0;
        while i < 4 {
            h ^= bytes[i] as u64;
            h = h.wrapping_mul(PRIME);
            i += 1;
        }
        w += 1;
    }
    static_random(h)
}

/// Whether `peer` is a bond's identity in a `GET_BONDED_DEVICES` reply. A private address proves nothing.
#[must_use]
pub fn listed(params: &[u8], peer: Address) -> bool {
    bonded_devices(params).any(|bond| bond == peer)
}

/// The bond `ble forget <address>` names in a `GET_BONDED_DEVICES` reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forget {
    /// A bond with this address and type, the only one to remove.
    Bond,
    /// No bond of the type given, but this one has the same bytes and is left alone.
    OtherType(Address),
    NotBonded,
}

/// Which bond, if any, `wanted` names; a bond of another type is reported, never chosen.
#[must_use]
pub fn forget(params: &[u8], wanted: Address) -> Forget {
    if listed(params, wanted) {
        return Forget::Bond;
    }
    bonded_devices(params).find(|bond| bond.bytes == wanted.bytes).map_or(Forget::NotBonded, Forget::OtherType)
}

/// Whether a link from `peer` is provably not `forgotten`'s: `peer` is the identity of another bond.
#[must_use]
pub fn another_bond(params: &[u8], forgotten: Address, peer: Address) -> bool {
    peer != forgotten && listed(params, peer)
}

/// Whether a pairing with `peer` provably kept a bond: the reply read after it lists `peer`, or
/// holds more bonds than the `before` count read ahead of it.
#[must_use]
pub fn bond_kept(params: &[u8], before: Option<u8>, peer: Address) -> bool {
    before.zip(params.first()).is_some_and(|(before, &now)| now > before) || listed(params, peer)
}
