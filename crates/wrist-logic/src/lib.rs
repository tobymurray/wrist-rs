#![cfg_attr(not(test), no_std)]

//! The image's decisions and device registers that need no hardware to check.

pub mod console;
pub mod crash;
pub mod eeprom;
pub mod haptic;
pub mod pages;
pub mod power;
pub mod time;
