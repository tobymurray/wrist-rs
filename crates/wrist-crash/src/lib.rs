//! The retained failure record's words (`HARDWARE.md`, "Retained failure record").

#![no_std]

const PANIC: u32 = 0xD2C0_0000;
const FAULT: u32 = 0xD2F0_0000;
const TAG: u32 = 0xFFF0_0000;
const LOW20: u32 = 0x000F_FFFF;
const CHECK_SEED: u32 = 0x5752_4331;

/// Six words in address order, the tag and its word last.
pub type Words = [u32; 6];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Panic { line: u32, file_fnv1a: u32 },
    Fault { cfsr_low20: u32, pc: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    /// A firmware hash, and whether it was `-dirty`.
    Hash { id: u64, dirty: bool },
    /// Any other build ID, by its FNV-1a.
    Named(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub build: Build,
    pub uptime_s: u32,
    /// Records since the last clear, this one included.
    pub count: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    None,
    /// A tag and its word whose other words do not check: an older image's record.
    Old(Kind),
    Record(Record),
}

#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

fn check(w: &Words) -> u32 {
    let mut h = fnv1a(&CHECK_SEED.to_le_bytes());
    for word in [w[0], w[1], w[2], w[4], w[5]] {
        for b in word.to_le_bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
    }
    h
}

impl Build {
    #[must_use]
    pub fn from_id(id: &str) -> Build {
        let (hex, rest) = id.split_at(id.len().min(12));
        let digits = hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        match (digits, rest) {
            (true, "" | "-dirty") => Build::Hash {
                id: u64::from_str_radix(hex, 16).unwrap_or(0),
                dirty: !rest.is_empty(),
            },
            _ => Build::Named(fnv1a(id.as_bytes())),
        }
    }

    /// The hash as `version` prints it, or `#` and a name's FNV-1a.
    pub fn text(self, out: &mut [u8; 18]) -> &str {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let (value, digits, start) = match self {
            Build::Hash { id, .. } => (id, 12, 0),
            Build::Named(hash) => (u64::from(hash), 8, 1),
        };
        out[0] = b'#';
        for i in 0..digits {
            out[start + i] = DIGITS[(value >> (4 * (digits - 1 - i)) & 0xF) as usize];
        }
        let mut n = start + digits;
        if let Build::Hash { dirty: true, .. } = self {
            out[n..n + 6].copy_from_slice(b"-dirty");
            n += 6;
        }
        core::str::from_utf8(&out[..n]).unwrap_or("")
    }
}

impl Record {
    #[must_use]
    pub fn encode(&self) -> Words {
        let (tag, second) = match self.kind {
            Kind::Panic { line, file_fnv1a } => (PANIC | line & LOW20, file_fnv1a),
            Kind::Fault { cfsr_low20, pc } => (FAULT | cfsr_low20 & LOW20, pc),
        };
        let (high, low, form) = match self.build {
            Build::Hash { id, dirty } => (
                u32::try_from(id >> 16 & 0xFFFF_FFFF).unwrap_or_default(),
                u32::try_from(id & 0xFFFF).unwrap_or_default(),
                u32::from(dirty),
            ),
            Build::Named(hash) => (hash, 0, 2),
        };
        let mut w = [high, low << 16 | form << 8 | u32::from(self.count), self.uptime_s, 0, tag, second];
        w[3] = check(&w);
        w
    }
}

#[must_use]
pub fn decode(w: Words) -> Decoded {
    let kind = match w[4] & TAG {
        PANIC => Kind::Panic { line: w[4] & LOW20, file_fnv1a: w[5] },
        FAULT => Kind::Fault { cfsr_low20: w[4] & LOW20, pc: w[5] },
        _ => return Decoded::None,
    };
    let count = (w[1] & 0xFF) as u8;
    let build = match w[1] >> 8 & 0xFF {
        form @ (0 | 1) => Build::Hash { id: u64::from(w[0]) << 16 | u64::from(w[1] >> 16), dirty: form == 1 },
        2 if w[1] >> 16 == 0 => Build::Named(w[0]),
        _ => return Decoded::Old(kind),
    };
    if w[3] != check(&w) || count == 0 {
        return Decoded::Old(kind);
    }
    Decoded::Record(Record { kind, build, uptime_s: w[2], count })
}

/// The count a new record carries, given what was there before it.
#[must_use]
pub fn next_count(previous: Decoded) -> u8 {
    match previous {
        Decoded::None => 1,
        Decoded::Old(_) => 2,
        Decoded::Record(r) => r.count.saturating_add(1),
    }
}
