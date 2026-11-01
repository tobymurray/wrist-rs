//! The devices around the watch, as the fake chip meets them on air.

use wrist_ble::{AddrKind, BdAddr, BluetoothUuid, Interval, Millis, ParamRange, PeerAddr};

/// What a phone's Bluetooth stack does as this device's central.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Os {
    /// macOS: a bonded Mac encrypts within 0.16 to 0.27 s of connecting.
    Mac,
    /// A Pixel on Android 14: re-times its own link three times in the first 2 s, encrypts like
    /// `BlueZ`.
    Android,
    /// `BlueZ`: reads the features first, then encrypts 0.2 to 0.56 s after connecting
    /// (`fixtures/bluez-reconnects.txt`).
    Linux,
}

/// Whose keys survive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keys {
    Bonded,
    None,
    /// The phone forgot the watch, the chip kept its keys: the phone asks to pair.
    PhoneLost,
    /// The chip forgot the phone: its encryption fails with PIN or Key Missing and the link
    /// stays up unencrypted.
    ChipLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Phone {
    pub os: Os,
    pub addr: PeerAddr,
    pub keys: Keys,
    /// The person at the phone confirms a numeric comparison this long after it shows.
    pub confirms_after: Option<Millis>,
    /// The phone's answer to this device's parameter request ends in a failed update.
    pub update_fails: bool,
    /// With `Keys::PhoneLost`: the chip reports the lost bond before the failed pairing.
    pub bond_lost_first: bool,
    /// Subscribes to the console once encrypted.
    pub console: bool,
}

impl Phone {
    #[must_use]
    pub fn new(os: Os, n: u8) -> Self {
        Self {
            os,
            addr: PeerAddr { kind: AddrKind::RANDOM, addr: BdAddr::new([n, 0x10, 0x20, 0x30, 0x40, 0xC0]) },
            keys: Keys::Bonded,
            confirms_after: Some(Millis::secs(4)),
            update_fails: false,
            bond_lost_first: true,
            console: true,
        }
    }

    /// When the bonded phone starts and finishes encrypting, after connecting.
    pub(crate) fn encryption(self) -> ((u32, u32), (u32, u32)) {
        match self.os {
            Os::Mac => ((30, 60), (160, 270)),
            Os::Android | Os::Linux => ((45, 65), (197, 559)),
        }
    }

    /// The interval granted for a request: the longest multiple of 15 ms in range, as `BlueZ`
    /// (30 of 15–30 ms, 195 of 100–200) and a Pixel (645 of 400–650) chose.
    #[must_use]
    pub fn grant(self, min: Interval, max: Interval) -> Interval {
        let (lo, hi) = (min.as_u16(), max.as_u16());
        let best = hi / 12 * 12;
        Interval::from_u16(if best >= lo { best } else { hi })
    }
}

/// A heart-rate strap, or any sensor serving one characteristic to notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sensor {
    pub addr: PeerAddr,
    pub adv_every: Millis,
    pub service: BluetoothUuid,
    pub characteristic: BluetoothUuid,
    /// The service appears this long after the sensor starts.
    pub service_after: Millis,
    pub reading_every: Millis,
    /// Of the readings due, the share delivered.
    pub delivers_percent: u8,
    /// Asks the central for these parameters this long after connecting.
    pub asks: Option<(Millis, ParamRange)>,
    /// Connections that fail as they are set up (0x3E), per thousand.
    pub not_established_permille: u16,
    /// Refuses the central's updates, which then complete with an error.
    pub refuses_updates: bool,
    /// Hangs up (0x13) this long after each link forms.
    pub hangs_up_after: Option<Millis>,
    pub value: u8,
}

impl Sensor {
    #[must_use]
    pub fn strap(n: u8) -> Self {
        Self {
            addr: PeerAddr { kind: AddrKind::PUBLIC, addr: BdAddr::new([n, 0x51, 0x52, 0x53, 0x54, 0x55]) },
            adv_every: Millis(250),
            service: BluetoothUuid::from_u16(0x180D),
            characteristic: BluetoothUuid::from_u16(0x2A37),
            service_after: Millis(0),
            reading_every: Millis::secs(1),
            delivers_percent: 100,
            asks: None,
            not_established_permille: 0,
            refuses_updates: false,
            hangs_up_after: None,
            value: 72,
        }
    }
}
