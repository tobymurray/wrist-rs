#![no_std]

//! The DRV2625 haptic driver (TI SLOS879C) as the watch drives it: its ERM motor open-loop
//! through real-time playback, so no calibration is needed (`HARDWARE.md`, "Haptics").

/// Register addresses (SLOS879C Table 8-1).
pub mod reg {
    pub const CHIP_ID: u8 = 0x00;
    pub const STATUS: u8 = 0x01;
    pub const VBAT: u8 = 0x04;
    pub const MODE: u8 = 0x07;
    pub const CONTROL1: u8 = 0x08;
    pub const GO: u8 = 0x0C;
    pub const RTP_INPUT: u8 = 0x0E;
    pub const RATED_VOLTAGE: u8 = 0x1F;
    pub const OD_CLAMP: u8 = 0x20;
}

/// `CHIP_ID[3:0]`, the high nibble of register 0x00; 0 is the DRV2624.
pub const CHIP_DRV2625: u8 = 1;

#[must_use]
pub const fn is_drv2625(id: u8) -> bool {
    id >> 4 == CHIP_DRV2625
}

/// `REV[3:0]`: 2 and 3 were both released.
#[must_use]
pub const fn revision(id: u8) -> u8 {
    id & 0xF
}

/// The motor's rated voltage: the BOM's "Vibration ERM motor, 2.7V DC". Nothing here drives
/// it above that, though an ERM is usually overdriven to start faster.
pub const RATED_MV: u32 = 2_700;

/// ERM open loop: full scale is `OD_CLAMP` × 21.59 mV (SLOS879C Equation 8). Rounded down, so
/// the drive stays at or under `mv`.
#[must_use]
pub const fn od_clamp_code(mv: u32) -> u8 {
    saturate(mv * 100 / 2159)
}

#[must_use]
pub const fn od_clamp_mv(code: u8) -> u32 {
    code as u32 * 2159 / 100
}

/// ERM closed loop: `RATED_VOLTAGE` × 21.88 mV (Equation 6). Open loop ignores it; it is
/// written so a closed-loop experiment starts from the motor's rating.
#[must_use]
pub const fn rated_voltage_code(mv: u32) -> u8 {
    saturate(mv * 100 / 2188)
}

#[allow(clippy::cast_possible_truncation)] // under 256 once clamped
const fn saturate(code: u32) -> u8 {
    if code > 255 { 255 } else { code as u8 }
}

/// `VDD` while playing (Equation 5); the register reads 0 otherwise.
#[must_use]
pub const fn vbat_mv(raw: u8) -> u32 {
    raw as u32 * 5600 / 255
}

/// `MODE`: real-time playback, and `TRIG_PIN_FUNC` 0 (pulse trigger) so the `GO` bit starts
/// and stops it; the reset value's 1, level trigger, ignores `GO`. `LRA_PERIOD_AVG_DIS`
/// keeps its reset value.
pub const MODE_RTP_BY_GO: u8 = 0x40;
/// `CONTROL1`: `LRA_ERM` 0 (ERM), `CONTROL_LOOP` 1 (open loop), and neither automatic brake,
/// which both rely on a calibrated closed loop.
pub const CONTROL1_ERM_OPEN_LOOP: u8 = 0x40;

/// Written after each release from reset, which loses every register, then read back.
pub const BRING_UP: [(u8, u8); 4] = [
    (reg::MODE, MODE_RTP_BY_GO),
    (reg::CONTROL1, CONTROL1_ERM_OPEN_LOOP),
    (reg::RATED_VOLTAGE, rated_voltage_code(RATED_MV)),
    (reg::OD_CLAMP, od_clamp_code(RATED_MV)),
];

/// `TRIG_PIN_FUNC[1:0]` of a `MODE` value.
#[must_use]
pub const fn trigger_function(mode: u8) -> u8 {
    (mode >> 2) & 0b11
}

/// `MODE[1:0]`: 0 is real-time playback.
#[must_use]
pub const fn work_mode(mode: u8) -> u8 {
    mode & 0b11
}

/// `STATUS`, whose bits clear when read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status(pub u8);

impl Status {
    pub const DIAG_RESULT: u8 = 1 << 7;
    pub const PROCESS_DONE: u8 = 1 << 3;
    pub const UVLO: u8 = 1 << 2;
    pub const OVER_TEMP: u8 = 1 << 1;
    pub const OC_DETECT: u8 = 1 << 0;

    /// What stops the output stage: a supply droop, heat, or too much current.
    pub const FAULTS: [(u8, &'static str); 3] =
        [(Self::UVLO, "UVLO"), (Self::OVER_TEMP, "OVER_TEMP"), (Self::OC_DETECT, "OC_DETECT")];

    #[must_use]
    pub const fn faulted(self) -> bool {
        self.0 & (Self::UVLO | Self::OVER_TEMP | Self::OC_DETECT) != 0
    }

    pub fn faults(self) -> impl Iterator<Item = &'static str> {
        Self::FAULTS.into_iter().filter(move |&(bit, _)| self.0 & bit != 0).map(|(_, name)| name)
    }
}

/// Real-time playback's full scale, `OD_CLAMP`'s voltage; the input is signed.
pub const RTP_FULL: u8 = 0x7F;

/// The drive at `level` for `on_ms`, then nothing for `off_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pulse {
    pub level: u8,
    pub on_ms: u16,
    pub off_ms: u16,
}

const fn pulse(level: u8, on_ms: u16, off_ms: u16) -> Pulse {
    Pulse { level, on_ms, off_ms }
}

/// Short enough that an ERM has barely spun up: about what a coin motor needs to be felt.
const TAP: [Pulse; 1] = [pulse(RTP_FULL, 40, 0)];
/// Six buzzes at the rated voltage, 3.6 s in all.
const ALARM: [Pulse; 6] = [pulse(RTP_FULL, 350, 250); 6];

/// What [`Pattern::pulses`] may hold: no longer on at once, and no stronger, than this.
pub const MAX_ON_MS: u16 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// One short knock, for a press or a notice.
    Tap,
    /// A strong buzz repeated for a few seconds, until it ends or is stopped.
    Alarm,
}

impl Pattern {
    pub const ALL: [Self; 2] = [Self::Tap, Self::Alarm];

    #[must_use]
    pub const fn pulses(self) -> &'static [Pulse] {
        match self {
            Self::Tap => &TAP,
            Self::Alarm => &ALARM,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tap => "tap",
            Self::Alarm => "alarm",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == word)
    }

    /// From the first drive to the end of the last pause.
    #[must_use]
    pub fn total_ms(self) -> u32 {
        self.pulses().iter().map(|p| u32::from(p.on_ms) + u32::from(p.off_ms)).sum()
    }
}
