//! Which `TAMP_BKPnR` holds what, and the move out of the registers stock firmware uses (`HARDWARE.md`).

use crate::zone::Zone;

/// The registers stock firmware reads or writes.
pub const STOCK: [u32; 23] = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23];

/// The daily alarm (`alarm`).
pub const ALARM: u32 = 4;
/// The zone and its next two changes, in [`Zone::encode`]'s order.
pub const ZONE: [u32; 5] = [24, 25, 26, 27, 28];
pub const FLAGS: u32 = 29;
/// The local time the calendar was last set, seconds since 1970.
pub const LAST_SET_AT: u32 = 31;

/// Every register this image writes.
pub const OWNED: [u32; 8] = [ALARM, ZONE[0], ZONE[1], ZONE[2], ZONE[3], ZONE[4], FLAGS, LAST_SET_AT];

/// Where the records were before they moved, read once by [`migrate`].
pub mod old {
    pub const ZONE: [u32; 5] = [1, 11, 12, 13, 14];
    pub const SOURCE: u32 = 2;
    pub const SOURCE_TAG: u32 = 0x5352_0000;
    pub const RADIO: u32 = 16;
    pub const RADIO_ON: u32 = 0x424C_0001;
}

const FLAGS_TAG: u32 = 0x5752_0000;
const RADIO: u32 = 1 << 0;
const SOURCE_SHIFT: u32 = 4;
const SOURCE_MASK: u32 = 0b11 << SOURCE_SHIFT;

/// The radio's last word and who last set the calendar; its tag marks the move done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub radio: bool,
    /// 0 for no record, 1 the host, 2 GPS.
    pub source: u8,
}

impl Flags {
    #[must_use]
    pub fn encode(self) -> u32 {
        FLAGS_TAG | u32::from(self.radio) | (u32::from(self.source.min(2)) << SOURCE_SHIFT)
    }

    /// `None` without the tag, or with a bit no field uses.
    #[must_use]
    pub fn decode(w: u32) -> Option<Self> {
        if w & !(RADIO | SOURCE_MASK) != FLAGS_TAG {
            return None;
        }
        let source = u8::try_from((w & SOURCE_MASK) >> SOURCE_SHIFT).ok().filter(|&s| s <= 2)?;
        Some(Self { radio: w & RADIO != 0, source })
    }
}

/// The zone and flags to write, `bkp(n)` reading `BKPnR`; `None` once [`FLAGS`] holds its tag.
pub fn migrate(bkp: impl Fn(u32) -> u32) -> Option<([u32; 5], Flags)> {
    if Flags::decode(bkp(FLAGS)).is_some() {
        return None;
    }
    let zone = Zone::decode(old::ZONE.map(&bkp)).map_or([0; 5], |z| z.encode());
    let w = bkp(old::SOURCE);
    let source = if w & 0xFFFF_0000 == old::SOURCE_TAG && matches!(w & 0xFFFF, 1 | 2) {
        u8::try_from(w & 0xFF).unwrap_or(0)
    } else {
        0
    };
    Some((zone, Flags { radio: bkp(old::RADIO) == old::RADIO_ON, source }))
}
