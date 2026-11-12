//! A scripted BlueNRG-2 on a virtual clock: the far end of the bus, its pins and its reset.

#![allow(clippy::unused_async_trait_impl, reason = "the fake chip answers at once; the traits are async")]
#![allow(dead_code, reason = "each test file uses part of it")]

use std::cell::RefCell;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{self, SpiBus};
use wrist_bluenrg2::driver::{self, Driver};
use wrist_bluenrg2::transport::{Counts, Transport};

/// What the chip does on the other end of the bus.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is one line of the script")]
pub struct Chip {
    /// Ticks of the virtual clock, a microsecond each unless a test runs it at another rate.
    pub now: u64,
    pub cs_low: bool,
    /// Bytes clocked in the current CS-low span, and every span so far.
    pub span: Vec<u8>,
    pub spans: Vec<Vec<u8>>,
    /// The room the chip reports for writes.
    pub room: u16,
    /// The chip never raises IRQ for a write.
    pub never_ready: bool,
    /// IRQ stays high after a transfer.
    pub stuck: bool,
    /// What the next reads hand over, each announced whole.
    pub queued: Vec<Vec<u8>>,
    /// The next read's header, in place of one built from `room` and `queued`.
    pub header: Option<[u8; 5]>,
    /// The phase of a span: `None` before its header, then the header's first byte.
    pub phase: Option<u8>,
    pub done: bool,
    /// Every header from here on, once `header` is spent.
    pub sticky_header: Option<[u8; 5]>,
    /// The reset pin's levels as the host set them, with the time.
    pub resets: Vec<(u64, bool)>,
    /// What the chip hands over first once let out of reset, its queue gone.
    pub boot: Option<Vec<u8>>,
}

impl Chip {
    fn irq(&self) -> bool {
        if self.cs_low {
            match self.phase {
                None => !self.never_ready,
                Some(_) => !self.done || self.stuck,
            }
        } else {
            !self.queued.is_empty() || self.stuck
        }
    }
}

pub type Shared = Rc<RefCell<Chip>>;

pub struct Bus(pub Shared);
pub struct Cs(pub Shared);
pub struct Irq(pub Shared);
pub struct Rst(pub Shared);
#[derive(Clone)]
pub struct Clock<const HZ: u64 = 1_000_000>(pub Shared);

impl spi::ErrorType for Bus {
    type Error = Infallible;
}

impl SpiBus for Bus {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Infallible> {
        words.fill(0);
        self.transfer_in_place(words).await
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Infallible> {
        self.transfer_in_place(&mut words.to_vec()).await
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Infallible> {
        let mut w = write.to_vec();
        self.transfer_in_place(&mut w).await?;
        read.copy_from_slice(&w[..read.len()]);
        Ok(())
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Infallible> {
        let mut c = self.0.borrow_mut();
        assert!(c.cs_low, "clocked with CS high");
        c.span.extend_from_slice(words);
        match c.phase {
            None => {
                let kind = words[0];
                let waiting: usize = c.queued.first().map_or(0, Vec::len);
                let reply = c.header.take().or(c.sticky_header).unwrap_or({
                    let [r0, r1] = c.room.to_le_bytes();
                    let [w0, w1] = u16::try_from(if kind == 0x0B { waiting } else { 0 }).unwrap().to_le_bytes();
                    [0x02, r0, r1, w0, w1]
                });
                words.copy_from_slice(&reply);
                c.phase = Some(kind);
                c.done = kind == 0x0B && reply[3..] == [0, 0];
            }
            Some(0x0B) => {
                let event = c.queued.remove(0);
                let n = words.len().min(event.len());
                words[..n].copy_from_slice(&event[..n]);
                c.done = true;
            }
            Some(_) => c.done = true,
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}

impl ErrorType for Cs {
    type Error = Infallible;
}

impl OutputPin for Cs {
    fn set_low(&mut self) -> Result<(), Infallible> {
        let mut c = self.0.borrow_mut();
        c.cs_low = true;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        let mut c = self.0.borrow_mut();
        if c.cs_low {
            let span = std::mem::take(&mut c.span);
            c.spans.push(span);
        }
        c.cs_low = false;
        c.phase = None;
        c.done = false;
        Ok(())
    }
}

impl ErrorType for Irq {
    type Error = Infallible;
}

impl InputPin for Irq {
    fn is_high(&mut self) -> Result<bool, Infallible> {
        Ok(self.0.borrow().irq())
    }

    fn is_low(&mut self) -> Result<bool, Infallible> {
        Ok(!self.0.borrow().irq())
    }
}

/// Ready while the chip holds IRQ at `level`; otherwise never, as nothing moves the chip but
/// the host.
struct Level(Shared, bool);

impl Future for Level {
    type Output = Result<(), Infallible>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0.borrow().irq() == self.1 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

impl Wait for Irq {
    async fn wait_for_high(&mut self) -> Result<(), Infallible> {
        Level(self.0.clone(), true).await
    }

    async fn wait_for_low(&mut self) -> Result<(), Infallible> {
        Level(self.0.clone(), false).await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Infallible> {
        unreachable!()
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Infallible> {
        unreachable!()
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Infallible> {
        unreachable!()
    }
}

impl<const HZ: u64> DelayNs for Clock<HZ> {
    async fn delay_ns(&mut self, ns: u32) {
        self.0.borrow_mut().now += (u64::from(ns) * HZ).div_ceil(1_000_000_000);
    }
}

pub fn chip(room: u16) -> (Shared, Transport<'static, Bus, Cs, Irq, Clock>) {
    let shared = Rc::new(RefCell::new(Chip { room, ..Chip::default() }));
    let counts: &'static Counts = Box::leak(Box::new(Counts::new()));
    let t = Transport::new(Bus(shared.clone()), Cs(shared.clone()), Irq(shared.clone()), Clock(shared.clone()), counts);
    (shared, t)
}

pub fn event(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, code, u8::try_from(params.len()).unwrap()];
    p.extend(params);
    p
}

impl ErrorType for Rst {
    type Error = Infallible;
}

impl OutputPin for Rst {
    fn set_low(&mut self) -> Result<(), Infallible> {
        let mut c = self.0.borrow_mut();
        let now = c.now;
        c.resets.push((now, false));
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        let mut c = self.0.borrow_mut();
        let now = c.now;
        let held = c.resets.last().is_some_and(|&(_, high)| !high);
        c.resets.push((now, true));
        if held {
            c.queued.clear();
            let boot = c.boot.clone();
            c.queued.extend(boot);
        }
        Ok(())
    }
}

impl<const HZ: u64> driver::Clock for Clock<HZ> {
    const HZ: u64 = HZ;

    fn now(&self) -> u64 {
        self.0.borrow().now
    }
}

pub type TestDriver<const HZ: u64 = 1_000_000> = Driver<'static, Bus, Cs, Irq, Clock<HZ>, Rst, Clock<HZ>>;

pub fn driver(room: u16) -> (Shared, TestDriver) {
    driver_at(room)
}

/// A driver on a clock of `HZ` ticks a second, every wait rounded up to whole ticks.
pub fn driver_at<const HZ: u64>(room: u16) -> (Shared, TestDriver<HZ>) {
    let shared = Rc::new(RefCell::new(Chip { room, ..Chip::default() }));
    let counts: &'static Counts = Box::leak(Box::new(Counts::new()));
    let d = Driver::new(Bus(shared.clone()), Cs(shared.clone()), Irq(shared.clone()), Clock(shared.clone()), counts, Rst(shared.clone()), Clock(shared.clone()));
    (shared, d)
}
