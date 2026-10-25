#![cfg_attr(not(test), no_std)]

//! The installer's screen, kept apart from the watch so it can be checked on the host.

pub mod draw;
pub mod flow;
pub mod installer;
pub mod label;

#[cfg(feature = "device")]
pub mod ffi;
