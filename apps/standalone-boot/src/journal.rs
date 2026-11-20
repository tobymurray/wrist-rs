//! The console's output kept in RAM for a central that was away (`log since <n>`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Instant;
use wrist_logic::console::Log;

use crate::uart::Uart;

const SIZE: usize = 32 * 1024;

static LOG: Mutex<CriticalSectionRawMutex, RefCell<Log<SIZE>>> = Mutex::new(RefCell::new(Log::new()));

/// The time driver runs: lines before it are stamped 0.
static CLOCK: AtomicBool = AtomicBool::new(false);

pub fn clock_started() {
    CLOCK.store(true, Ordering::Relaxed);
}

pub fn record(bytes: &[u8]) {
    let now = if CLOCK.load(Ordering::Relaxed) { Instant::now().as_millis() } else { 0 };
    LOG.lock(|l| {
        if let Ok(mut l) = l.try_borrow_mut() {
            l.record(now, bytes);
        }
    });
}

/// `log [since <n>]`: what was kept from `n` to the last line end, or from the start when `n` is
/// past the end, then `[log] next <end>`.
pub async fn command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    let asked = match text.strip_prefix("since") {
        _ if text.is_empty() => Some(0),
        Some(n) => n.trim().parse::<u64>().ok(),
        None => None,
    };
    let Some(asked) = asked else {
        return uart.write_line("[log] usage: log [since <n>]");
    };
    let end = LOG.lock(|l| l.borrow().line_end());
    let mut at = if asked > end { 0 } else { asked };
    let mut dropped = 0;
    dropped += replay(uart, HEADER, || {
        let mut l = uart.lines();
        l.write_str("[log] since ");
        write_u64(&mut l, asked);
        l.write_line(if asked > end { ": past the end, so from the start (the watch restarted)" } else { "" });
    })
    .await;
    let mut lost = 0;
    let mut piece = [0u8; 256];
    let mut mid_line = false;
    while at < end {
        let r = LOG.lock(|l| l.borrow().read(at, end, &mut piece));
        lost += r.from.saturating_sub(at);
        let next = r.from + r.len as u64;
        if next <= at {
            break;
        }
        // The rest of a line goes at once, so that no other output lands inside it.
        dropped += replay(uart, if mid_line { 0 } else { r.len }, || uart.write_bytes(&piece[..r.len])).await;
        mid_line = piece[..r.len].last() != Some(&b'\n');
        at = next;
        if !mid_line {
            uart.pace().await;
        }
    }
    let ms = Instant::now().as_millis();
    replay(uart, HEADER, || {
        let mut l = uart.lines();
        l.write_str("[log] next ");
        write_u64(&mut l, end);
        l.write_str(" at ");
        write_u64(&mut l, ms / 1000);
        l.write_str(".");
        let frac = u32::try_from(ms % 1000).unwrap_or(0);
        l.write_str(if frac < 10 { "00" } else if frac < 100 { "0" } else { "" });
        l.write_u32(frac);
        if lost > 0 {
            l.write_str(", ");
            write_u64(&mut l, lost);
            l.write_str(" bytes lost");
        }
        if dropped > 0 {
            l.write_str(", ");
            l.write_u32(dropped);
            l.write_str(" lines dropped on the way");
        }
        l.write_line("");
    })
    .await;
}

/// Room asked of the mirror for the replay's own lines.
const HEADER: usize = 128;

/// Writes part of the replay once the mirror has room for `len` bytes, or at once for 0; how many
/// lines it dropped.
#[cfg_attr(not(feature = "ble"), allow(clippy::unused_async))]
async fn replay(uart: Uart, len: usize, write: impl FnOnce()) -> u32 {
    #[cfg(feature = "ble")]
    if len > 0 {
        crate::ble::room(len).await;
    }
    #[cfg(not(feature = "ble"))]
    let _ = len;
    let before = mirror_dropped();
    uart.replay(write);
    mirror_dropped().wrapping_sub(before)
}

fn mirror_dropped() -> u32 {
    #[cfg(feature = "ble")]
    {
        crate::ble::lines_dropped()
    }
    #[cfg(not(feature = "ble"))]
    {
        0
    }
}

fn write_u64(l: &mut crate::uart::Lines, v: u64) {
    if let Ok(v) = u32::try_from(v) {
        return l.write_u32(v);
    }
    write_u64(l, v / 1_000_000_000);
    let low = u32::try_from(v % 1_000_000_000).unwrap_or(0);
    let mut pad = 100_000_000;
    while pad > 1 && low < pad {
        l.write_str("0");
        pad /= 10;
    }
    l.write_u32(low);
}
