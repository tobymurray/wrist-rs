//! `ble-trace`: each packet to and from the BlueNRG-2 as `[trace] <ms> <c|e|r> <hex>` to the USB
//! host, so it can replay the link through the core.

use embassy_time::Instant;

/// Enough of a packet for every header and every event the replay decodes; the rest of a long
/// one is only counted.
const KEPT: usize = 40;

#[derive(Clone, Copy)]
pub(super) enum Dir {
    ToChip,
    FromChip,
    Reset,
}

/// USB only: mirrored over the air, each line would make the packets that trace the next.
pub(super) fn packet(dir: Dir, p: &[u8]) {
    let mut line = Line { buf: [0; 128], len: 0 };
    line.push(b"[trace] ");
    line.decimal(u32::try_from(Instant::now().as_millis()).unwrap_or(u32::MAX));
    line.push(match dir {
        Dir::ToChip => b" c ",
        Dir::FromChip => b" e ",
        Dir::Reset => b" r ",
    });
    for &b in &p[..p.len().min(KEPT)] {
        line.push(&[HEX[usize::from(b >> 4)], HEX[usize::from(b & 0xF)]]);
    }
    if p.len() > KEPT {
        line.push(b" +");
        line.decimal(u32::try_from(p.len() - KEPT).unwrap_or(u32::MAX));
    }
    line.push(b"\r\n");
    crate::console::usb_only(&line.buf[..line.len]);
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

struct Line {
    buf: [u8; 128],
    len: usize,
}

impl Line {
    fn push(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn decimal(&mut self, mut v: u32) {
        let mut digits = [0u8; 10];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.push(&digits[i..]);
    }
}
