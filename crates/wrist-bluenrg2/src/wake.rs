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
        /// A numeric comparison is answered from the console (which signals) or rejected
        /// at this tick.
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
    /// the rest of that line is dropped and the next line starts on a new one.
    pub struct Lines<const N: usize> {
        buf: [u8; N],
        len: usize,
        /// A piece of the current line is in the sink.
        partial: bool,
        /// The current line is being dropped up to its `\n`.
        dropping: bool,
        /// The sink ends in a line that was cut short: a `\r\n` goes before what follows.
        open: bool,
    }

    impl<const N: usize> Default for Lines<N> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<const N: usize> Lines<N> {
        #[must_use]
        pub const fn new() -> Self {
            Self { buf: [0; N], len: 0, partial: false, dropping: false, open: false }
        }

        /// Forgets the line in hand, as when the sink is emptied; `cut`: what the sink
        /// already passed on ends inside a line, so the next one starts on a new line.
        pub fn clear(&mut self, cut: bool) {
            *self = Self { open: cut, ..Self::new() };
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
            let lead: &[u8] = if self.open { b"\r\n" } else { b"" };
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
            self.partial = true;
            true
        }
    }

    /// Console output to a subscribed central; a chunk refused for want of TX buffers goes again first.
    pub struct Outbox<const N: usize> {
        held: Option<([u8; N], usize)>,
        /// The last chunk sent ended inside a line.
        cut: bool,
    }

    impl<const N: usize> Default for Outbox<N> {
        fn default() -> Self {
            Self { held: None, cut: false }
        }
    }

    impl<const N: usize> Outbox<N> {
        #[must_use]
        pub fn holding(&self) -> bool {
            self.held.is_some()
        }

        /// Drops the held chunk, which the central never had, as when what was queued is
        /// discarded; whether the central was left inside a line.
        pub fn discard(&mut self) -> bool {
            self.held = None;
            self.cut
        }

        /// The held chunk, else up to `most` bytes from `read`.
        pub fn next(&mut self, most: usize, read: impl FnOnce(&mut [u8]) -> usize) -> ([u8; N], usize) {
            self.held.take().unwrap_or_else(|| {
                let mut chunk = [0u8; N];
                let n = read(&mut chunk[..most.min(N)]);
                (chunk, n)
            })
        }

        /// The chip's status for a chunk from [`Outbox::next`]; `Err` with it when the chunk
        /// is lost.
        ///
        /// # Errors
        /// The status, for any refusal but `INSUFFICIENT_RESOURCES`.
        pub fn sent(&mut self, chunk: [u8; N], n: usize, status: u8) -> Result<(), u8> {
            match bluenrg2::BleStatus::new(status) {
                bluenrg2::BleStatus::SUCCESS => {
                    self.cut = chunk[..n].last() != Some(&b'\n');
                    Ok(())
                }
                bluenrg2::BleStatus::INSUFFICIENT_RESOURCES => {
                    self.held = Some((chunk, n));
                    Ok(())
                }
                _ => Err(status),
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
