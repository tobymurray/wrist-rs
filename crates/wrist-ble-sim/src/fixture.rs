//! A `ble-trace` capture cut to what the replay reads, with no address and no console text.

use std::fmt::Write as _;

use bluenrg2::event_code::{COMMAND_COMPLETE, COMMAND_STATUS, LE_META, VENDOR};
use bluenrg2::le_subevent::{ADVERTISING_REPORT, CONNECTION_COMPLETE, ENHANCED_CONNECTION_COMPLETE};
use bluenrg2::packet::EVENT;
use bluenrg2::vendor::GATT_ATTRIBUTE_MODIFIED;
use wrist_ble::Instant;
use wrist_bluenrg2::shadow::{is_op, trace as snapshot};

use crate::capture::{command, scrub_addresses, stamped, trace};

/// The capture's lines a fixture keeps, each as it keeps it.
#[must_use]
pub fn fixture(capture: &str) -> String {
    capture.lines().filter_map(keep).fold(String::new(), |mut out, line| {
        let _ = writeln!(out, "{line}");
        out
    })
}

/// `line` as a fixture keeps it: chip resets, whether the core drives, the driver's lost reads,
/// snapshots and operations, the chip's events less what [`event`] drops, the console commands
/// the replay acts on, and scenario marks and `[update]` lines with their addresses replaced.
#[must_use]
pub fn keep(line: &str) -> Option<String> {
    // A `ble dump` line has no host stamp: the watch's own milliseconds stand in for it.
    let dumped;
    let (stamp, text) = if let Some(l) = stamped(line) {
        (l.stamp, l.text.trim())
    } else {
        let t = trace(line.trim())?;
        dumped = format!("{}.{:03}", t.ms / 1000, t.ms % 1000);
        (dumped.as_str(), line.trim())
    };
    if text.starts_with("== ") || text.starts_with("[update] ") {
        return Some(format!("{stamp} {}", scrub_addresses(text)));
    }
    if let Some(cmd) = text.strip_prefix(">> usb ") {
        return command(cmd.trim()).map(|c| format!("{stamp} >> usb {c}"));
    }
    let t = trace(text)?;
    if matches!(t.kind, "r" | "l") || (t.kind == "s" && t.packet.is_none()) {
        return Some(format!("{stamp} [trace] {} {}", t.ms, t.kind));
    }
    // The recorder's goals, timing, radio, answer, ask, hang-up and selftest: no address in any.
    if matches!(t.kind, "G" | "g" | "o" | "a" | "i" | "d" | "x" | "f" | "y" | "t") {
        return Some(format!("{stamp} {text}"));
    }
    let packet = t.packet?;
    let packet = match t.kind {
        "c" => packet
            .get(1..3)
            .is_some_and(|op| is_op(u16::from_le_bytes([op[0], op[1]])))
            .then_some(packet),
        "e" => event(packet),
        "s" => snapshot::decode(Instant(0), &packet).is_some().then_some(packet),
        "m" => matches!(packet[..], [0 | 1]).then_some(packet),
        _ => None,
    }?;
    let mut out = format!("{stamp} [trace] {} {} ", t.ms, t.kind);
    for b in packet {
        let _ = write!(out, "{b:02X}");
    }
    Some(out)
}

/// The chip's event `p` as a fixture keeps it: a reply to an operation cut after its status, a
/// connection's peer and resolvable addresses zeroed; `None` for an advertising report, a console
/// write or a reply to another command.
fn event(mut p: Vec<u8>) -> Option<Vec<u8>> {
    if p.len() < 3 || p[0] != EVENT {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
    match p[1] {
        COMMAND_COMPLETE | COMMAND_STATUS => {
            // Complete: ncmd, opcode, status; Status: status, ncmd, opcode.
            if p.len() < 7 || !is_op(if p[1] == COMMAND_COMPLETE { u16_at(4) } else { u16_at(5) }) {
                return None;
            }
            p.truncate(7);
            p[2] = 4;
        }
        VENDOR if p.len() >= 5 && u16_at(3) == GATT_ATTRIBUTE_MODIFIED => return None,
        LE_META if p.len() >= 4 => {
            // After the subevent: status, handle, role and the peer's kind, then its address and,
            // in the enhanced event, the local and peer resolvable addresses.
            let addresses = match p[3] {
                ADVERTISING_REPORT => return None,
                CONNECTION_COMPLETE => 9..15,
                ENHANCED_CONNECTION_COMPLETE => 9..27,
                _ => 0..0,
            };
            let end = addresses.end.min(p.len());
            p.get_mut(addresses.start..end).into_iter().flatten().for_each(|b| *b = 0);
        }
        _ => {}
    }
    Some(p)
}
