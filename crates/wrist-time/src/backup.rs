//! Which `TAMP_BKPnR` holds what, clear of the registers stock firmware uses (`HARDWARE.md`).

/// The registers stock firmware reads or writes.
pub const STOCK: [u32; 23] = [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23];

/// The daily alarm (`alarm`).
pub const ALARM: u32 = 4;
/// The zone and its next two changes, in [`crate::zone::Zone::encode`]'s order.
pub const ZONE: [u32; 5] = [24, 25, 26, 27, 28];
pub const FLAGS: u32 = 29;
/// The local time the calendar was last set, seconds since 1970.
pub const LAST_SET_AT: u32 = 31;

/// Every register this image writes.
pub const OWNED: [u32; 8] = [ALARM, ZONE[0], ZONE[1], ZONE[2], ZONE[3], ZONE[4], FLAGS, LAST_SET_AT];

const FLAGS_TAG: u32 = 0x5752_0000;
const TAG_MASK: u32 = 0xFFFF_0000;
const RADIO: u32 = 1 << 0;
const SOURCE_SHIFT: u32 = 4;
const SOURCE_MASK: u32 = 0b11 << SOURCE_SHIFT;

/// The radio's last word and who last set the calendar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub radio: bool,
    /// 0 for no record, 1 the host, 2 GPS.
    pub source: u8,
}

impl Flags {
    #[must_use]
    pub fn encode(self) -> u32 {
        FLAGS_TAG | u32::from(self.radio) | ((u32::from(self.source) << SOURCE_SHIFT) & SOURCE_MASK)
    }

    /// `None` without the tag.
    #[must_use]
    pub fn decode(w: u32) -> Option<Self> {
        if w & TAG_MASK != FLAGS_TAG {
            return None;
        }
        let source = u8::try_from((w & SOURCE_MASK) >> SOURCE_SHIFT).unwrap_or(0);
        Some(Self { radio: w & RADIO != 0, source })
    }
}
