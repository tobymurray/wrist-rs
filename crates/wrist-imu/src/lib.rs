#![no_std]

//! The BMI270 on the GNSS module: its registers, the upload of Bosch's configuration image,
//! and the step counter's setup and outputs.
//!
//! The configuration image is `bmi270_config_file[]` from Bosch Sensortec's BMI270 Sensor API
//! (`bmi270.c` at commit 41129fc), under the BSD-3-Clause licence in `LICENSE-BOSCH`.

pub const ADDR: u8 = 0x68;
pub const CHIP_ID: u8 = 0x24;

pub mod reg {
    pub const CHIP_ID: u8 = 0x00;
    pub const INTERNAL_STATUS: u8 = 0x21;
    pub const FEAT_PAGE: u8 = 0x2F;
    pub const FEATURES: u8 = 0x30;
    pub const ACC_CONF: u8 = 0x40;
    pub const INIT_CTRL: u8 = 0x59;
    pub const INIT_ADDR_0: u8 = 0x5B;
    pub const INIT_DATA: u8 = 0x5E;
    pub const PWR_CONF: u8 = 0x7C;
    pub const PWR_CTRL: u8 = 0x7D;
    pub const CMD: u8 = 0x7E;
}

pub const SOFT_RESET: u8 = 0xB6;

/// After a write with advanced power save on, the next access waits this long.
pub const POWER_SAVE_GAP_US: u64 = 450;

pub static CONFIG: &[u8; 8192] = include_bytes!("../bosch/bmi270_config.bin");

/// Bytes per `INIT_DATA` burst: about 3 ms at 400 kHz.
pub const CHUNK: usize = 128;

/// One burst of the configuration image: write `addr`, then `data`, each as one transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// `INIT_ADDR_0` and the image's word address: its low nibble, then the rest.
    pub addr: [u8; 3],
    /// `INIT_DATA` and the burst.
    pub data: [u8; 1 + CHUNK],
}

/// The image in order, addressed in 16-bit words.
pub fn chunks() -> impl Iterator<Item = Chunk> {
    CONFIG.chunks(CHUNK).enumerate().map(|(i, bytes)| {
        let word = i * CHUNK / 2;
        let mut data = [0; 1 + CHUNK];
        data[0] = reg::INIT_DATA;
        data[1..].copy_from_slice(bytes);
        #[allow(clippy::cast_possible_truncation)]
        let addr = [reg::INIT_ADDR_0, (word & 0x0F) as u8, (word >> 4) as u8];
        Chunk { addr, data }
    })
}

/// `INTERNAL_STATUS.message`: the image was accepted and the feature engine runs.
#[must_use]
pub const fn engine_ok(internal_status: u8) -> bool {
    internal_status & 0x0F == 1
}

/// The accelerometer on alone (`PWR_CTRL.acc_en`).
pub const PWR_CTRL: u8 = 0x04;
/// 50 Hz, two repetitions, power-optimised: the low-power recipe of the datasheet's section 4.3,
/// at the rate the features need.
pub const ACC_CONF: u8 = 0x17;
/// Advanced power save, with the FIFO's self wake-up left on as the recipe has it.
pub const PWR_CONF: u8 = 0x03;

/// Feature page 6 holds the step counter's `SC_26` word at offset 2.
pub const STEP_PAGE: u8 = 6;
pub const PAGE_LEN: usize = 16;
const SC_26_HIGH: usize = 3;
/// `en_counter` and `en_activity`, bits 12 and 13 of `SC_26`.
const STEPS_ON: u8 = 0x30;

pub fn enable_steps(page: &mut [u8; PAGE_LEN]) {
    page[SC_26_HIGH] |= STEPS_ON;
}

#[must_use]
pub const fn steps_enabled(page: &[u8; PAGE_LEN]) -> bool {
    page[SC_26_HIGH] & STEPS_ON == STEPS_ON
}

/// What the chip holds when the image starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub internal_status: u8,
    pub pwr_ctrl: u8,
    pub acc_conf: u8,
    pub pwr_conf: u8,
    pub step_page: [u8; PAGE_LEN],
}

impl Found {
    /// Counting as this image sets it up, so the count it holds carries on: the chip keeps its
    /// state across the MCU's resets and updates.
    #[must_use]
    pub const fn adoptable(&self) -> bool {
        engine_ok(self.internal_status)
            && self.pwr_ctrl == PWR_CTRL
            && self.acc_conf == ACC_CONF
            && self.pwr_conf == PWR_CONF
            && steps_enabled(&self.step_page)
    }
}

/// Feature page 0: the step count at offset 0, the activity at offset 4.
pub const OUT_PAGE: u8 = 0;
pub const OUT_LEN: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Still,
    Walking,
    Running,
    Unknown,
}

impl Activity {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Still => "still",
            Self::Walking => "walking",
            Self::Running => "running",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outputs {
    pub steps: u32,
    pub activity: Activity,
}

impl Outputs {
    #[must_use]
    pub const fn parse(out: &[u8; OUT_LEN]) -> Self {
        let activity = match out[4] & 0x03 {
            0 => Activity::Still,
            1 => Activity::Walking,
            2 => Activity::Running,
            _ => Activity::Unknown,
        };
        Self { steps: u32::from_le_bytes([out[0], out[1], out[2], out[3]]), activity }
    }
}
