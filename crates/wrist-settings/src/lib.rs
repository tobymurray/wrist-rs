#![no_std]

//! The owner's saved preferences: a versioned, checked record that fits in fourteen 32-bit
//! words (the image keeps it in backup registers), with each setting's name, default and
//! value as the console writes them.
//!
//! The record is word 0, `ST`, the version and the payload's length in bytes; the payload,
//! up to 48 bytes in words 1 to 12; and word 13, a CRC-32 of the words before it. The payload
//! is a list of entries, each a key byte, a length byte and the value. A key this version does
//! not know is skipped, and a key absent from the record takes its default, so a later version
//! adds settings without moving the others. Export and import use the payload alone, as hex,
//! so a record outlives a change of where it is kept.

/// Words in a record.
pub const WORDS: usize = 14;
/// The longest payload.
pub const PAYLOAD: usize = 4 * (WORDS - 2);
/// The record's version. A record from a later version is not read.
pub const VERSION: u8 = 1;
/// The longest value [`Value::write`] writes: `AA:BB:CC:DD:EE:FF random`.
pub const VALUE_TEXT: usize = 24;

const TAG: u32 = 0x5354_0000;
const TAG_MASK: u32 = 0xFFFF_0000;

const KEY_STRAP: u8 = 1;
const KEY_RELAY: u8 = 2;
const KEY_MULTI: u8 = 3;

/// A Bluetooth device to reconnect to: the address least significant byte first, as the radio
/// takes it, and whether it is a random address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    pub random: bool,
    pub address: [u8; 6],
}

/// Everything saved. The defaults are all off, as the image starts without a record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The heart-rate strap `hr strap` reads, reconnected whenever the radio starts.
    pub strap: Option<Device>,
    /// `hr relay`: readings served as the watch's own heart rate.
    pub relay: bool,
    /// `ble multi`: advertising while a central is connected, so a second can connect.
    pub multi: bool,
}

/// Why a record was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No tag: nothing was saved since the words were last cleared.
    Absent,
    /// The tag but a bad length, check or entry.
    Corrupt,
    /// Written by a later version than this one.
    Newer(u8),
}

/// One setting, as the console names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Strap,
    Relay,
    Multi,
}

/// A setting's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Switch(bool),
    Device(Option<Device>),
}

impl Key {
    pub const ALL: [Self; 3] = [Self::Strap, Self::Relay, Self::Multi];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Strap => "hr.strap",
            Self::Relay => "hr.relay",
            Self::Multi => "ble.multi",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// What the console takes for this setting.
    #[must_use]
    pub fn usage(self) -> &'static str {
        match self {
            Self::Strap => "none | AA:BB:CC:DD:EE:FF [random]",
            Self::Relay | Self::Multi => "on | off",
        }
    }

    /// The value from the console's words, or `None` if they do not fit [`Key::usage`].
    #[must_use]
    pub fn parse(self, text: &str) -> Option<Value> {
        let mut words = text.split_whitespace();
        let first = words.next()?;
        let second = words.next();
        if words.next().is_some() {
            return None;
        }
        match self {
            Self::Strap => match (first, second) {
                ("none", None) => Some(Value::Device(None)),
                (a, kind) => parse_device(a, kind).map(|d| Value::Device(Some(d))),
            },
            Self::Relay | Self::Multi => match (first, second) {
                ("on", None) => Some(Value::Switch(true)),
                ("off", None) => Some(Value::Switch(false)),
                _ => None,
            },
        }
    }
}

fn parse_device(text: &str, kind: Option<&str>) -> Option<Device> {
    let mut address = [0u8; 6];
    let mut parts = text.split(':');
    for slot in address.iter_mut().rev() {
        let p = parts.next()?;
        if p.len() != 2 || !p.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        *slot = u8::from_str_radix(p, 16).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let random = match kind {
        None | Some("public") => false,
        Some("random") => true,
        Some(_) => return None,
    };
    Some(Device { random, address })
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

impl Value {
    /// The value as the console writes it, into `out`; returns its length.
    pub fn write(self, out: &mut [u8; VALUE_TEXT]) -> usize {
        let text: &[u8] = match self {
            Self::Switch(true) => b"on",
            Self::Switch(false) => b"off",
            Self::Device(None) => b"none",
            Self::Device(Some(d)) => {
                let mut n = 0;
                for (i, b) in d.address.iter().rev().enumerate() {
                    if i > 0 {
                        out[n] = b':';
                        n += 1;
                    }
                    out[n] = HEX[usize::from(b >> 4)];
                    out[n + 1] = HEX[usize::from(b & 0xF)];
                    n += 2;
                }
                if d.random {
                    out[n..n + 7].copy_from_slice(b" random");
                    n += 7;
                }
                return n;
            }
        };
        out[..text.len()].copy_from_slice(text);
        text.len()
    }
}

impl Settings {
    #[must_use]
    pub fn get(&self, key: Key) -> Value {
        match key {
            Key::Strap => Value::Device(self.strap),
            Key::Relay => Value::Switch(self.relay),
            Key::Multi => Value::Switch(self.multi),
        }
    }

    /// Sets `key`; `false` if `value` is not that setting's kind.
    pub fn set(&mut self, key: Key, value: Value) -> bool {
        match (key, value) {
            (Key::Strap, Value::Device(d)) => self.strap = d,
            (Key::Relay, Value::Switch(on)) => self.relay = on,
            (Key::Multi, Value::Switch(on)) => self.multi = on,
            _ => return false,
        }
        true
    }

    /// The payload: one entry for each setting away from its default. Returns its length.
    pub fn to_bytes(&self, out: &mut [u8; PAYLOAD]) -> usize {
        let mut n = 0;
        let mut put = |key: u8, value: &[u8]| {
            out[n] = key;
            out[n + 1] = u8::try_from(value.len()).unwrap_or(0);
            out[n + 2..n + 2 + value.len()].copy_from_slice(value);
            n += 2 + value.len();
        };
        if let Some(d) = self.strap {
            let mut v = [0u8; 7];
            v[0] = u8::from(d.random);
            v[1..].copy_from_slice(&d.address);
            put(KEY_STRAP, &v);
        }
        if self.relay {
            put(KEY_RELAY, &[1]);
        }
        if self.multi {
            put(KEY_MULTI, &[1]);
        }
        n
    }

    /// The settings a payload holds. Unknown keys are skipped and missing ones take their
    /// defaults; an entry that runs past the end, or a known key with the wrong length or
    /// value, refuses the whole payload.
    #[must_use]
    pub fn from_bytes(mut b: &[u8]) -> Option<Self> {
        if b.len() > PAYLOAD {
            return None;
        }
        let mut s = Self::default();
        while let [key, len, rest @ ..] = b {
            let len = usize::from(*len);
            if rest.len() < len {
                return None;
            }
            let (value, after) = rest.split_at(len);
            match (*key, value) {
                (KEY_STRAP, [kind @ (0 | 1), address @ ..]) if address.len() == 6 => {
                    let mut a = [0u8; 6];
                    a.copy_from_slice(address);
                    s.strap = Some(Device { random: *kind == 1, address: a });
                }
                (KEY_RELAY, [on @ (0 | 1)]) => s.relay = *on == 1,
                (KEY_MULTI, [on @ (0 | 1)]) => s.multi = *on == 1,
                (KEY_STRAP | KEY_RELAY | KEY_MULTI, _) => return None,
                _ => {}
            }
            b = after;
        }
        // A single byte left over is half an entry.
        b.is_empty().then_some(s)
    }

    /// The record's words.
    #[must_use]
    pub fn encode(&self) -> [u32; WORDS] {
        let mut payload = [0u8; PAYLOAD];
        let n = self.to_bytes(&mut payload);
        let mut w = [0u32; WORDS];
        w[0] = TAG | u32::from(VERSION) << 8 | u32::try_from(n).unwrap_or(0);
        for (word, chunk) in w[1..WORDS - 1].iter_mut().zip(payload.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*chunk);
        }
        w[WORDS - 1] = crc32(&w[..WORDS - 1]);
        w
    }

    /// The settings a record holds.
    ///
    /// # Errors
    /// [`Problem::Absent`] without the tag, [`Problem::Newer`] for a later version's record,
    /// and [`Problem::Corrupt`] for a bad length, check or entry.
    pub fn decode(w: &[u32; WORDS]) -> Result<Self, Problem> {
        if w[0] & TAG_MASK != TAG {
            return Err(Problem::Absent);
        }
        let version = u8::try_from(w[0] >> 8 & 0xFF).unwrap_or(0);
        if version > VERSION {
            return Err(Problem::Newer(version));
        }
        let n = usize::try_from(w[0] & 0xFF).unwrap_or(usize::MAX);
        if version == 0 || n > PAYLOAD || crc32(&w[..WORDS - 1]) != w[WORDS - 1] {
            return Err(Problem::Corrupt);
        }
        let mut payload = [0u8; PAYLOAD];
        for (chunk, word) in payload.as_chunks_mut::<4>().0.iter_mut().zip(&w[1..WORDS - 1]) {
            *chunk = word.to_le_bytes();
        }
        Self::from_bytes(&payload[..n]).ok_or(Problem::Corrupt)
    }
}

/// CRC-32 (IEEE 802.3, reflected, as zlib's) of the words' little-endian bytes.
#[must_use]
pub fn crc32(words: &[u32]) -> u32 {
    let mut crc = !0u32;
    for byte in words.iter().flat_map(|w| w.to_le_bytes()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { crc >> 1 ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Two hex digits for each payload byte into `out` (twice the payload's length); returns the
/// length written.
pub fn to_hex(payload: &[u8], out: &mut [u8; 2 * PAYLOAD]) -> usize {
    for (pair, b) in out.as_chunks_mut::<2>().0.iter_mut().zip(payload) {
        *pair = [HEX[usize::from(b >> 4)], HEX[usize::from(b & 0xF)]];
    }
    2 * payload.len().min(PAYLOAD)
}

/// The settings from [`to_hex`]'s digits, either case.
#[must_use]
pub fn from_hex(text: &str) -> Option<Settings> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) || digits.len() > 2 * PAYLOAD || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut payload = [0u8; PAYLOAD];
    for (slot, pair) in payload.iter_mut().zip(digits.as_chunks::<2>().0) {
        let s = core::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(s, 16).ok()?;
    }
    Settings::from_bytes(&payload[..digits.len() / 2])
}
