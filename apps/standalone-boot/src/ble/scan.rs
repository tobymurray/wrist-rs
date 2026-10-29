//! The watch as a central, scanning: `ble scan` lists advertisers for a few seconds.

use core::sync::atomic::Ordering;

use embassy_time::{Duration, Instant};
use wrist_aci::opcode::{ACI_GAP_SET_NON_DISCOVERABLE, ACI_GAP_START_GENERAL_DISCOVERY_PROC, ACI_GAP_TERMINATE_GAP_PROC};
use wrist_aci::{address, gap_proc, Address, Uuid};
use wrist_wake::ble::{scan, Scan};

use super::{power, Link, Request, STATE, STATE_ADVERTISING, STATE_IDLE};
use crate::uart::Uart;

const SCAN_DEFAULT_S: u8 = 10;
/// The chip ends a general discovery itself at about 10 s (HARDWARE.md).
const SCAN_MAX_S: u8 = 10;
/// Lines one scan prints; past this, advertisers are only counted.
const SCAN_LINES: u32 = 48;
/// Advertisers one scan tells apart; past this, repeats may print again.
const SCAN_SEEN: usize = 32;
/// HCI error: Command Disallowed.
const COMMAND_DISALLOWED: u8 = 0x0C;

pub(super) const USAGE: &str = "[ble] usage: ble scan [seconds, 1 to 10] [interval ms] [window ms]";

/// The words after `ble scan`; `None` if mistyped.
pub(super) fn parse(args: &[u8]) -> Option<Request> {
    let text = core::str::from_utf8(args).ok()?;
    let mut words = text.split_whitespace();
    let seconds = words.next().map(str::parse::<u8>);
    let ms = |w: Option<&str>| w.and_then(|w| w.parse::<u32>().ok());
    let duty = match (ms(words.next()), ms(words.next())) {
        (None, None) => None,
        // Core Specification: 2.5 ms to 10.24 s, the window no longer than the interval.
        (Some(i), Some(w)) if (3..=10_240).contains(&i) && (3..=i).contains(&w) => Some(Scan {
            interval: u16::try_from(i * 8 / 5).unwrap_or(16),
            window: u16::try_from(w * 8 / 5).unwrap_or(16),
        }),
        _ => return None,
    };
    if words.next().is_some() {
        return None;
    }
    match seconds {
        None => Some(Request::Scan(SCAN_DEFAULT_S, None)),
        Some(Ok(s)) if (1..=SCAN_MAX_S).contains(&s) => Some(Request::Scan(s, duty)),
        _ => None,
    }
}

fn write_address(uart: Uart, a: Address) {
    for i in (0..6).rev() {
        uart.write_hex8(a.bytes[i]);
        if i > 0 {
            uart.write_str(":");
        }
    }
    if a.kind == 1 {
        uart.write_str(" (random)");
    }
}

fn write_uuid(uart: Uart, u: Uuid) {
    match u {
        Uuid::U16(v) => {
            uart.write_hex8((v >> 8) as u8);
            uart.write_hex8((v & 0xFF) as u8);
        }
        Uuid::U128(b) => {
            for (i, byte) in b.iter().rev().enumerate() {
                if matches!(i, 4 | 6 | 8 | 10) {
                    uart.write_str("-");
                }
                uart.write_hex8(*byte);
            }
        }
    }
}

/// Printable ASCII as is, anything else as `?`.
fn write_name(uart: Uart, name: &[u8]) {
    uart.write_str("\"");
    for &b in name.iter().take(29) {
        uart.write_byte(if (0x20..0x7F).contains(&b) && b != b'"' { b } else { b'?' });
    }
    uart.write_str("\"");
}

#[derive(Default)]
pub(super) struct Scanner {
    /// The chip took the central role at GAP init.
    available: bool,
    /// A scan runs until this instant; lines printed and reports heard.
    scan: Option<(Instant, u32, u32)>,
    /// The advertisers this scan has printed (address, a scan response), oldest replaced.
    seen: [Option<(Address, bool)>; SCAN_SEEN],
    seen_next: usize,
    /// Advertising was stopped so the chip could scan; it resumes after.
    paused_advertising: bool,
}

impl Scanner {
    /// The radio is up: whether the chip took the central role.
    pub(super) fn started(&mut self, available: bool) {
        *self = Self { available, ..Self::default() };
    }

    pub(super) fn until(&self) -> Option<Instant> {
        self.scan.map(|(until, ..)| until)
    }

    /// Advertising was refused: whether a scan is the reason, in which case it resumes when the scan ends.
    pub(super) fn advertising_refused(&mut self) -> bool {
        self.paused_advertising |= self.scan.is_some();
        self.scan.is_some()
    }

    /// LE Advertising Report while a scan runs: one line per advertiser, up to [`SCAN_LINES`].
    pub(super) fn advertising(&mut self, data: &[u8], uart: Uart) {
        let Some((_, printed, heard)) = self.scan.as_mut() else {
            return;
        };
        for r in wrist_aci::reports(data) {
            *heard += 1;
            // One line per advertiser and kind.
            let key = (r.address, r.kind == 0x04);
            if *printed >= SCAN_LINES || self.seen.contains(&Some(key)) {
                continue;
            }
            self.seen[self.seen_next] = Some(key);
            self.seen_next = (self.seen_next + 1) % SCAN_SEEN;
            *printed += 1;
            uart.write_str("[ble] seen ");
            write_address(uart, r.address);
            uart.write_str(" ");
            if r.rssi < 0 {
                uart.write_str("-");
            }
            uart.write_u32(u32::from(r.rssi.unsigned_abs()));
            uart.write_str(" dBm");
            if r.kind == 0x04 {
                uart.write_str(" (scan response)");
            }
            if let Some(name) = wrist_aci::ad_name(r.data) {
                uart.write_str(" ");
                write_name(uart, name);
            }
            for u in wrist_aci::ad_uuid16s(r.data) {
                uart.write_str(" ");
                write_uuid(uart, Uuid::U16(u));
            }
            for u in wrist_aci::ad_uuid128s(r.data) {
                uart.write_str(" ");
                write_uuid(uart, Uuid::U128(*u));
            }
            uart.write_line("");
        }
    }

    /// `GAP_PROC_COMPLETE`.
    /// The chip ended the scan; the watch's own end at its time takes the scan first.
    pub(super) fn gap_proc_complete(&mut self, code: u8, status: u8, uart: Uart) {
        if code == gap_proc::GENERAL_DISCOVERY && self.scan.is_some() {
            if status != 0 {
                uart.write_str("[ble] the chip ended the scan, status ");
                uart.write_hex8(status);
                uart.write_line("");
            }
            ended(self.scan.take(), uart);
        }
    }
}

fn ended(scan: Option<(Instant, u32, u32)>, uart: Uart) {
    if let Some((_, printed, heard)) = scan {
        uart.write_str("[ble] scan done: ");
        uart.write_u32(heard);
        uart.write_str(" reports, ");
        uart.write_u32(printed);
        uart.write_line(" advertisers printed");
    }
}

impl Link<'_> {
    /// A command that may be refused while advertising: then advertising stops for it, and
    /// resumes when the scan is over.
    async fn while_advertising(&mut self, opcode: u16, params: &[u8]) -> Result<u8, &'static str> {
        let status = self.command(opcode, params).await?.status;
        if status != COMMAND_DISALLOWED || STATE.load(Ordering::Relaxed) != STATE_ADVERTISING {
            return Ok(status);
        }
        if self.command(ACI_GAP_SET_NON_DISCOVERABLE, &[]).await.is_ok() {
            STATE.store(STATE_IDLE, Ordering::Relaxed);
            self.scanner.paused_advertising = true;
            self.uart.write_line("[ble] advertising paused: the chip will not do both");
        }
        Ok(self.command(opcode, params).await?.status)
    }

    /// `ble scan`: for `seconds`, at `duty` or the power source's.
    pub(super) async fn scan_request(&mut self, seconds: u8, duty: Option<Scan>) {
        if !self.scanner.available {
            return self.uart.write_line("[ble] the central role is not available on this chip (see the start-up log)");
        }
        if self.scanner.scan.is_some() {
            return self.uart.write_line("[ble] a scan is already running");
        }
        self.scanner.seen = [None; SCAN_SEEN];
        self.scanner.seen_next = 0;
        let s = duty.unwrap_or(scan(power()));
        let mut p = [0u8; 6];
        p[0..2].copy_from_slice(&s.interval.to_le_bytes());
        p[2..4].copy_from_slice(&s.window.to_le_bytes());
        // The chip filtering duplicates.
        p[4] = address::STATIC_RANDOM;
        p[5] = 0x01;
        match self.while_advertising(ACI_GAP_START_GENERAL_DISCOVERY_PROC, &p).await {
            Ok(0) => {
                self.scanner.scan = Some((Instant::now() + Duration::from_secs(u64::from(seconds)), 0, 0));
                self.uart.write_str("[ble] scanning for ");
                self.uart.write_u32(u32::from(seconds));
                self.uart.write_str(" s, listening ");
                self.uart.write_u32(u32::from(s.window) * 100 / u32::from(s.interval));
                self.uart.write_str("% of the time (");
                self.uart.write_u32(u32::from(s.window) * 5 / 8);
                self.uart.write_str(" ms in every ");
                self.uart.write_u32(u32::from(s.interval) * 5 / 8);
                self.uart.write_line(" ms)");
            }
            Ok(status) => {
                self.uart.write_str("[ble] scan refused, status ");
                self.uart.write_hex8(status);
                self.uart.write_line("");
                self.resume_advertising();
            }
            Err(why) => {
                self.uart.write_str("[ble] scan: ");
                self.uart.write_line(why);
                self.resume_advertising();
            }
        }
    }

    fn resume_advertising(&mut self) {
        if core::mem::take(&mut self.scanner.paused_advertising) && STATE.load(Ordering::Relaxed) == STATE_IDLE {
            self.readvertise = true;
        }
    }

    /// A scan past its time is ended, and advertising paused for it resumes.
    pub(super) async fn scan_tick(&mut self) {
        if self.scanner.scan.is_some_and(|(until, ..)| Instant::now() >= until) {
            let scan = self.scanner.scan.take();
            let _ = self.command(ACI_GAP_TERMINATE_GAP_PROC, &[gap_proc::GENERAL_DISCOVERY]).await;
            ended(scan, self.uart);
        }
        if self.scanner.scan.is_none() {
            self.resume_advertising();
        }
    }
}
