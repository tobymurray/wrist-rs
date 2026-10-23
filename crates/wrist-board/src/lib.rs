#![no_std]

//! Net-to-pin assignments for the `UNAcore` Rev3.2 board.

pub mod pins;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    pub port: Port,
    pub number: u8,
}

const fn pin(port: Port, number: u8) -> Pin {
    Pin { port, number }
}

/// 7-bit I2C addresses.
pub mod i2c {
    pub const DRV2625_HAPTIC: u8 = 0x5A;
    pub const PCA9420_PMIC: u8 = 0x61;
    pub const MAX17262_GAUGE: u8 = 0x36;
    pub const STUSB4500_USB_PD: u8 = 0x29;
}
