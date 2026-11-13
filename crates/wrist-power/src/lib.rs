#![no_std]

//! Whether the watch is on external power, and what a stretch on battery cost.

/// The PCA9420's registers (datasheet Rev 4.2).
pub mod pca9420 {
    pub const DEV_INFO: u8 = 0x00;
    pub const TOP_CNTL0: u8 = 0x09;
    pub const TOP_CNTL1: u8 = 0x0A;
    pub const TOP_CNTL2: u8 = 0x0B;
    pub const TOP_CNTL3: u8 = 0x0C;
    pub const CHG_STATUS_0: u8 = 0x18;
    pub const CHG_STATUS_1: u8 = 0x19;
    pub const CHG_STATUS_2: u8 = 0x1A;
    pub const REG_STATUS: u8 = 0x20;
    pub const RESET_MONITOR: u8 = 0x71;
    /// `CHG_STATUS_1.IN_PWR_STATUS`, bits 7:6; 0 is `VIN` below `VIN_UVLO` (Table 39).
    pub const IN_PWR_STATUS_SHIFT: u32 = 6;
    /// `CHG_STATUS_2.BAT_CHG_STATUS` Done (Table 40).
    pub const BAT_CHG_DONE: u8 = 0b110;
}

/// The MAX17262's registers and the bits used (ADI UG6595, UG6597).
pub mod max17262 {
    pub const STATUS: u8 = 0x00;
    pub const REP_CAP: u8 = 0x05;
    pub const REP_SOC: u8 = 0x06;
    pub const TEMP: u8 = 0x08;
    pub const VCELL: u8 = 0x09;
    pub const CURRENT: u8 = 0x0A;
    pub const AVG_CURRENT: u8 = 0x0B;
    pub const MIX_SOC: u8 = 0x0D;
    pub const AV_SOC: u8 = 0x0E;
    pub const FULL_CAP_REP: u8 = 0x10;
    pub const TTE: u8 = 0x11;
    pub const FULL_SOC_THR: u8 = 0x13;
    pub const CYCLES: u8 = 0x17;
    pub const DESIGN_CAP: u8 = 0x18;
    pub const CONFIG: u8 = 0x1D;
    pub const ICHG_TERM: u8 = 0x1E;
    pub const FULL_CAP_NOM: u8 = 0x23;
    pub const LEARN_CFG: u8 = 0x28;
    pub const MISC_CFG: u8 = 0x2B;
    pub const CGAIN: u8 = 0x2E;
    pub const COFF: u8 = 0x2F;
    pub const RCOMP0: u8 = 0x38;
    pub const TEMP_CO: u8 = 0x39;
    pub const V_EMPTY: u8 = 0x3A;
    pub const FSTAT: u8 = 0x3D;
    pub const COMMAND: u8 = 0x60;
    pub const STATUS2: u8 = 0xB0;
    pub const HIB_CFG: u8 = 0xBA;
    pub const CONFIG2: u8 = 0xBB;
    pub const MODEL_CFG: u8 = 0xDB;
    pub const VFOCV: u8 = 0xFB;
    pub const VFSOC: u8 = 0xFF;

    pub const STATUS_POR: u16 = 1 << 1;
    pub const FSTAT_DNR: u16 = 1 << 0;
    pub const FSTAT_REL_DT2: u16 = 1 << 6;
    pub const FSTAT_FQ: u16 = 1 << 7;
    pub const FSTAT_EDET: u16 = 1 << 8;
    pub const FSTAT_REL_DT: u16 = 1 << 9;
    pub const STATUS2_FULL_DET: u16 = 1 << 5;
    pub const MODEL_CFG_REFRESH: u16 = 1 << 15;
    pub const SOFT_WAKEUP: u16 = 0x0090;
}

use max17262::{
    COMMAND, DESIGN_CAP, FSTAT, FSTAT_DNR, HIB_CFG, ICHG_TERM, MODEL_CFG, MODEL_CFG_REFRESH, SOFT_WAKEUP, STATUS, STATUS_POR,
    V_EMPTY,
};

/// From the PCA9420's `CHG_STATUS_1` and the USB-PD attach bit: an attachment
/// wins, and `None` means neither answered.
#[must_use]
pub fn external_power(pd_attached: Option<bool>, charger_status: Option<u8>) -> Option<bool> {
    if pd_attached == Some(true) { return Some(true); }
    charger_status.map(|v| v >> pca9420::IN_PWR_STATUS_SHIFT != 0).or(pd_attached)
}

#[must_use]
pub const fn charge_done(chg_status_2: u8) -> bool {
    chg_status_2 & 0b111 == pca9420::BAT_CHG_DONE
}

/// On battery or an unknown input the charge shown only falls; on external power it follows the
/// gauge but reads 100 % only once the charger is done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShownCharge(Option<u8>);

impl ShownCharge {
    #[must_use]
    pub const fn new() -> Self {
        Self(None)
    }

    /// `gauge` in whole percent; `None` keeps the last shown.
    pub fn reading(&mut self, gauge: Option<u8>, external: Option<bool>, done: Option<bool>) -> Option<u8> {
        if let Some(g) = gauge {
            self.0 = Some(match (external, done) {
                (Some(true), Some(true)) => 100,
                (Some(true), _) => g.min(99),
                _ => self.0.map_or(g, |shown| shown.min(g)),
            });
        }
        self.0
    }
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

/// `HibCfg` 0: the MAX17262 never hibernates. Hibernating, it read the current once every 45 s,
/// missed a bursty draw, and at rest took noise in the end-of-charge band for a charge ending,
/// reporting the cell full (ADI UG6597). It costs about 11 µA (datasheet, 16 against 5.2).
pub const HIB_CFG_AWAKE: u16 = 0;

/// `IChgTerm` 5.0 mA: what the gauge reads as the charger stops. It reads the PCA9420's 100 mA
/// charge as 99 to 100 mA, so the nominal 10 mA top-off ends nearer 5; stock's 13.9 mA called
/// the cell full early.
pub const ICHG_TERM_WATCH: u16 = 0x0020;

/// `DesignCap` 220 mAh, a UNA cell down to [`V_EMPTY_WATCH`] (`HARDWARE.md`); each gauge learns
/// its own cell from there.
pub const DESIGN_CAP_WATCH: u16 = 0x01B8;

/// `VEmpty` empty at 3.30 V, recovery 3.88 V, the MAX17262's default: above where the watch's draw
/// rises (`HARDWARE.md`).
pub const V_EMPTY_WATCH: u16 = 0xA561;

/// Whether `DesignCap` and `VEmpty` were left by stock or an older image, and need
/// [`GAUGE_RESTORE`]. Its `Refresh` keeps the learned `FullCapRep` and `Cycles` (`HARDWARE.md`).
#[must_use]
pub const fn gauge_model_differs(design_cap: u16, v_empty: u16) -> bool {
    design_cap != DESIGN_CAP_WATCH || v_empty != V_EMPTY_WATCH
}

/// Whether `HibCfg` and `IChgTerm` need [`GAUGE_SETTLE`].
#[must_use]
pub const fn gauge_unsettled(hib_cfg: u16, ichg_term: u16) -> bool {
    hib_cfg != HIB_CFG_AWAKE || ichg_term != ICHG_TERM_WATCH
}

/// Whether the MAX17262 has been reset since it was last configured: `Status.POR`, bit 1.
#[must_use]
pub const fn gauge_reset(status: u16) -> bool {
    status & STATUS_POR != 0
}

/// One step of configuring the MAX17262 (ADI UG6595, Steps 1 to 3, EZ option).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaugeStep {
    /// Read the register until these bits are clear.
    WaitClear(u8, u16),
    Write(u8, u16),
    /// Clear `Status.POR`, and read it back.
    ClearPor,
}

/// Out of hibernate: 0x90 to `Command`, `HibCfg` 0, `Command` cleared (ADI UG6595).
const WAKE: [GaugeStep; 3] = [
    GaugeStep::Write(COMMAND, SOFT_WAKEUP),
    GaugeStep::Write(HIB_CFG, HIB_CFG_AWAKE),
    GaugeStep::Write(COMMAND, 0),
];

/// After a reset, or over a configuration [`gauge_model_differs`] finds: awake, then
/// [`DESIGN_CAP_WATCH`], [`ICHG_TERM_WATCH`], [`V_EMPTY_WATCH`], and stock's `ModelCfg`, charged
/// above 4.275 V, carrying `Refresh`.
pub const GAUGE_RESTORE: [GaugeStep; 10] = [
    GaugeStep::WaitClear(FSTAT, FSTAT_DNR),
    WAKE[0],
    WAKE[1],
    WAKE[2],
    GaugeStep::Write(DESIGN_CAP, DESIGN_CAP_WATCH),
    GaugeStep::Write(ICHG_TERM, ICHG_TERM_WATCH),
    GaugeStep::Write(V_EMPTY, V_EMPTY_WATCH),
    GaugeStep::Write(MODEL_CFG, 0x8400),
    GaugeStep::WaitClear(MODEL_CFG, MODEL_CFG_REFRESH),
    GaugeStep::ClearPor,
];

/// Without a reset, on a gauge stock or an older image configured: awake, [`ICHG_TERM_WATCH`].
pub const GAUGE_SETTLE: [GaugeStep; 4] = [WAKE[0], WAKE[1], WAKE[2], GaugeStep::Write(ICHG_TERM, ICHG_TERM_WATCH)];

/// The MAX17262's registers, a little-endian word at a time.
#[allow(async_fn_in_trait)]
pub trait GaugeBus {
    async fn read(&mut self, reg: u8) -> Option<u16>;
    async fn write(&mut self, reg: u8, value: u16) -> bool;
    async fn wait_ms(&mut self, ms: u32);
}

/// Runs `steps`, [`GAUGE_RESTORE`] or [`GAUGE_SETTLE`].
///
/// # Errors
///
/// The step that failed.
pub async fn configure_gauge(bus: &mut impl GaugeBus, steps: &[GaugeStep]) -> Result<(), &'static str> {
    for &step in steps {
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
            GaugeStep::ClearPor => {
                let status = bus.read(STATUS).await.ok_or("Status unread")?;
                if !bus.write(STATUS, status & !STATUS_POR).await {
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

/// A MAX17262 register's unit (ADI UG6597, "Register Type"; the MAX17262 datasheet's register LSBs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// 1/256 %.
    Percent,
    /// 0.5 mAh.
    Capacity,
    /// 78.125 µV.
    Voltage,
    /// 156.25 µA, signed, positive into the battery.
    Current,
    /// 1 % of a cycle.
    Cycles,
}

impl Unit {
    /// `raw` in hundredths of [`Unit::name`], toward zero.
    #[must_use]
    pub fn hundredths(self, raw: u16) -> i32 {
        let r = i64::from(raw);
        let v = match self {
            Self::Percent => r * 100 / 256,
            Self::Capacity => r * 50,
            Self::Voltage => r * 78_125 / 10_000,
            Self::Current => i64::from(raw.cast_signed()) * 125 / 8,
            Self::Cycles => r,
        };
        i32::try_from(v).unwrap_or(i32::MAX)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Percent => "%",
            Self::Capacity => "mAh",
            Self::Voltage => "mV",
            Self::Current => "mA",
            Self::Cycles => "cycles",
        }
    }
}

/// `Status2.FullDet` (ADI UG6597).
#[must_use]
pub const fn full_detected(status2: u16) -> bool {
    status2 & max17262::STATUS2_FULL_DET != 0
}

/// `LearnCfg.LS` (ADI UG6597).
#[must_use]
pub const fn learn_stage(learn_cfg: u16) -> u8 {
    ((learn_cfg >> 4) & 0x7).to_le_bytes()[0]
}

/// `MiscCfg.FUS` (ADI UG6597).
#[must_use]
pub const fn full_update_slope(misc_cfg: u16) -> u8 {
    (misc_cfg >> 12).to_le_bytes()[0]
}

/// `MiscCfg.MR`, in 6.25 µV across the sense resistor (ADI UG6597).
#[must_use]
pub const fn mixing_rate(misc_cfg: u16) -> u8 {
    ((misc_cfg >> 5) & 0x1F).to_le_bytes()[0]
}

/// The names of the `FStat` flags set (ADI UG6597, Table 20).
pub fn fstat_flags(fstat: u16) -> impl Iterator<Item = &'static str> {
    use max17262::{FSTAT_DNR, FSTAT_EDET, FSTAT_FQ, FSTAT_REL_DT, FSTAT_REL_DT2};
    [(FSTAT_DNR, "DNR"), (FSTAT_REL_DT, "RelDt"), (FSTAT_REL_DT2, "RelDt2"), (FSTAT_FQ, "FQ"), (FSTAT_EDET, "EDet")]
        .into_iter()
        .filter(move |&(bit, _)| fstat & bit != 0)
        .map(|(_, name)| name)
}

/// Whether `VFSOC`, `Current` and `AvgCurrent`, in that order, pass end-of-charge detection (ADI UG6597).
#[must_use]
pub fn end_of_charge(vfsoc: u16, full_soc_thr: u16, ichg_term: u16, current: u16, avg_current: u16) -> [bool; 3] {
    let term = i32::from(ichg_term.cast_signed());
    let in_band = |raw: u16| {
        let i = i32::from(raw.cast_signed());
        8 * i > term && 4 * i < 5 * term
    };
    [vfsoc > full_soc_thr, in_band(current), in_band(avg_current)]
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
