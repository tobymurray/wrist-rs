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
    }

    /// When the loop must next run by time; `None` means only the chip's IRQ, a request or
    /// a signal can give it work.
    #[must_use]
    pub fn due(s: &State) -> Option<u64> {
        let now = s.now;
        let quiet = s.transfer == Transfer::Started;
        let console = s.console.and_then(|c| (c.pending && !quiet).then_some(c.next_at.max(now)));
        earliest([s.work.then_some(now), s.confirm_by, console, s.abandon_at, s.ask_at])
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

    /// Console output to a subscribed central; a chunk refused for want of TX buffers goes again first.
    pub struct Outbox<const N: usize> {
        held: Option<([u8; N], usize)>,
    }

    impl<const N: usize> Default for Outbox<N> {
        fn default() -> Self {
            Self { held: None }
        }
    }

    impl<const N: usize> Outbox<N> {
        #[must_use]
        pub fn holding(&self) -> bool {
            self.held.is_some()
        }

        pub fn clear(&mut self) {
            self.held = None;
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
                0 => Ok(()),
                wrist_aci::status::INSUFFICIENT_RESOURCES => {
                    self.held = Some((chunk, n));
                    Ok(())
                }
                _ => Err(status),
            }
        }
    }
}
