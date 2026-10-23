//! `usblog`: `embassy-usb`'s own log in a `usb-log` build, and this image's notes about the link,
//! in a RAM ring.

use core::cell::RefCell;
#[cfg(feature = "usb-log")]
use core::fmt::Write;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Instant;

use crate::uart::Uart;

const SIZE: usize = 8192;

struct Ring {
    buf: [u8; SIZE],
    head: usize,
    wrapped: bool,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; SIZE],
            head: 0,
            wrapped: false,
        }
    }
    fn push(&mut self, b: u8) {
        self.buf[self.head] = b;
        self.head += 1;
        if self.head == SIZE {
            self.head = 0;
            self.wrapped = true;
        }
    }
    fn push_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            self.push(b);
        }
    }
    fn push_decimal(&mut self, mut n: u64) {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            #[allow(clippy::cast_possible_truncation)]
            let d = (n % 10) as u8;
            digits[i] = b'0' + d;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for &b in &digits[i..] {
            self.push(b);
        }
    }
}

#[cfg(feature = "usb-log")]
impl Write for Ring {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            self.push(b);
        }
        Ok(())
    }
}

static RING: Mutex<CriticalSectionRawMutex, RefCell<Ring>> = Mutex::new(RefCell::new(Ring::new()));

#[cfg(feature = "usb-log")]
struct Line {
    buf: [u8; 192],
    len: usize,
}

#[cfg(feature = "usb-log")]
impl Line {
    const fn new() -> Self {
        Self {
            buf: [0; 192],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

#[cfg(feature = "usb-log")]
impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.buf.len() - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

#[cfg(feature = "usb-log")]
struct Sink;

#[cfg(feature = "usb-log")]
impl log::Log for Sink {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let t = metadata.target();
        t.starts_with("embassy_usb") && !t.contains("class")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Class requests flood the ring, so they are dropped.
        let mut line = Line::new();
        let _ = write!(line, "{}: {}", record.level(), record.args());
        if line.as_str().contains("request_type: Class") {
            return;
        }
        let ms = Instant::now().as_millis();
        RING.lock(|ring| {
            let mut ring = ring.borrow_mut();
            let _ = writeln!(ring, "{ms}ms {}", line.as_str());
        });
    }

    fn flush(&self) {}
}

#[cfg(feature = "usb-log")]
static SINK: Sink = Sink;

pub fn note(what: &str, value: u32) {
    let ms = Instant::now().as_millis();
    RING.lock(|ring| {
        let mut ring = ring.borrow_mut();
        ring.push_decimal(ms);
        ring.push_str("ms NOTE: ");
        ring.push_str(what);
        ring.push(b' ');
        ring.push_decimal(u64::from(value));
        ring.push(b'\n');
    });
}

/// Call once, after the time driver starts and before the USB device is built.
pub fn install() {
    #[cfg(feature = "usb-log")]
    {
        let _ = log::set_logger(&SINK);
        log::set_max_level(log::LevelFilter::Trace);
    }
}

pub async fn dump(uart: Uart) {
    const PIECE: usize = 256;
    if cfg!(feature = "usb-log") {
        uart.write_line("[usblog] embassy-usb's own account and this image's notes, oldest first:");
    } else {
        uart.write_line("[usblog] this image's notes only (embassy-usb's own lines need a `usb-log` build), oldest first:");
    }
    let (start, len) = RING.lock(|ring| {
        let ring = ring.borrow();
        if ring.wrapped {
            (ring.head, SIZE)
        } else {
            (0, ring.head)
        }
    });
    let mut done = 0;
    while done < len {
        let n = (len - done).min(PIECE);
        let mut piece = [0u8; PIECE];
        RING.lock(|ring| {
            let ring = ring.borrow();
            for (i, b) in piece[..n].iter_mut().enumerate() {
                *b = ring.buf[(start + done + i) % SIZE];
            }
        });
        uart.write_bytes(&piece[..n]);
        done += n;
        crate::console::make_room().await;
    }
    uart.write_line("[usblog] end");
}
