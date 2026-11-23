//! The watch's identity and pairing, and what a `GET_BONDED_DEVICES` reply says about a central.

use bluenrg2::{bonded_devices, static_random, AddrKind, Address, AuthenticationRequirement, BdAddr, FixedPin, IdentityAddress, KeySizes, SecureConnections};

/// The most bonds the chip keeps safely (HARDWARE.md, the bond store).
pub const MOST: u8 = 4;

/// The bonds a `GET_BONDED_DEVICES` reply listed, kept to compare the next reply with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listing {
    bytes: [u8; 1 + 7 * Listing::CAPACITY],
}

impl Listing {
    /// Entries kept; a reply listing more keeps its first this many.
    pub const CAPACITY: usize = 12;
    pub const EMPTY: Self = Listing { bytes: [0; 1 + 7 * Listing::CAPACITY] };

    #[must_use]
    pub fn new(params: &[u8]) -> Self {
        let mut l = Self::EMPTY;
        let entries = params.get(1..).unwrap_or(&[]);
        let count = usize::from(params.first().copied().unwrap_or(0)).min(entries.len() / 7).min(Self::CAPACITY);
        l.bytes[0] = u8::try_from(count).unwrap_or(0);
        l.bytes[1..=7 * count].copy_from_slice(&entries[..7 * count]);
        l
    }

    #[must_use]
    pub fn params(&self) -> &[u8] {
        &self.bytes[..=7 * usize::from(self.bytes[0])]
    }

    #[must_use]
    pub fn count(&self) -> u8 {
        self.bytes[0]
    }

    /// Whether a pairing with `peer` keeps the bonds at [`MOST`] or fewer: fewer are listed, or
    /// `peer` is one of them, whose pairing again replaces its bond (HARDWARE.md).
    #[must_use]
    pub fn room_for(&self, peer: Option<Address>) -> bool {
        self.count() < MOST || peer.is_some_and(|peer| listed(self.params(), peer))
    }

    /// This listing less `bond`, as the next read should list it once `bond` is forgotten.
    #[must_use]
    pub fn without(&self, bond: Address) -> Self {
        let mut l = Self::EMPTY;
        let mut n = 0;
        for (listed, entry) in bonded_devices(self.params()).zip(self.params()[1..].as_chunks::<7>().0) {
            if listed != bond {
                l.bytes[1 + 7 * n..=7 * (n + 1)].copy_from_slice(entry);
                n += 1;
            }
        }
        l.bytes[0] = u8::try_from(n).unwrap_or(0);
        l
    }

    /// The bond listed with these address bytes, of either type.
    #[must_use]
    pub fn bond_at(&self, addr: BdAddr) -> Option<Address> {
        bonded_devices(self.params()).find(|bond| bond.addr == addr)
    }

    /// The bonds listed here that `after` no longer lists.
    pub fn dropped<'a>(&'a self, after: &'a Listing) -> impl Iterator<Item = Address> + 'a {
        bonded_devices(self.params()).filter(move |&bond| !listed(after.params(), bond))
    }
}

/// `ACI_GAP_RESOLVE_PRIVATE_ADDR`: the identity address, without its type, of the bond whose key
/// resolves the address given (ST's `bluenrg1_gap_aci.h`).
pub const RESOLVE_PRIVATE_ADDR: u16 = 0xFCA0;

/// Whether `peer` is a resolvable private address: random, its two top bits `01`.
#[must_use]
pub fn resolvable(peer: Address) -> bool {
    peer.kind == AddrKind::RANDOM && peer.addr.raw()[5] >> 6 == 0b01
}

/// The watch's pairing.
pub const PAIRING: AuthenticationRequirement = AuthenticationRequirement {
    bonding: true,
    mitm: true,
    secure_connections: SecureConnections::Only,
    keypress_notifications: false,
    key_size: KeySizes::new(16, 16).unwrap(),
    fixed_pin: FixedPin::Ask,
    identity_address: IdentityAddress::StaticRandom,
};

/// The watch's static random address from the MCU's 96-bit unique id (FNV-1a).
#[must_use]
pub fn static_random_address(uid: [u32; 3]) -> [u8; 6] {
    // 64-bit FNV-1a's offset basis and prime.
    const OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01B3;
    let h = uid.iter().flat_map(|w| w.to_le_bytes()).fold(OFFSET, |h, b| (h ^ u64::from(b)).wrapping_mul(PRIME));
    static_random(h).into_inner()
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
    bonded_devices(params).find(|bond| bond.addr == wanted.addr).map_or(Forget::NotBonded, Forget::OtherType)
}

/// Whether a link from `peer` is provably not `forgotten`'s: `peer` is the identity of another bond.
#[must_use]
pub fn another_bond(params: &[u8], forgotten: Address, peer: Address) -> bool {
    peer != forgotten && listed(params, peer)
}

/// Whether a pairing with `peer` provably kept a bond: the reply read after it lists `peer` or
/// the `identity` its private address resolved to, or holds more bonds than the `before` count
/// read ahead of it.
#[must_use]
pub fn bond_kept(params: &[u8], before: Option<u8>, peer: Address, identity: Option<Address>) -> bool {
    before.zip(params.first()).is_some_and(|(before, &now)| now > before) || listed(params, peer) || identity.is_some_and(|i| listed(params, i))
}
