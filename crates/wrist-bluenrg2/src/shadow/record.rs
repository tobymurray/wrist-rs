//! The flight recorder: every input the shadow takes, as `ble-trace` writes it, kept in a RAM
//! ring on every image and read back as `[trace]` lines the replay reads.

use core::fmt::{self, Write};

use wrist_ble::policy::goals::{Goals, PhoneGoal, Power, ScanGoal};
use wrist_ble::Millis;

/// The most of a record's bytes kept; the rest is only counted.
pub const KEPT: usize = 40;

/// The bytes kept of a console notification: its command's header, handle and offset. The core
/// reads no more of it, and the console's own traffic would otherwise fill the ring.
pub const NOTIFY_KEPT: usize = 8;

/// A record's header: its length, its time in ms, its kind, and its bytes before cutting.
const HEADER: usize = 1 + 4 + 1 + 2;

/// A command to the chip, an event from it, its reset, whether the core drives, a lost read, a
/// snapshot, as `ble-trace` names them.
pub const COMMAND: u8 = b'c';
pub const EVENT: u8 = b'e';
pub const RESET: u8 = b'r';
pub const MODE: u8 = b'm';
pub const LOST: u8 = b'l';
pub const SNAPSHOT: u8 = b's';
/// The core's goals ([`encode_goals`]), its timing (power, transfer), the radio on or off, the
/// comparison's answer, `ble interval now` (min, max), and `ble disconnect`.
pub const GOALS: u8 = b'G';
pub const TIMING: u8 = b'g';
pub const RADIO: u8 = b'o';
pub const ANSWER: u8 = b'a';
pub const INTERVAL_NOW: u8 = b'i';
pub const DISCONNECT: u8 = b'd';
/// Records the ring refused while a dump read it, and how many: the core's inputs are not
/// whole across it.
pub const GAP: u8 = b'x';

const KINDS: [u8; 13] = [COMMAND, EVENT, RESET, MODE, LOST, SNAPSHOT, GOALS, TIMING, RADIO, ANSWER, INTERVAL_NOW, DISCONNECT, GAP];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub ms: u32,
    pub kind: u8,
    /// The bytes' length before they were cut to [`KEPT`].
    pub total: u16,
    bytes: [u8; KEPT],
    kept: u8,
}

impl Record {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.kept)]
    }

    /// `[trace] <ms> <kind> <hex>[ +<bytes not kept>]`, without the line's end.
    ///
    /// # Errors
    ///
    /// Only those of `w`.
    pub fn write<W: Write>(&self, w: &mut W) -> fmt::Result {
        write!(w, "[trace] {} {}", self.ms, char::from(self.kind))?;
        if !self.bytes().is_empty() {
            w.write_char(' ')?;
        }
        for b in self.bytes() {
            write!(w, "{b:02X}")?;
        }
        let rest = usize::from(self.total) - self.bytes().len();
        if rest > 0 {
            write!(w, " +{rest}")?;
        }
        Ok(())
    }
}

/// What [`Ring::next`] finds at a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Record(Record, u64),
    /// The position was overwritten; the oldest record kept starts here.
    Lost(u64),
    End,
}

/// The newest records that fit in `N` bytes, each at its byte position from boot.
pub struct Ring<const N: usize> {
    buf: [u8; N],
    start: u64,
    end: u64,
    /// Held while a reader goes through it: a record that would push out an older one is not
    /// kept, and counted.
    held: bool,
    refused: u32,
}

impl<const N: usize> Ring<N> {
    #[allow(clippy::large_stack_arrays)]
    pub const NEW: Self = Self { buf: [0; N], start: 0, end: 0, held: false, refused: 0 };

    /// Hold the records kept, for a reader going through them, or let them go again; how many
    /// records were not kept while held.
    pub fn hold(&mut self, held: bool) -> u32 {
        self.held = held;
        core::mem::take(&mut self.refused)
    }

    /// Where the next record will be written.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// One record at `ms`, dropping the oldest to make room.
    pub fn push(&mut self, ms: u64, kind: u8, bytes: &[u8]) {
        self.push_kept(ms, kind, bytes, KEPT);
    }

    /// One record of which only the first `keep` bytes are kept.
    #[allow(clippy::cast_possible_truncation)]
    pub fn push_kept(&mut self, ms: u64, kind: u8, bytes: &[u8], keep: usize) {
        let kept = bytes.len().min(keep).min(KEPT);
        let len = HEADER + kept;
        if self.held && self.end + len as u64 - self.start > N as u64 {
            self.refused += 1;
            return;
        }
        // Bounded by the ring's size, so a corrupt length cannot hold the task.
        for _ in 0..N {
            if self.end + len as u64 - self.start <= N as u64 {
                break;
            }
            self.start += u64::from(self.byte(self.start)).max(1);
        }
        let total = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        let mut header = [0; HEADER];
        header[0] = len as u8;
        header[1..5].copy_from_slice(&(ms as u32).to_le_bytes());
        header[5] = kind;
        header[6..8].copy_from_slice(&total.to_le_bytes());
        for &b in header.iter().chain(&bytes[..kept]) {
            self.buf[(self.end % N as u64) as usize] = b;
            self.end += 1;
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn byte(&self, at: u64) -> u8 {
        self.buf[(at % N as u64) as usize]
    }

    /// The first record's position at or after `at`, whatever `at` is: a reader's own position
    /// may fall inside a record, whose bytes can read as one.
    #[must_use]
    pub fn align(&self, at: u64) -> u64 {
        let mut p = self.start;
        for _ in 0..N {
            if p >= at || p >= self.end {
                break;
            }
            p += u64::from(self.byte(p)).max(1);
        }
        p.min(self.end)
    }

    /// The record at `at`, a position [`Self::align`], [`Self::end`] or an earlier
    /// [`Next::Record`] gave.
    #[must_use]
    pub fn next(&self, at: u64) -> Next {
        if at < self.start {
            return Next::Lost(self.start);
        }
        if at >= self.end {
            return Next::End;
        }
        let len = usize::from(self.byte(at));
        let kind = self.byte(at + 5);
        if !(HEADER..=HEADER + KEPT).contains(&len) || !KINDS.contains(&kind) || at + len as u64 > self.end {
            return Next::End;
        }
        let mut header = [0; HEADER];
        for (i, b) in header.iter_mut().enumerate() {
            *b = self.byte(at + i as u64);
        }
        let kept = len - HEADER;
        let mut bytes = [0; KEPT];
        for (i, b) in bytes.iter_mut().take(kept).enumerate() {
            *b = self.byte(at + (HEADER + i) as u64);
        }
        let record = Record {
            ms: u32::from_le_bytes([header[1], header[2], header[3], header[4]]),
            kind,
            total: u16::from_le_bytes([header[6], header[7]]),
            bytes,
            kept: u8::try_from(kept).unwrap_or(0),
        };
        Next::Record(record, at + len as u64)
    }
}

/// The goals the app gives the core, which hold no peripherals: radio, discoverable, pairing
/// open, transfer, battery and workout as bits, then a scan's length in ms if one is wanted.
#[must_use]
pub fn encode_goals(g: &Goals, out: &mut [u8; 5]) -> usize {
    let bits = [g.radio_on, g.phone.discoverable, g.phone.pairing_open, g.transfer, g.power == Power::Battery, g.workout, g.scan.is_some()];
    out[0] = bits.iter().enumerate().map(|(i, &b)| u8::from(b) << i).sum();
    match g.scan {
        Some(s) => {
            out[1..5].copy_from_slice(&s.duration.0.to_le_bytes());
            5
        }
        None => 1,
    }
}

/// The goals [`encode_goals`] wrote; `None` if `bytes` are not some.
#[must_use]
pub fn decode_goals(bytes: &[u8]) -> Option<Goals> {
    let (&bits, rest) = bytes.split_first()?;
    let bit = |i: u8| bits & (1 << i) != 0;
    let scan = match (bit(6), rest) {
        (false, []) => None,
        (true, &[a, b, c, d]) => Some(ScanGoal { duration: Millis(u32::from_le_bytes([a, b, c, d])) }),
        _ => return None,
    };
    if bits >> 7 != 0 {
        return None;
    }
    Some(Goals {
        radio_on: bit(0),
        phone: PhoneGoal { discoverable: bit(1), pairing_open: bit(2) },
        transfer: bit(3),
        power: if bit(4) { Power::Battery } else { Power::Cable },
        workout: bit(5),
        scan,
        ..Goals::OFF
    })
}

/// How many bytes of a record of `kind` to keep: a console notification's header only.
#[must_use]
pub fn kept(kind: u8, bytes: &[u8]) -> usize {
    let notify = kind == COMMAND && bytes.get(1..3) == Some(&bluenrg2::opcode::ACI_GATT_UPDATE_CHAR_VALUE.to_le_bytes()[..]);
    if notify {
        NOTIFY_KEPT
    } else {
        KEPT
    }
}
