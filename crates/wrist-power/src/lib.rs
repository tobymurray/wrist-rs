#![no_std]

//! Whether the watch is on external power, and what a stretch on battery cost.

/// From the PCA9420's `CHG_STATUS_1` and the USB-PD attach bit: an attachment
/// wins, and `None` means neither answered.
#[must_use]
pub fn external_power(pd_attached: Option<bool>, charger_status: Option<u8>) -> Option<bool> {
    if pd_attached == Some(true) { return Some(true); }
    charger_status.map(|v| v >> 6 != 0).or(pd_attached)
}

/// Reads in a row, and the time they span, before an unanswered input counts as battery for Stop.
pub const UNANSWERED_READS: u32 = 5;
pub const UNANSWERED_MS: u64 = 5 * 60_000;

/// The input as Stop sees it: unanswered reads count as battery once they have run on long
/// enough with no USB host on the link, until a read answers or the input line signals a change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopInput {
    misses: u32,
    first_miss_ms: u64,
    assumed: bool,
}

impl StopInput {
    #[must_use]
    pub const fn new() -> Self {
        Self { misses: 0, first_miss_ms: 0, assumed: false }
    }

    /// `read` as [`external_power`] gives it; `changed` when the input line fell since the last read.
    pub fn sample(&mut self, now_ms: u64, read: Option<bool>, changed: bool, host: bool) -> Option<bool> {
        if read.is_some() || changed {
            self.misses = 0;
            self.assumed = false;
        }
        if read.is_some() {
            return read;
        }
        if self.misses == 0 {
            self.first_miss_ms = now_ms;
        }
        self.misses = self.misses.saturating_add(1);
        self.assumed = !host
            && self.misses >= UNANSWERED_READS
            && now_ms.saturating_sub(self.first_miss_ms) >= UNANSWERED_MS;
        self.assumed.then_some(false)
    }

    /// Unanswered reads in a row.
    #[must_use]
    pub const fn misses(&self) -> u32 {
        self.misses
    }

    /// Whether the last read left Stop treating the input as battery.
    #[must_use]
    pub const fn assumed_battery(&self) -> bool {
        self.assumed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryRun {
    start_ms: u64,
    last_ms: u64,
    sum_ua: i64,
    samples: u32,
    /// At the first and the last readings that had one.
    pub first_soc_x10: Option<u16>,
    pub last_soc_x10: Option<u16>,
    /// `None` when the last reading had none.
    pub last_tte_s: Option<u32>,
    /// Readings that knew whether the gauge was in hibernate, and those that found it there.
    pub hib_known: u32,
    pub hibernating: u32,
}

impl BatteryRun {
    #[must_use]
    pub const fn new(now_ms: u64) -> Self {
        Self {
            start_ms: now_ms,
            last_ms: now_ms,
            sum_ua: 0,
            samples: 0,
            first_soc_x10: None,
            last_soc_x10: None,
            last_tte_s: None,
            hib_known: 0,
            hibernating: 0,
        }
    }

    /// A `None` is left out, not counted as zero.
    pub fn reading(&mut self, now_ms: u64, avg_ua: Option<i32>, soc_x10: Option<u16>, tte_s: Option<u32>, hib: Option<bool>) {
        self.last_ms = self.last_ms.max(now_ms);
        if let Some(h) = hib {
            self.hib_known += 1;
            self.hibernating += u32::from(h);
        }
        if let Some(ua) = avg_ua {
            self.sum_ua += i64::from(ua);
            self.samples += 1;
        }
        if soc_x10.is_some() {
            self.first_soc_x10 = self.first_soc_x10.or(soc_x10);
            self.last_soc_x10 = soc_x10;
        }
        self.last_tte_s = tte_s;
    }

    /// `None` before any reading with a current.
    #[must_use]
    pub fn mean_ua(&self) -> Option<i32> {
        (self.samples > 0).then(|| i32::try_from(self.sum_ua / i64::from(self.samples)).unwrap_or(i32::MIN))
    }

    /// Readings that had a current.
    #[must_use]
    pub const fn samples(&self) -> u32 {
        self.samples
    }

    /// From the start to the last reading.
    #[must_use]
    pub const fn seconds(&self) -> u64 {
        (self.last_ms - self.start_ms) / 1000
    }
}

/// Whether the MAX17262 is in hibernate, from its `Status2` (`Hib`, bit 1; ADI UG6597).
#[must_use]
pub const fn hibernating(status2: u16) -> bool {
    status2 & (1 << 1) != 0
}

/// The MAX17262's `TTE` register in seconds, or `None` for its `0xFFFF`.
#[must_use]
pub fn tte_seconds(raw: u16) -> Option<u32> {
    (raw != 0xFFFF).then(|| u32::from(raw) * 45 / 8)
}

/// The PCA9420's `MODECFG_0_0`; mode `n`'s four registers start `4 * n` after it.
pub const MODECFG_BASE: u8 = 0x22;

/// One PCA9420 mode's `MODECFG_n_0` to `MODECFG_n_3`, decoded (datasheet rev 4.2, Tables 44 to 51).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rails {
    pub ship: bool,
    /// The mode is chosen by the `MODESEL` pins rather than `TOP_CNTL3`'s bits.
    pub by_pin: bool,
    pub sw1_mv: u16,
    pub sw2_mv: u16,
    pub ldo1_mv: u16,
    pub ldo2_mv: u16,
    /// SW1, SW2, LDO1 and LDO2 enabled.
    pub enabled: [bool; 4],
    /// `None` when off.
    pub watchdog_s: Option<u8>,
}

impl Rails {
    #[must_use]
    pub fn decode(r: [u8; 4]) -> Self {
        let sw1 = r[0] & 0x3F;
        let sw1_mv = match sw1 {
            0x3F => 1800,
            0..=0x28 => 500 + 25 * u16::from(sw1),
            _ => 1500,
        };
        let offset = |bit: bool| if bit { 1200 } else { 0 };
        let wide = |lsb: u8| 1500 + 25 * u16::from(lsb.min(0x18));
        Self {
            ship: r[0] & 0x80 != 0,
            by_pin: r[0] & 0x40 != 0,
            sw1_mv,
            sw2_mv: wide(r[1] & 0x1F) + offset(r[1] & 0x20 != 0),
            ldo1_mv: 1700 + 25 * u16::from((r[2] >> 4).min(8)),
            ldo2_mv: wide(r[3] & 0x1F) + offset(r[3] & 0x20 != 0),
            enabled: [r[2] & 8 != 0, r[2] & 4 != 0, r[2] & 2 != 0, r[2] & 1 != 0],
            watchdog_s: match r[3] >> 6 {
                0 => None,
                n => Some(8 << n),
            },
        }
    }
}

/// The mode in force from `TOP_CNTL3`'s `MODE1_I2C:MODE0_I2C`, when the mode's own `by_pin` is
/// clear (datasheet Table 20).
#[must_use]
pub const fn i2c_mode(top_cntl3: u8) -> u8 {
    (top_cntl3 >> 3) & 3
}
