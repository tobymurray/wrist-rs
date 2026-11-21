//! The flight recorder: every packet to and from the BlueNRG-2 and every other input the core is
//! given, kept in a RAM ring that `ble dump` reads back as `[trace]` lines the host replays; with
//! `ble-trace`, each also goes to the USB host as it happens.

use core::cell::RefCell;
use core::fmt::Write;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Instant;
use wrist_bluenrg2::shadow::record::{self, Next, Record, Ring};

use crate::uart::Uart;

/// The ring's size: on battery, an hour or more of a quiet link.
const RING: usize = 32 * 1024;

static RECORDS: Mutex<CriticalSectionRawMutex, RefCell<Ring<RING>>> = Mutex::new(RefCell::new(Ring::NEW));

#[derive(Clone, Copy)]
pub(super) enum Dir {
    ToChip,
    FromChip,
    Reset,
    Mode,
    Lost,
    Snapshot,
    Goals,
    Timing,
    Radio,
    Answer,
    IntervalNow,
    Disconnect,
}

impl Dir {
    fn kind(self) -> u8 {
        match self {
            Dir::ToChip => record::COMMAND,
            Dir::FromChip => record::EVENT,
            Dir::Reset => record::RESET,
            Dir::Mode => record::MODE,
            Dir::Lost => record::LOST,
            Dir::Snapshot => record::SNAPSHOT,
            Dir::Goals => record::GOALS,
            Dir::Timing => record::TIMING,
            Dir::Radio => record::RADIO,
            Dir::Answer => record::ANSWER,
            Dir::IntervalNow => record::INTERVAL_NOW,
            Dir::Disconnect => record::DISCONNECT,
        }
    }
}

/// The driver's snapshot as [`wrist_bluenrg2::shadow::trace`] carries it; empty when it failed.
pub(super) fn snapshot(s: Option<&wrist_ble::Snapshot>) {
    let mut bytes = [0; wrist_bluenrg2::shadow::trace::SNAPSHOT_MAX];
    let len = s.map_or(0, |s| wrist_bluenrg2::shadow::trace::encode(s, &mut bytes));
    packet(Dir::Snapshot, &bytes[..len]);
}

/// The goals the core is given.
pub(super) fn goals(g: &wrist_ble::policy::goals::Goals) {
    let mut bytes = [0; 5];
    let len = record::encode_goals(g, &mut bytes);
    packet(Dir::Goals, &bytes[..len]);
}

pub(super) fn packet(dir: Dir, p: &[u8]) {
    let ms = Instant::now().as_millis();
    RECORDS.lock(|r| r.borrow_mut().push(ms, dir.kind(), p));
    #[cfg(feature = "ble-trace")]
    {
        // USB only: mirrored over the air, each line would make the packets that trace the next.
        let mut one = Ring::<{ 8 + record::KEPT }>::NEW;
        one.push(ms, dir.kind(), p);
        if let Next::Record(r, _) = one.next(0) {
            let mut line = Line::new();
            let _ = r.write(&mut line);
            line.push(b"\r\n");
            crate::console::usb_only(line.bytes());
        }
    }
}

/// `ble dump [since <n>]`: the records from position `n` (0, the oldest kept), at the mirror's
/// pace, unrecorded themselves, then `[ble] dump next <n>`.
pub async fn dump(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    let since = match text.strip_prefix("since") {
        _ if text.is_empty() => Some(0),
        Some(n) => n.trim().parse::<u64>().ok(),
        None => None,
    };
    let Some(mut at) = since else {
        return uart.write_line("[ble] usage: ble dump [since <n>]");
    };
    let end = RECORDS.lock(|r| r.borrow().end());
    if at > end {
        at = 0;
    }
    let mut lost = RECORDS.lock(|r| {
        let r = r.borrow();
        let from = r.align(at);
        let lost = r.align(0).saturating_sub(at);
        at = from;
        lost
    });
    while at < end {
        let next = RECORDS.lock(|r| r.borrow().next(at));
        match next {
            Next::Record(r, after) => {
                write_record(uart, &r).await;
                at = after;
            }
            Next::Lost(from) => {
                lost += from - at;
                at = from;
            }
            Next::End => break,
        }
    }
    let mut line = Line::new();
    let _ = write!(line, "[ble] dump next {at}");
    if lost > 0 {
        let _ = write!(line, ", {lost} bytes lost");
    }
    line.push(b"\r\n");
    crate::journal::replay(uart, line.len, || uart.write_bytes(line.bytes())).await;
}

async fn write_record(uart: Uart, r: &Record) {
    let mut line = Line::new();
    let _ = r.write(&mut line);
    line.push(b"\r\n");
    crate::journal::replay(uart, line.len, || uart.write_bytes(line.bytes())).await;
    uart.pace().await;
}

struct Line {
    buf: [u8; 128],
    len: usize,
}

impl Line {
    fn new() -> Self {
        Self { buf: [0; 128], len: 0 }
    }

    fn push(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}
