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

/// `HibCfg` with `HibScalar` at its largest: in hibernate the MAX17262 samples every
/// 351 ms × 2^7 = 45 s instead of the default 5.6 s (ADI UG6597). A value with `EnHib`
/// clear, as a failed restore leaves, starts from the default `0x870C` instead.
#[must_use]
pub const fn slow_hibernate(hib_cfg: u16) -> u16 {
    let base = if hib_cfg & (1 << 15) != 0 { hib_cfg } else { 0x870C };
    base | 0b111
}

/// Whether the MAX17262 has been reset since it was last configured: `Status.POR`, bit 1.
#[must_use]
pub const fn gauge_reset(status: u16) -> bool {
    status & (1 << 1) != 0
}

/// One step of configuring the MAX17262 after a reset (ADI UG6595, Steps 1 to 3, EZ option).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaugeStep {
    /// Read the register until these bits are clear.
    WaitClear(u8, u16),
    Write(u8, u16),
    /// Read `HibCfg`, for [`GaugeStep::RestoreHibCfg`].
    SaveHibCfg,
    /// Write it back, with [`slow_hibernate`].
    RestoreHibCfg,
    /// Clear `Status.POR`, and read it back.
    ClearPor,
}

/// The stock firmware's configuration, as read from the watch (`HARDWARE.md`): 200 mAh,
/// 13.9 mA end of charge, empty at 3.40 V, charged above 4.275 V. `ModelCfg` carries `Refresh`.
pub const GAUGE_RESTORE: [GaugeStep; 12] = [
    GaugeStep::WaitClear(0x3D, 1),
    GaugeStep::SaveHibCfg,
    GaugeStep::Write(0x60, 0x0090),
    GaugeStep::Write(0xBA, 0),
    GaugeStep::Write(0x60, 0),
    GaugeStep::Write(0x18, 0x0190),
    GaugeStep::Write(0x1E, 0x0059),
    GaugeStep::Write(0x3A, 0xAA63),
    GaugeStep::Write(0xDB, 0x8400),
    GaugeStep::WaitClear(0xDB, 0x8000),
    GaugeStep::RestoreHibCfg,
    GaugeStep::ClearPor,
];

const HIB_CFG: u8 = 0xBA;
const STATUS: u8 = 0x00;

/// The MAX17262's registers, a little-endian word at a time.
#[allow(async_fn_in_trait)]
pub trait GaugeBus {
    async fn read(&mut self, reg: u8) -> Option<u16>;
    async fn write(&mut self, reg: u8, value: u16) -> bool;
    async fn wait_ms(&mut self, ms: u32);
}

/// Runs [`GAUGE_RESTORE`]. A failure after `HibCfg` was saved still writes it back, so the
/// gauge is never left out of hibernate.
///
/// # Errors
///
/// The step that failed.
pub async fn restore_gauge(bus: &mut impl GaugeBus) -> Result<(), &'static str> {
    let mut hib_cfg = None;
    let outcome = run_restore(bus, &mut hib_cfg).await;
    if outcome.is_err() {
        if let Some(v) = hib_cfg {
            bus.write(HIB_CFG, slow_hibernate(v)).await;
        }
    }
    outcome
}

async fn run_restore(bus: &mut impl GaugeBus, hib_cfg: &mut Option<u16>) -> Result<(), &'static str> {
    for step in GAUGE_RESTORE {
        match step {
            GaugeStep::WaitClear(reg, mask) => {
                let mut tries = 0;
                while bus.read(reg).await.ok_or("no answer while waiting")? & mask != 0 {
                    tries += 1;
                    if tries > 100 {
                        return Err("still busy after 1 s");
                    }
                    bus.wait_ms(10).await;
                }
            }
            GaugeStep::Write(reg, v) => {
                if !bus.write(reg, v).await {
                    return Err("a write failed");
                }
            }
            GaugeStep::SaveHibCfg => *hib_cfg = Some(bus.read(HIB_CFG).await.ok_or("HibCfg unread")?),
            GaugeStep::RestoreHibCfg => {
                let v = hib_cfg.take().ok_or("HibCfg not saved")?;
                if !bus.write(HIB_CFG, slow_hibernate(v)).await {
                    *hib_cfg = Some(v);
                    return Err("HibCfg unwritten");
                }
            }
            GaugeStep::ClearPor => {
                let status = bus.read(STATUS).await.ok_or("Status unread")?;
                if !bus.write(STATUS, status & !(1 << 1)).await {
                    return Err("Status unwritten");
                }
                if gauge_reset(bus.read(STATUS).await.ok_or("Status unread")?) {
                    return Err("POR still set");
                }
            }
        }
    }
    Ok(())
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
