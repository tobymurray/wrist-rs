#![no_std]

//! Standard Bluetooth sensor profiles as bytes, from the Bluetooth SIG's published GATT
//! specifications, without a radio. The image's `ble` module reads them from a sensor and
//! serves them again.

/// The Heart Rate Service (HRS 1.0): service 0x180D, Heart Rate Measurement 0x2A37
/// (notified), Body Sensor Location 0x2A38 (read).
pub mod heart_rate {
    pub const SERVICE: u16 = 0x180D;
    pub const MEASUREMENT: u16 = 0x2A37;
    pub const BODY_SENSOR_LOCATION: u16 = 0x2A38;
    /// The Client Characteristic Configuration descriptor, which turns notifications on.
    pub const CCCD: u16 = 0x2902;

    /// Body Sensor Location values.
    pub mod location {
        pub const OTHER: u8 = 0;
        pub const CHEST: u8 = 1;
        pub const WRIST: u8 = 2;
    }

    /// The most RR intervals a measurement carries here: a 20-byte notification (the default
    /// ATT MTU) with an 8-bit rate and no energy holds nine.
    pub const MAX_RR: usize = 9;

    /// The longest measurement [`Measurement::encode`] writes.
    pub const MAX_LEN: usize = 1 + 2 + 2 + 2 * MAX_RR;

    const FLAG_RATE_U16: u8 = 0x01;
    const FLAG_CONTACT_DETECTED: u8 = 0x02;
    const FLAG_CONTACT_SUPPORTED: u8 = 0x04;
    const FLAG_ENERGY: u8 = 0x08;
    const FLAG_RR: u8 = 0x10;

    /// Skin contact, as the sensor reports it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Contact {
        /// The sensor cannot tell.
        Unsupported,
        /// Supported, and no contact: a strap taken off, or not yet wet.
        Lost,
        Detected,
    }

    /// One Heart Rate Measurement.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Measurement {
        pub bpm: u16,
        pub contact: Contact,
        /// Energy expended since the last reset, in kilojoules.
        pub energy_kj: Option<u16>,
        /// RR intervals in 1/1024 s, oldest first; `rr_len` of them are valid.
        pub rr: [u16; MAX_RR],
        pub rr_len: usize,
    }

    impl Measurement {
        /// A rate alone, no contact report, no RR.
        #[must_use]
        pub const fn rate(bpm: u16) -> Self {
            Self { bpm, contact: Contact::Unsupported, energy_kj: None, rr: [0; MAX_RR], rr_len: 0 }
        }

        /// The RR intervals sent.
        #[must_use]
        pub fn rr(&self) -> &[u16] {
            &self.rr[..self.rr_len]
        }

        /// A notification's bytes: flags, the rate (8 or 16 bits), the energy if flagged, then
        /// RR intervals to the end. A value too short for what its flags promise is refused;
        /// RR intervals past [`MAX_RR`], or a trailing odd byte, are dropped.
        #[must_use]
        pub fn decode(b: &[u8]) -> Option<Self> {
            let (&flags, rest) = b.split_first()?;
            let (bpm, mut rest) = if flags & FLAG_RATE_U16 != 0 {
                let (v, r) = rest.split_first_chunk::<2>()?;
                (u16::from_le_bytes(*v), r)
            } else {
                let (v, r) = rest.split_first()?;
                (u16::from(*v), r)
            };
            let contact = match (flags & FLAG_CONTACT_SUPPORTED != 0, flags & FLAG_CONTACT_DETECTED != 0) {
                (false, _) => Contact::Unsupported,
                (true, false) => Contact::Lost,
                (true, true) => Contact::Detected,
            };
            let energy_kj = if flags & FLAG_ENERGY != 0 {
                let (v, r) = rest.split_first_chunk::<2>()?;
                rest = r;
                Some(u16::from_le_bytes(*v))
            } else {
                None
            };
            let mut m = Self { bpm, contact, energy_kj, rr: [0; MAX_RR], rr_len: 0 };
            if flags & FLAG_RR != 0 {
                for (slot, v) in m.rr.iter_mut().zip(rest.as_chunks::<2>().0) {
                    *slot = u16::from_le_bytes(*v);
                    m.rr_len += 1;
                }
            }
            Some(m)
        }

        /// The measurement as a notification into `out`; returns its length (0 if `out` is
        /// shorter than [`MAX_LEN`]). The rate goes in 8 bits when it fits.
        pub fn encode(&self, out: &mut [u8]) -> usize {
            if out.len() < MAX_LEN {
                return 0;
            }
            let mut flags = match self.contact {
                Contact::Unsupported => 0,
                Contact::Lost => FLAG_CONTACT_SUPPORTED,
                Contact::Detected => FLAG_CONTACT_SUPPORTED | FLAG_CONTACT_DETECTED,
            };
            let mut n = 1;
            if let Ok(small) = u8::try_from(self.bpm) {
                out[n] = small;
                n += 1;
            } else {
                flags |= FLAG_RATE_U16;
                out[n..n + 2].copy_from_slice(&self.bpm.to_le_bytes());
                n += 2;
            }
            if let Some(kj) = self.energy_kj {
                flags |= FLAG_ENERGY;
                out[n..n + 2].copy_from_slice(&kj.to_le_bytes());
                n += 2;
            }
            if self.rr_len > 0 {
                flags |= FLAG_RR;
                for v in self.rr() {
                    out[n..n + 2].copy_from_slice(&v.to_le_bytes());
                    n += 2;
                }
            }
            out[0] = flags;
            n
        }
    }

    /// An RR interval in 1/1024 s as milliseconds, rounded.
    #[must_use]
    pub const fn rr_ms(rr: u16) -> u32 {
        (rr as u32 * 1000 + 512) / 1024
    }
}
