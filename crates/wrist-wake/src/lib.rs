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
        /// The watch's own central work by time, the earliest of: a scan's end, a connection
        /// attempt or GATT procedure given up, the next reconnection to a sensor.
        pub central_at: Option<u64>,
        /// Advertising slows down at this tick ([`advertise_every`]).
        pub slow_at: Option<u64>,
    }

    /// When the loop must next run by time; `None` means only the chip's IRQ, a request or
    /// a signal can give it work.
    #[must_use]
    pub fn due(s: &State) -> Option<u64> {
        let now = s.now;
        let quiet = s.transfer == Transfer::Started;
        let console = s.console.and_then(|c| (c.pending && !quiet).then_some(c.next_at.max(now)));
        earliest([s.work.then_some(now), s.confirm_by, console, s.abandon_at, s.ask_at, s.central_at, s.slow_at])
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
    /// about once a second (995 ms, chosen to fit beside a sensor link: [`sensor`]) and an idle link that
    /// wakes the radio about every 2 s, until a transfer asks for speed while it lasts. The idle
    /// link is as slow as Apple's accessory rules allow (interval x (1 + latency) at most 2 s,
    /// and the timeout above three times that and at most 6 s), so a Mac keeps it; the watch
    /// still sends at once, and only what a central sends waits for the next event it attends.
    #[must_use]
    pub const fn timing(power: Power, transfer: bool) -> Timing {
        const FAST: Timing = Timing { advertise: 160, interval_min: 12, interval_max: 24, latency: 0, timeout: 500 };
        match (power, transfer) {
            (Power::Cable, _) | (Power::Battery, true) => FAST,
            (Power::Battery, false) => Timing { advertise: 1592, interval_min: 80, interval_max: 160, latency: 8, timeout: 600 },
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

    /// How long (ms) the watch keeps advertising at [`timing`]'s interval after the radio
    /// starts, a central leaves or a button is pressed.
    pub const ADVERTISE_FAST_MS: u64 = 2 * 60 * 1000;

    /// The battery's idle advertising (0.625 ms units): 2495 ms, so that, as 995 ms does, it
    /// plus 5 ms is a whole multiple of the battery's 500 ms sensor link ([`fits`]).
    pub const ADVERTISE_IDLE: u16 = 3992;

    /// The advertising interval (0.625 ms units), `since_ms` after the radio started, a
    /// central left or a button was pressed, whichever was last.
    ///
    /// Each advertising event is three transmissions, the largest share of an idle radio's
    /// current. On battery, once nobody has connected for [`ADVERTISE_FAST_MS`], the watch
    /// advertises two and a half times less often: a phone that is away costs less, and a
    /// press brings the usual interval back for when the owner wants to connect. While the
    /// relay serves heart rate it stays at the usual interval, so a rower or a bike finds the
    /// watch quickly. On a cable, always the cable's.
    #[must_use]
    pub const fn advertise_every(power: Power, since_ms: u64, relay: bool) -> u16 {
        let usual = timing(power, false).advertise;
        match power {
            Power::Battery if !relay && since_ms >= ADVERTISE_FAST_MS => ADVERTISE_IDLE,
            _ => usual,
        }
    }

    /// A scan's duty: every `interval`, listen for `window` (both 0.625 ms units).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Scan {
        pub interval: u16,
        pub window: u16,
    }

    /// Scanning only ever runs on request and for a bounded time. On a cable it listens 60% of
    /// the time (60 ms in every 100 ms); on battery 20% (200 ms in every second).
    ///
    /// The interval follows advertising's: while the watch advertises, ST's stack granted
    /// about one scan window per advertising period (install 153, advertising every 995 ms,
    /// six seconds each: 20 ms in every 100 ms heard 3 reports, 30 ms in every 50 ms 7, 100 ms
    /// in every 500 ms 20, 200 ms in every 1000 ms 51).
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

    /// The link the watch asks of a sensor it connects to as the central (latency 0: a
    /// central attends every event). A heart-rate strap notifies about once a second, so on
    /// battery 500 ms loses nothing a person sees; on a cable, 30 to 50 ms.
    ///
    /// ST's stack schedules its own advertising around a link where the watch is the
    /// central, and (ST's programming manual, as quoted on ST's forum) wants the advertising
    /// interval plus 5 ms to be a whole multiple of that link's interval. Without it the chip
    /// refused the connection (0x84) or the advertising (0x85) at installs 151 and 152. So the
    /// two are chosen together: 995 ms + 5 ms is twice 500 ms, and on a cable 100 ms + 5 ms is
    /// three times 35 ms, inside 30 to 50 ms.
    #[must_use]
    pub const fn sensor(power: Power) -> Timing {
        match power {
            Power::Cable => Timing { advertise: 0, interval_min: 24, interval_max: 40, latency: 0, timeout: 400 },
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

    /// After a sensor is lost, how long (ms) before the next attempt to reach it: 5 s on a
    /// cable, 30 s on battery, each attempt a scan of its own.
    #[must_use]
    pub const fn reconnect_after_ms(power: Power) -> u64 {
        match power {
            Power::Cable => 5_000,
            Power::Battery => 30_000,
        }
    }

    /// How long a lost sensor is looked for before the watch gives up on it (ms).
    pub const GIVE_UP_MS: u64 = 5 * 60 * 1000;

    /// The shortest advertising interval the watch uses (0.625 ms units): the cable's.
    pub const ADVERTISE_FASTEST: u16 = timing(Power::Cable, false).advertise;

    /// The advertising intervals to try, best first (0.625 ms units). With a link of its own
    /// as the central (`sensor_interval`, 1.25 ms units), ST's stack schedules advertising
    /// around that link and refuses an interval that does not [`fits`] (install 151: 1022.5 ms
    /// beside a 420 ms link, status 0x85). So after the wanted interval come the two longest
    /// that fit and are no longer than it, then the cable's interval: the watch must stay
    /// reachable, and a faster interval costs current but never loses the phone.
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
            let k = (u32::from(want) + 8) / step;
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
