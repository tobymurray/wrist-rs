//! When a task that waits for events next has work of its own to do by time.

/// The earliest of some deadlines; `None` if there are none.
#[must_use]
pub fn earliest<I: IntoIterator<Item = Option<u64>>>(deadlines: I) -> Option<u64> {
    deadlines.into_iter().flatten().min()
}

/// The BLE task (`apps/standalone-boot/src/ble.rs`): its deadlines and timing.
pub mod ble {
    pub mod restart;
    pub mod server;

    use wrist_ble::policy::{self, chip::Chip};

    use super::earliest;
    use crate::driver::Unanswered;

    /// A firmware image arriving over the console's RX characteristic.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Transfer {
        None,
        /// `update <len> <crc>` taken, no image byte in yet.
        Claimed,
        /// Image bytes arriving: the mirror stays quiet (the chip doubled writes after
        /// refused notifications).
        Started,
    }

    /// The console mirror to a subscribed central.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Console {
        /// Output is waiting in the pipe.
        pub pending: bool,
        /// The next notification may go at this tick (they are paced).
        pub next_at: u64,
    }

    /// Everything the BLE loop does on its own time.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct State {
        pub now: u64,
        /// Work an event or request left for the loop: do it on the next pass.
        pub work: bool,
        /// A numeric comparison's digits stop at this tick, unless the console answers first
        /// (which signals).
        pub confirm_by: Option<u64>,
        pub transfer: Transfer,
        /// `None` with no central subscribed to the console over an encrypted link.
        pub console: Option<Console>,
        /// A transfer that has gone quiet is abandoned at this tick.
        pub abandon_at: Option<u64>,
        /// A scan ends at this tick.
        pub scan_end: Option<u64>,
    }

    /// When the loop must next run by time; `None` means only the chip's IRQ, a request or
    /// a signal can give it work.
    #[must_use]
    pub fn due(s: &State) -> Option<u64> {
        let now = s.now;
        let quiet = s.transfer == Transfer::Started;
        let console = s.console.and_then(|c| (c.pending && !quiet).then_some(c.next_at.max(now)));
        earliest([s.work.then_some(now), s.confirm_by, console, s.abandon_at, s.scan_end])
    }

    /// Where the watch's power comes from, as the radio sees it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Power {
        Cable,
        Battery,
    }

    /// The radio's timing: how often it advertises, and the connection it asks a central
    /// for. Units are the Bluetooth ones: advertising in 0.625 ms, the connection interval
    /// in 1.25 ms, the supervision timeout in 10 ms.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Timing {
        pub advertise: u16,
        pub interval_min: u16,
        pub interval_max: u16,
        /// Connection events the watch may skip when it has nothing to send.
        pub latency: u16,
        pub timeout: u16,
    }

    /// On a cable, as responsive as the link allows. On battery, current first: advertising
    /// about once a second (1022.5 ms, an interval phones scan well) and an idle link that
    /// wakes the radio about once a second, until a transfer asks for speed while it lasts.
    #[must_use]
    pub const fn timing(power: Power, transfer: bool) -> Timing {
        const FAST: Timing = Timing { advertise: 160, interval_min: 12, interval_max: 24, latency: 0, timeout: 500 };
        match (power, transfer) {
            (Power::Cable, _) | (Power::Battery, true) => FAST,
            (Power::Battery, false) => Timing { advertise: 1636, interval_min: 80, interval_max: 160, latency: 4, timeout: 600 },
        }
    }

    /// Whether a supervision timeout keeps to Core 6.B 4.5.2.
    #[must_use]
    pub const fn supervised(interval_max: u16, latency: u16, timeout: u16) -> bool {
        // timeout x 10 ms > (1 + latency) x interval x 1.25 ms x 2
        timeout as u32 * 4 > (1 + latency as u32) * interval_max as u32
    }

    /// `ble interval now`'s latency and supervision timeout (10 ms units).
    pub const NOW_LATENCY: u16 = 0;
    pub const NOW_TIMEOUT: u16 = 500;

    /// `ble interval now <min> <max>`: a range the central may be asked for, at
    /// [`NOW_LATENCY`] and [`NOW_TIMEOUT`].
    #[must_use]
    pub const fn interval_now(min: u16, max: u16) -> bool {
        6 <= min && min <= max && max <= 3_200 && supervised(max, NOW_LATENCY, NOW_TIMEOUT)
    }

    /// Where the radio is, as `ble` and the page report it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum Radio {
        Off,
        Starting,
        Idle,
        Advertising,
        Connected,
        Failed,
    }

    impl Radio {
        #[must_use]
        pub const fn from_u8(v: u8) -> Self {
            match v {
                0 => Self::Off,
                1 => Self::Starting,
                2 => Self::Idle,
                3 => Self::Advertising,
                4 => Self::Connected,
                _ => Self::Failed,
            }
        }

        /// The radio as the core's record has it, while it is on.
        #[must_use]
        pub fn of(state: &policy::State) -> Self {
            match state.chip.state {
                Chip::Off => Self::Off,
                Chip::Starting { .. } => Self::Starting,
                Chip::Fault { .. } => Self::Failed,
                Chip::Ready(_) if state.phone.link().is_some() => Self::Connected,
                Chip::Ready(_) if state.radio.advertising.is_some() => Self::Advertising,
                Chip::Ready(_) => Self::Idle,
            }
        }
    }

    /// Where [`Lines`] puts the mirror's output: the image's pipe.
    pub trait Sink {
        /// Bytes it has room for.
        fn free(&self) -> usize;
        /// Queues a prefix of `bytes`, which may be short of what fits (a ring buffer's
        /// end); how many.
        fn write(&mut self, bytes: &[u8]) -> usize;
    }

    /// Console output for the mirror, held until its line ends so that the line goes into
    /// the sink whole or not at all: what the central reads never runs one line into
    /// another. A line longer than `N` goes in `N`-byte pieces; when a piece does not fit,
    /// the rest of that line is dropped. The next line to go starts on a new one, after
    /// [`LOST`] if a line was lost.
    #[allow(clippy::struct_excessive_bools)]
    pub struct Lines<const N: usize> {
        buf: [u8; N],
        len: usize,
        /// A piece of the current line is in the sink.
        partial: bool,
        /// The current line is being dropped up to its `\n`.
        dropping: bool,
        /// The sink ends in a line that was cut short: a `\r\n` goes before what follows.
        open: bool,
        /// A line was lost since the last that went.
        lost: bool,
    }

    impl<const N: usize> Default for Lines<N> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<const N: usize> Lines<N> {
        #[must_use]
        pub const fn new() -> Self {
            Self { buf: [0; N], len: 0, partial: false, dropping: false, open: false, lost: false }
        }

        /// Forgets the line in hand, as when the sink is emptied.
        pub fn clear(&mut self) {
            *self = Self::new();
        }

        /// Takes console output; how many lines it lost for want of room.
        pub fn feed(&mut self, mut bytes: &[u8], sink: &mut impl Sink) -> u32 {
            let mut lost = 0;
            while !bytes.is_empty() {
                let end = bytes.iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| i + 1);
                let (mut piece, rest) = bytes.split_at(end);
                bytes = rest;
                let ends = piece.last() == Some(&b'\n');
                while !self.dropping && !piece.is_empty() {
                    if self.len == N && !self.put(sink) {
                        self.dropping = true;
                        self.lost = true;
                        lost += 1;
                        break;
                    }
                    let n = (N - self.len).min(piece.len());
                    self.buf[self.len..self.len + n].copy_from_slice(&piece[..n]);
                    self.len += n;
                    piece = &piece[n..];
                }
                // Only the line's end refused: the next line's `\r\n` lead completes it.
                let end_only = self.partial && self.buf[..self.len].iter().all(|&b| b == b'\r' || b == b'\n');
                if ends && !self.dropping && !self.put(sink) {
                    self.dropping = true;
                    self.lost |= !end_only;
                    lost += u32::from(!end_only);
                }
                if ends {
                    self.open |= self.dropping && self.partial;
                    self.dropping = false;
                    self.partial = false;
                }
            }
            lost
        }

        /// What is held goes into the sink whole, or is dropped; whether it went.
        fn put(&mut self, sink: &mut impl Sink) -> bool {
            let held = &self.buf[..self.len];
            self.len = 0;
            let lead: &[u8] = match (self.lost, self.open) {
                (true, true) => LOST,
                (true, false) => &LOST[2..],
                (false, true) => &LOST[..2],
                (false, false) => b"",
            };
            if lead.len() + held.len() > sink.free() {
                return false;
            }
            for mut part in [lead, held] {
                while !part.is_empty() {
                    match sink.write(part) {
                        0 => return false,
                        n => part = &part[n..],
                    }
                }
            }
            self.open = false;
            self.lost = false;
            self.partial = true;
            true
        }
    }

    /// What the central's console shows where output may be missing.
    pub const LOST: &[u8] = b"\r\n[ble] console: output lost here\r\n";

    /// Times a chunk refused for anything but want of TX buffers goes again before it counts
    /// as lost.
    pub const RESENDS: u8 = 3;

    #[derive(Clone, Copy)]
    enum Chunk {
        Idle,
        /// Sent and unanswered; `cleared` once what was queued is discarded meanwhile.
        InFlight { cleared: bool, refusals: u8 },
        /// `pool`: refused for want of TX buffers, and the chip has not said it has some since.
        Held { refusals: u8, pool: bool },
    }

    /// Console output to a subscribed central, a chunk at a time; one that may have gone, or
    /// that the chip kept refusing, is followed by [`LOST`].
    pub struct Outbox<const N: usize> {
        chunk: [u8; N],
        len: usize,
        state: Chunk,
        /// What the central has may end inside a line.
        cut: bool,
        /// Goes before the next output.
        owed: &'static [u8],
    }

    impl<const N: usize> Default for Outbox<N> {
        fn default() -> Self {
            Self { chunk: [0; N], len: 0, state: Chunk::Idle, cut: false, owed: b"" }
        }
    }

    impl<const N: usize> Outbox<N> {
        /// A chunk to send again, or bytes owed.
        #[must_use]
        pub fn pending(&self) -> bool {
            matches!(self.state, Chunk::Held { .. }) || !self.owed.is_empty()
        }

        /// The held chunk waits for `ACI_GATT_TX_POOL_AVAILABLE` rather than a timer.
        #[must_use]
        pub fn waits_for_pool(&self) -> bool {
            matches!(self.state, Chunk::Held { pool: true, .. })
        }

        /// The chip has TX buffers again.
        pub fn pool_available(&mut self) {
            if let Chunk::Held { refusals, .. } = self.state {
                self.state = Chunk::Held { refusals, pool: false };
            }
        }

        /// What was queued is gone, a held chunk with it, and what follows starts on a new
        /// line; a chunk in flight counts when it is answered.
        pub fn clear(&mut self) {
            if let Chunk::InFlight { refusals, .. } = self.state {
                self.state = Chunk::InFlight { cleared: true, refusals };
            } else {
                self.state = Chunk::Idle;
            }
            self.owe_line_end();
        }

        fn owe_line_end(&mut self) {
            self.owed = if self.cut { &LOST[..2] } else { b"" };
        }

        fn owe_loss(&mut self) {
            self.owed = if self.cut { LOST } else { &LOST[2..] };
        }

        /// The held chunk, else what is owed and then `read`'s bytes, `most` in all; empty when
        /// there is nothing to send.
        pub fn next(&mut self, most: usize, read: impl FnOnce(&mut [u8]) -> usize) -> &[u8] {
            let refusals = if let Chunk::Held { refusals, .. } = self.state {
                refusals
            } else {
                let most = most.min(N);
                let owed = self.owed.len().min(most);
                self.chunk[..owed].copy_from_slice(&self.owed[..owed]);
                self.owed = &self.owed[owed..];
                self.len = owed + read(&mut self.chunk[owed..most]);
                0
            };
            self.state = if self.len == 0 { Chunk::Idle } else { Chunk::InFlight { cleared: false, refusals } };
            &self.chunk[..self.len]
        }

        /// `answer`: the chip's status for the chunk from [`Outbox::next`], or why there was
        /// none; whether the chunk was lost.
        pub fn sent(&mut self, answer: Result<u8, Unanswered>) -> bool {
            let Chunk::InFlight { cleared, refusals } = self.state else {
                return false;
            };
            self.state = Chunk::Idle;
            let inside = self.chunk[..self.len].last() != Some(&b'\n');
            let status = answer.map(bluenrg2::BleStatus::new);
            let went = status == Ok(bluenrg2::BleStatus::SUCCESS);
            let unknown = answer == Err(Unanswered::NoReply);
            if cleared || unknown {
                self.cut = if went { inside } else { self.cut || (unknown && inside) };
                if cleared {
                    self.owe_line_end();
                    return false;
                }
                self.owe_loss();
                return true;
            }
            if went {
                self.cut = inside;
                return false;
            }
            let pool = status == Ok(bluenrg2::BleStatus::INSUFFICIENT_RESOURCES);
            let refusals = refusals + u8::from(!pool);
            if refusals > RESENDS {
                self.owe_loss();
                return true;
            }
            self.state = Chunk::Held { refusals, pool };
            false
        }
    }

    /// A write to the console's RX beside the one before it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Write {
        New,
        /// The same bytes within `within` of [`Writes::arrived`].
        Doubled,
        /// The same bytes later, in a write longer than a default-MTU one.
        Again,
    }

    /// The console's last write, as a CRC and when it came.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Writes {
        last: Option<(u32, u64)>,
    }

    impl Writes {
        pub fn arrived(&mut self, crc: u32, long: bool, at: u64, within: u64) -> Write {
            let last = self.last.replace((crc, at));
            match last {
                Some((c, then)) if c == crc && at.saturating_sub(then) < within => Write::Doubled,
                Some((c, _)) if c == crc && long => Write::Again,
                _ => Write::New,
            }
        }
    }

    /// A scan's duty: every `interval`, listen for `window` (both 0.625 ms units).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Scan {
        pub interval: u16,
        pub window: u16,
    }

    /// The interval follows advertising's, which paces a scan while the watch advertises (HARDWARE.md).
    #[must_use]
    pub const fn scan(power: Power) -> Scan {
        match power {
            Power::Cable => Scan { interval: 160, window: 96 },
            Power::Battery => Scan { interval: 1600, window: 320 },
        }
    }
}
