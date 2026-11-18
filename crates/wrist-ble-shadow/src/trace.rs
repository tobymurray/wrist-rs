//! The driver's snapshot as a `ble-trace` capture carries it: what the core reads of it, a byte of
//! flags (advertising, scanning, initiating) then each link's handle, role and encryption.

use wrist_ble::{AddrKind, BdAddr, ConnHandle, Instant, LeConnRole, LinkStatus, PeerAddr, Snapshot, MAX_LINKS};

/// The longest a snapshot is carried in.
pub const SNAPSHOT_MAX: usize = 1 + 4 * MAX_LINKS;

/// `s` in `out`; how many bytes it takes.
#[must_use]
pub fn encode(s: &Snapshot, out: &mut [u8; SNAPSHOT_MAX]) -> usize {
    out[0] = u8::from(s.advertising) | u8::from(s.scanning) << 1 | u8::from(s.initiating) << 2;
    let mut len = 1;
    for l in s.links.iter().flatten() {
        let [lo, hi] = l.link.raw().to_le_bytes();
        let role = u8::from(l.role == LeConnRole::Peripheral);
        let encrypted = l.encrypted.map_or(2, u8::from);
        out[len..len + 4].copy_from_slice(&[lo, hi, role, encrypted]);
        len += 4;
    }
    len
}

/// The snapshot `bytes` carry, taken at `at`, with each peer's address zero; `None` if they
/// are not one.
#[must_use]
pub fn decode(at: Instant, bytes: &[u8]) -> Option<Snapshot> {
    let (&flags, rest) = bytes.split_first()?;
    if flags > 0b111 || rest.len() % 4 != 0 || rest.len() > 4 * MAX_LINKS {
        return None;
    }
    let mut links = [None; MAX_LINKS];
    for (slot, l) in links.iter_mut().zip(rest.as_chunks::<4>().0) {
        let role = match l[2] {
            0 => LeConnRole::Central,
            1 => LeConnRole::Peripheral,
            _ => return None,
        };
        let encrypted = match l[3] {
            0 => Some(false),
            1 => Some(true),
            2 => None,
            _ => return None,
        };
        let peer = PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([0; 6]) };
        *slot = Some(LinkStatus { link: ConnHandle::new(u16::from_le_bytes([l[0], l[1]])), role, peer, encrypted });
    }
    Some(Snapshot { at, links, advertising: flags & 1 != 0, scanning: flags & 2 != 0, initiating: flags & 4 != 0, anchor: None })
}
