//! The SPI handshake to the BlueNRG-2, after ST's X-CUBE-BLE2 `hci_tl_interface.c`.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_futures::select::{select, Either};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiBus;

/// The most one read takes; the chip announces every queued event in one count.
pub const MAX_READ: usize = 1024;
/// The most the chip's own buffers hold; a header naming more is unreadable.
const MAX_ROOM: usize = 1024;
/// The chip's ready for a write, after CS falls.
pub const READY_WITHIN_MS: u32 = 100;
/// IRQ's fall after a transfer.
pub const IRQ_LOW_WITHIN_MS: u32 = 1_000;
/// Between attempts at a write the chip had no room for, and after an empty read.
pub const RETRY_MS: u32 = 10;
pub const WRITE_ATTEMPTS: u32 = 5;
/// Unreadable headers in a row before the transport says so and stops reading packets.
pub const UNREADABLE_RUN: u32 = 3;

/// What the transport counts, readable from other tasks.
#[derive(Debug, Default)]
pub struct Counts {
    /// Reads that returned bytes.
    pub reads: AtomicU32,
    /// Packets whose own length ran past the end of their read, or that started with no packet
    /// header, dropped with what followed.
    pub cut_short: AtomicU32,
    /// Reads and writes after which IRQ did not fall within [`IRQ_LOW_WITHIN_MS`].
    pub irq_stuck_read: AtomicU32,
    pub irq_stuck_write: AtomicU32,
}

impl Counts {
    #[must_use]
    pub const fn new() -> Self {
        Self { reads: AtomicU32::new(0), cut_short: AtomicU32::new(0), irq_stuck_read: AtomicU32::new(0), irq_stuck_write: AtomicU32::new(0) }
    }
}

fn bump(c: &AtomicU32) {
    c.fetch_add(1, Ordering::Relaxed);
}

/// One read's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// This many bytes, the start of the buffer; see [`packets`].
    Bytes(usize),
    /// The chip had nothing, and the transport waited [`RETRY_MS`].
    Empty,
    /// [`UNREADABLE_RUN`] or more unreadable headers in a row; `first` on the one that reached it.
    Unreadable { header: [u8; 5], first: bool },
}

pub struct Transport<'c, B, C, I, D> {
    bus: B,
    cs: C,
    irq: I,
    delay: D,
    counts: &'c Counts,
    unreadable: u32,
}

impl<'c, B, C, I, D> Transport<'c, B, C, I, D>
where
    B: SpiBus,
    C: OutputPin,
    I: InputPin + Wait,
    D: DelayNs,
{
    pub fn new(bus: B, cs: C, irq: I, delay: D, counts: &'c Counts) -> Self {
        Self { bus, cs, irq, delay, counts, unreadable: 0 }
    }

    /// The bus, chip select and IRQ line back.
    pub fn release(self) -> (B, C, I) {
        (self.bus, self.cs, self.irq)
    }

    #[must_use]
    pub fn counts(&self) -> &'c Counts {
        self.counts
    }

    /// A read is waiting.
    pub fn pending(&mut self) -> bool {
        self.irq.is_high().unwrap_or(false)
    }

    /// Until a read is waiting.
    pub async fn wait_pending(&mut self) {
        let _ = self.irq.wait_for_high().await;
    }

    /// Whether a read became waiting within `us`.
    pub async fn wait_pending_within(&mut self, us: u32) -> bool {
        matches!(select(self.irq.wait_for_high(), self.delay.delay_us(us)).await, Either::First(_))
    }

    /// CS high, as a reset of the chip wants it.
    pub fn deselect(&mut self) {
        let _ = self.cs.set_high();
    }

    /// The last reads' headers were unreadable: the caller reads no faster than it must.
    #[must_use]
    pub fn backing_off(&self) -> bool {
        self.unreadable >= UNREADABLE_RUN
    }

    /// Whether the chip took `packet` within [`WRITE_ATTEMPTS`]; one longer than
    /// [`bluenrg2::MAX_COMMAND`] is refused unsent.
    pub async fn write(&mut self, packet: &[u8]) -> bool {
        if packet.len() > bluenrg2::MAX_COMMAND {
            return false;
        }
        for _ in 0..WRITE_ATTEMPTS {
            let _ = self.cs.set_low();
            if !within(&mut self.delay, READY_WITHIN_MS, self.irq.wait_for_high()).await {
                let _ = self.cs.set_high();
                self.delay.delay_ms(RETRY_MS).await;
                continue;
            }
            let mut header = bluenrg2::HEADER_WRITE;
            let _ = self.bus.transfer_in_place(&mut header).await;
            let (room, _) = bluenrg2::header_sizes(&header);
            // The BlueNRG-2 sends no ready byte, only its room; ST checks the room alone.
            let taken = (1..=MAX_ROOM).contains(&room) && room >= packet.len();
            if taken {
                let mut out = [0u8; bluenrg2::MAX_COMMAND];
                let out = &mut out[..packet.len()];
                out.copy_from_slice(packet);
                let _ = self.bus.transfer_in_place(out).await;
            }
            let _ = self.cs.set_high();
            if taken {
                if !within(&mut self.delay, IRQ_LOW_WITHIN_MS, self.irq.wait_for_low()).await {
                    bump(&self.counts.irq_stuck_write);
                }
                return true;
            }
            self.delay.delay_ms(RETRY_MS).await;
        }
        false
    }

    /// One read into `buf`, of what the chip announces, at most `buf`'s length.
    pub async fn read(&mut self, buf: &mut [u8]) -> Read {
        let _ = self.cs.set_low();
        let mut header = bluenrg2::HEADER_READ;
        let _ = self.bus.transfer_in_place(&mut header).await;
        let (room, announced) = bluenrg2::header_sizes(&header);
        let count = announced.min(buf.len());
        if count > 0 {
            buf[..count].fill(0);
            let _ = self.bus.transfer_in_place(&mut buf[..count]).await;
        }
        if !within(&mut self.delay, IRQ_LOW_WITHIN_MS, self.irq.wait_for_low()).await {
            bump(&self.counts.irq_stuck_read);
        }
        let _ = self.cs.set_high();
        if (1..=MAX_ROOM).contains(&room) {
            self.unreadable = 0;
        } else {
            self.unreadable += 1;
            if self.backing_off() {
                return Read::Unreadable { header, first: self.unreadable == UNREADABLE_RUN };
            }
        }
        if count == 0 {
            self.delay.delay_ms(RETRY_MS).await;
            return Read::Empty;
        }
        bump(&self.counts.reads);
        Read::Bytes(count)
    }
}

/// Whether `f` finished within `ms`.
async fn within<F: core::future::Future>(delay: &mut impl DelayNs, ms: u32, f: F) -> bool {
    matches!(select(f, delay.delay_ms(ms)).await, Either::First(_))
}

/// The whole packets in one read, in order; one cut short by the read's end, or bytes that start
/// no packet, end them, counted.
pub fn packets<'a>(read: &'a [u8], counts: &'a Counts) -> impl Iterator<Item = &'a [u8]> + 'a {
    let mut at = 0;
    core::iter::from_fn(move || {
        let rest = &read[at..];
        if rest.is_empty() {
            return None;
        }
        let Some(packet) = bluenrg2::packet_len(rest).and_then(|len| rest.get(..len)) else {
            bump(&counts.cut_short);
            at = read.len();
            return None;
        };
        at += packet.len();
        Some(packet)
    })
}
