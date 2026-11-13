//! What the chip is and what its link layer holds (PM0257 §4): the replies to
//! `ACI_HAL_GET_FIRMWARE_DETAILS`, `ACI_HAL_GET_UPDATER_VERSION`, `ACI_HAL_GET_ANCHOR_PERIOD` and
//! `ACI_HAL_GET_LINK_STATUS`. Layouts from ST's `bluenrg1_types.h`.

/// `ACI_HAL_GET_FIRMWARE_DETAILS`'s return parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FirmwareDetails {
    /// The network-coprocessor (DTM) application: major, minor, patch.
    pub dtm_version: [u8; 3],
    /// 1 UART, 2 SPI.
    pub dtm_variant: u8,
    pub dtm_build: u16,
    /// The BLE stack: major, minor, patch.
    pub stack_version: [u8; 3],
    /// 0 an official release, 1 an internal development one.
    pub stack_development: u8,
    /// Privacy 0x01, Secure Connections 0x02, central 0x04, data length extension 0x08, link
    /// layer only 0x10.
    pub stack_variant: u16,
    pub stack_build: u16,
}

impl FirmwareDetails {
    const LEN: usize = 14;

    /// From a Command Complete's return parameters, the bytes after its status.
    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let p = p.get(..Self::LEN)?;
        let at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
        Some(Self {
            dtm_version: [p[0], p[1], p[2]],
            dtm_variant: p[3],
            dtm_build: at(4),
            stack_version: [p[6], p[7], p[8]],
            stack_development: p[9],
            stack_variant: at(10),
            stack_build: at(12),
        })
    }
}

/// `ACI_HAL_GET_ANCHOR_PERIOD`'s return parameters, each in 0.625 ms slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct AnchorPeriod {
    pub anchor_period: u32,
    /// The longest slot a new link could still be given.
    pub max_free_slot: u32,
}

impl AnchorPeriod {
    /// From a Command Complete's return parameters, the bytes after its status.
    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let p = p.get(..8)?;
        Some(Self {
            anchor_period: u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
            max_free_slot: u32::from_le_bytes([p[4], p[5], p[6], p[7]]),
        })
    }
}

/// `ACI_HAL_GET_UPDATER_VERSION`'s return parameter.
#[must_use]
pub fn updater_version(p: &[u8]) -> Option<u8> {
    p.first().copied()
}

/// The links `ACI_HAL_GET_LINK_STATUS` reports.
pub const LINKS: usize = 8;

/// `ACI_HAL_GET_LINK_STATUS`'s return parameters: each link's state, then each link's connection
/// handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkStatus {
    pub link_status: [u8; LINKS],
    pub link_connection_handle: [u16; LINKS],
}

impl LinkStatus {
    /// From a Command Complete's return parameters, the bytes after its status.
    #[must_use]
    pub fn decode(p: &[u8]) -> Option<Self> {
        let (states, handles) = p.get(..3 * LINKS)?.split_at(LINKS);
        let mut s = Self { link_status: [0; LINKS], link_connection_handle: [0; LINKS] };
        s.link_status.copy_from_slice(states);
        for (h, b) in s.link_connection_handle.iter_mut().zip(handles.as_chunks::<2>().0) {
            *h = u16::from_le_bytes(*b);
        }
        Some(s)
    }
}
