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

/// The BLE task (`apps/standalone-boot/src/ble.rs`), as far as its timing goes.
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
        /// Work an event or request left for the loop (an MTU exchange, the interval
        /// request, the security request, a finished transfer, a passkey, a rebond, a
        /// re-advertise): do it on the next pass.
        pub work: bool,
        /// A numeric comparison is answered from the console (which signals) or rejected
        /// at this tick.
        pub confirm_by: Option<u64>,
        pub transfer: Transfer,
        /// `None` with no central subscribed to the console over an encrypted link.
        pub console: Option<Console>,
        /// A transfer that has gone quiet is abandoned at this tick.
        pub abandon_at: Option<u64>,
        /// The watch's own central work by time, the earliest of: a scan's end, a connection
        /// attempt or GATT procedure given up, the next reconnection to a sensor.
        pub central_at: Option<u64>,
    }

    /// When the loop must next run by time; `None` means only the chip's IRQ, a request or
    /// a signal can give it work.
    #[must_use]
    pub fn due(s: &State) -> Option<u64> {
        let now = s.now;
        let quiet = s.transfer == Transfer::Started;
        let console = s.console.and_then(|c| (c.pending && !quiet).then_some(c.next_at.max(now)));
        earliest([s.work.then_some(now), s.confirm_by, console, s.abandon_at, s.central_at])
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
    /// wakes the radio about once a second, until a transfer asks for speed while it lasts.
    #[must_use]
    pub const fn timing(power: Power, transfer: bool) -> Timing {
        const FAST: Timing = Timing { advertise: 160, interval_min: 12, interval_max: 24, latency: 0, timeout: 500 };
        match (power, transfer) {
            (Power::Cable, _) | (Power::Battery, true) => FAST,
            (Power::Battery, false) => Timing { advertise: 1592, interval_min: 80, interval_max: 160, latency: 4, timeout: 600 },
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
