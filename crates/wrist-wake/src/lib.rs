#![no_std]

//! When a task that waits for events next has work of its own to do by time.
//!
//! A loop sleeps until the next thing that is due or until something signals it, never for
//! a fixed polling period. The rules for what is due live here, where the host can test
//! them; the image supplies the state and the clock (any monotonic tick).

/// The earliest of some deadlines; `None` if there are none.
#[must_use]
pub fn earliest<I: IntoIterator<Item = Option<u64>>>(deadlines: I) -> Option<u64> {
    deadlines.into_iter().flatten().min()
}

/// The BLE task (`apps/standalone-boot/src/ble.rs`): its deadlines, timing and decisions.
pub mod ble {
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
        /// Work an event or request left for the loop (the interval request, a finished
        /// transfer, a passkey, a rebond, a re-advertise): do it on the next pass.
        pub work: bool,
        /// A numeric comparison is answered from the console (which signals) or rejected
        /// at this tick.
        pub confirm_by: Option<u64>,
        pub transfer: Transfer,
        /// `None` with no central subscribed to the console over an encrypted link.
        pub console: Option<Console>,
        /// A transfer that has gone quiet is abandoned at this tick.
        pub abandon_at: Option<u64>,
        /// An unencrypted central is asked to pair at this tick ([`PairAsk`]).
        pub ask_at: Option<u64>,
        /// A scan ends at this tick.
        pub scan_end: Option<u64>,
        /// The watch's own central work by time, the earliest of: a connection attempt or GATT
        /// procedure given up, the next reconnection to a sensor.
        pub central_at: Option<u64>,
    }

    /// When the loop must next run by time; `None` means only the chip's IRQ, a request or
    /// a signal can give it work.
    #[must_use]
    pub fn due(s: &State) -> Option<u64> {
        let now = s.now;
        let quiet = s.transfer == Transfer::Started;
        let console = s.console.and_then(|c| (c.pending && !quiet).then_some(c.next_at.max(now)));
        earliest([s.work.then_some(now), s.confirm_by, console, s.abandon_at, s.ask_at, s.scan_end, s.central_at])
    }

    /// How long a link stays unencrypted before the watch asks its central to pair: well past a
    /// bonded central's own encryption start (HARDWARE.md).
    pub const ASK_TO_PAIR_AFTER_MS: u64 = 5_000;

    /// Asking a central that has left its link unencrypted to pair: once a link, never at
    /// connect, and not once the link is encrypted or a pairing has begun.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct PairAsk {
        at: Option<u64>,
    }

    impl PairAsk {
        /// A central connected at `now`, on a clock of `ticks_per_second`.
        pub fn connected(&mut self, now: u64, ticks_per_second: u64) {
            self.at = Some(now + ASK_TO_PAIR_AFTER_MS * ticks_per_second / 1_000);
        }

        /// Encrypted, a pairing begun or over, or disconnected: nothing to ask on this link.
        pub fn settled(&mut self) {
            self.at = None;
        }

        #[must_use]
        pub fn due(&self) -> Option<u64> {
            self.at
        }

        /// True once, when the time has come.
        pub fn take(&mut self, now: u64) -> bool {
            let due = self.at.is_some_and(|at| now >= at);
            if due {
                self.at = None;
            }
            due
        }
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
    /// about once a second (chosen to fit beside a sensor link: [`sensor`]) and an idle link that
    /// wakes the radio about once a second, until a transfer asks for speed while it lasts.
    #[must_use]
    pub const fn timing(power: Power, transfer: bool) -> Timing {
        const FAST: Timing = Timing { advertise: 160, interval_min: 12, interval_max: 24, latency: 0, timeout: 500 };
        match (power, transfer) {
            (Power::Cable, _) | (Power::Battery, true) => FAST,
            (Power::Battery, false) => Timing { advertise: 1592, interval_min: 80, interval_max: 160, latency: 4, timeout: 600 },
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

    /// Where the radio is, as `ble` reports it.
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

        /// After a start, whether it failed: it never reached advertising.
        #[must_use]
        pub const fn start_failed(self) -> bool {
            matches!(self, Self::Starting | Self::Failed)
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
            match status {
                0 => {
                    self.cut = chunk[..n].last() != Some(&b'\n');
                    Ok(())
                }
                wrist_aci::status::INSUFFICIENT_RESOURCES => {
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

    /// How long one attempt to connect to a sensor lasts (ms): it listens as [`scan`] does,
    /// which on battery hears an advertiser less often.
    #[must_use]
    pub const fn connect_timeout_ms(power: Power) -> u64 {
        match power {
            Power::Cable => 10_000,
            Power::Battery => 20_000,
        }
    }

    /// The link the watch asks of a sensor it connects to as the central, chosen so that
    /// advertising [`fits`] beside it (HARDWARE.md).
    #[must_use]
    pub const fn sensor(power: Power) -> Timing {
        match power {
            Power::Cable => Timing { advertise: 0, interval_min: 28, interval_max: 28, latency: 0, timeout: 400 },
            Power::Battery => Timing { advertise: 0, interval_min: 400, interval_max: 400, latency: 0, timeout: 600 },
        }
    }

    /// Whether advertising every `advertise` (0.625 ms units) fits beside a central link of
    /// `interval` (1.25 ms units): `advertise + 5 ms` a whole multiple of the interval.
    #[must_use]
    pub const fn fits(advertise: u16, interval: u16) -> bool {
        // In units of 0.625 ms: 5 ms is 8, and the interval is twice its count.
        interval > 0 && (advertise as u32 + 8).is_multiple_of(2 * interval as u32)
    }

    /// How long (ms) the watch waits on one GATT procedure on a sensor link of `interval`
    /// (1.25 ms units, `None` unknown): 10 s, or 60 connection events where that is longer, as
    /// at 500 ms, where a strap's discovery took 18 s.
    #[must_use]
    pub fn sensor_procedure_ms(interval: Option<u16>) -> u64 {
        10_000u64.max(interval.map_or(0, |iv| u64::from(iv) * 5 * 60 / 4))
    }

    /// Whether a new sensor link interval (1.25 ms units, 0 unknown) means advertising must
    /// start again: it runs at `advertising` and no longer fits, or is not running.
    #[must_use]
    pub const fn readvertise_for(advertising: Option<u16>, interval: u16) -> bool {
        match advertising {
            Some(every) => interval != 0 && !fits(every, interval),
            None => true,
        }
    }

    /// After a sensor is lost, how long (ms) before the next attempt to reach it, each attempt
    /// a scan of its own.
    #[must_use]
    pub const fn reconnect_after_ms(power: Power) -> u64 {
        match power {
            Power::Cable => 5_000,
            Power::Battery => 30_000,
        }
    }

    /// How long a lost sensor may go without a reading before the watch gives up on it (ms).
    pub const GIVE_UP_MS: u64 = 5 * 60 * 1000;

    /// The shortest advertising interval the watch uses (0.625 ms units): the cable's.
    pub const ADVERTISE_FASTEST: u16 = timing(Power::Cable, false).advertise;

    /// The advertising intervals to try, best first (0.625 ms units), beside a central link of
    /// `sensor_interval` (1.25 ms units) that refuses any that does not [`fits`] (HARDWARE.md):
    /// the one wanted, the two longest that fit and are no longer (or beside a slower link the
    /// shortest that fits), then the cable's, so the watch stays reachable.
    #[must_use]
    pub fn advertising_ladder(want: u16, sensor_interval: Option<u16>) -> [Option<u16>; 4] {
        let mut out = [Some(want), None, None, None];
        let mut n = 1;
        let mut push = |v: u16, out: &mut [Option<u16>; 4]| {
            // Connectable advertising may be no faster than 20 ms (32 units).
            if v >= 32 && !out.contains(&Some(v)) && n < out.len() {
                out[n] = Some(v);
                n += 1;
            }
        };
        if let Some(iv) = sensor_interval.filter(|&iv| iv > 0) {
            let step = u32::from(iv) * 2;
            let k = ((u32::from(want) + 8) / step).max(1);
            for k in [k, k.saturating_sub(1)] {
                if k >= 1 && k * step > 8 {
                    push(u16::try_from(k * step - 8).unwrap_or(want), &mut out);
                }
            }
        }
        push(ADVERTISE_FASTEST.min(want), &mut out);
        out
    }
}
