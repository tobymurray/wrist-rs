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

/// `CHIP_ID[3:0]` for the DRV2625 (SLOS879C Table 8-1).
pub const CHIP_DRV2625: u8 = 1;

#[must_use]
pub const fn is_drv2625(id: u8) -> bool {
    id >> 4 == CHIP_DRV2625
}

/// `REV[3:0]` of a `CHIP_ID` value.
#[must_use]
pub const fn revision(id: u8) -> u8 {
    id & 0xF
}

/// The motor's rated voltage: the BOM's "Vibration ERM motor, 2.7V DC".
pub const RATED_MV: u32 = 2_700;

/// ERM open loop's full scale (SLOS879C Equation 8), rounded down so the drive stays at or
/// under `mv`.
#[must_use]
pub const fn od_clamp_code(mv: u32) -> u8 {
    saturate(mv * 100 / 2159)
}

#[must_use]
pub const fn od_clamp_mv(code: u8) -> u32 {
    code as u32 * 2159 / 100
}

/// ERM closed loop's rated voltage (Equation 6), which open loop ignores.
#[must_use]
pub const fn rated_voltage_code(mv: u32) -> u8 {
    saturate(mv * 100 / 2188)
}

#[allow(clippy::cast_possible_truncation)] // under 256 once clamped
const fn saturate(code: u32) -> u8 {
    if code > 255 { 255 } else { code as u8 }
}

/// `VDD` from a `VBAT` read while playing (Equation 5).
#[must_use]
pub const fn vbat_mv(raw: u8) -> u32 {
    raw as u32 * 5600 / 255
}

/// `MODE`: real-time playback, with `TRIG_PIN_FUNC` 0 so the `GO` bit starts and stops it.
pub const MODE_RTP_BY_GO: u8 = 0x40;
/// `CONTROL1`: ERM, open loop, and no automatic brake.
pub const CONTROL1_ERM_OPEN_LOOP: u8 = 0x40;

/// Written after each release from reset, then read back.
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

const TAP: Pulse = pulse(RTP_FULL, 40, 0);
const ALARM: Pulse = pulse(RTP_FULL, 350, 250);

/// The longest an alarm rings.
pub const ALARM_FOR_MS: u32 = 60_000;

/// What [`Pattern::pulse`] may be: no longer on at once, and no stronger, than this.
pub const MAX_ON_MS: u16 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// One short knock.
    Tap,
    /// A strong buzz repeated until it is stopped, or for [`ALARM_FOR_MS`].
    Alarm,
}

impl Pattern {
    pub const ALL: [Self; 2] = [Self::Tap, Self::Alarm];

    /// What each of its pulses plays.
    #[must_use]
    pub const fn pulse(self) -> Pulse {
        match self {
            Self::Tap => TAP,
            Self::Alarm => ALARM,
        }
    }

    /// The pulse to begin `elapsed_ms` in, after `played` others, or `None` once a tap has
    /// played or an alarm's next pulse and pause would end past [`ALARM_FOR_MS`].
    #[must_use]
    pub const fn next_pulse(self, played: u32, elapsed_ms: u32) -> Option<Pulse> {
        let p = self.pulse();
        let over = match self {
            Self::Tap => played > 0,
            Self::Alarm => elapsed_ms.saturating_add(cycle_ms(p)) > ALARM_FOR_MS,
        };
        if over { None } else { Some(p) }
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

    /// From the first drive to the end of the last pause, if nothing is late or stopped.
    #[must_use]
    pub const fn total_ms(self) -> u32 {
        let (mut played, mut ms) = (0, 0);
        while let Some(p) = self.next_pulse(played, ms) {
            played += 1;
            ms += cycle_ms(p);
        }
        ms
    }
}

const fn cycle_ms(p: Pulse) -> u32 {
    p.on_ms as u32 + p.off_ms as u32
}

/// How a pattern ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Played,
    Stopped,
    Failed,
}

impl Ending {
    /// A stop wins, since its reset fails the transfers after it; a fault, or a busy bus
    /// skipping every pulse, fails.
    #[must_use]
    pub const fn of(stopped: bool, faulted: bool, tried: u32, missed: u32) -> Self {
        if stopped {
            Self::Stopped
        } else if faulted || (missed > 0 && missed == tried) {
            Self::Failed
        } else {
            Self::Played
        }
    }
}
