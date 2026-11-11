//! The SPI handshake against a scripted BlueNRG-2 on a virtual clock.

#![allow(clippy::unused_async_trait_impl, reason = "the fake chip answers at once; the traits are async")]

use std::cell::RefCell;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use embassy_futures::block_on;
use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{self, SpiBus};
use wrist_bluenrg2::transport::{packets, Counts, Read, Transport, MAX_READ};

/// What the chip does on the other end of the bus.
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is one line of the script")]
struct Chip {
    /// Microseconds on the virtual clock.
    now: u64,
    cs_low: bool,
    /// Bytes clocked in the current CS-low span, and every span so far.
    span: Vec<u8>,
    spans: Vec<Vec<u8>>,
    /// The room the chip reports for writes.
    room: u16,
    /// The chip never raises IRQ for a write.
    never_ready: bool,
    /// IRQ stays high after a transfer.
    stuck: bool,
    /// What the next reads hand over, each announced whole.
    queued: Vec<Vec<u8>>,
    /// The next read's header, in place of one built from `room` and `queued`.
    header: Option<[u8; 5]>,
    /// The phase of a span: `None` before its header, then the header's first byte.
    phase: Option<u8>,
    done: bool,
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

type Shared = Rc<RefCell<Chip>>;

struct Bus(Shared);
struct Cs(Shared);
struct Irq(Shared);
struct Clock(Shared);

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
                let reply = c.header.take().unwrap_or({
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

impl DelayNs for Clock {
    async fn delay_ns(&mut self, ns: u32) {
        self.0.borrow_mut().now += u64::from(ns) / 1_000;
    }
}

fn chip(room: u16) -> (Shared, Transport<'static, Bus, Cs, Irq, Clock>) {
    let shared = Rc::new(RefCell::new(Chip { room, ..Chip::default() }));
    let counts: &'static Counts = Box::leak(Box::new(Counts::new()));
    let t = Transport::new(Bus(shared.clone()), Cs(shared.clone()), Irq(shared.clone()), Clock(shared.clone()), counts);
    (shared, t)
}

fn counts(t: &Transport<'static, Bus, Cs, Irq, Clock>) -> &'static Counts {
    t.counts()
}

const COMMAND: [u8; 4] = [0x01, 0x01, 0x10, 0x00];

fn event(code: u8, params: &[u8]) -> Vec<u8> {
    let mut p = vec![0x04, code, u8::try_from(params.len()).unwrap()];
    p.extend(params);
    p
}

#[test]
fn a_command_follows_the_write_header_once_the_chip_has_room() {
    let (c, mut t) = chip(128);
    assert!(block_on(t.write(&COMMAND)));
    let c = c.borrow();
    assert_eq!(c.spans, vec![[&[0x0A, 0, 0, 0, 0][..], &COMMAND].concat()]);
    assert!(!c.cs_low);
    assert_eq!(c.now, 0);
    assert_eq!(counts(&t).irq_stuck_write.load(Ordering::Relaxed), 0);
}

#[test]
fn a_chip_with_too_little_room_is_asked_five_times_then_given_up() {
    let (c, mut t) = chip(3);
    assert!(!block_on(t.write(&COMMAND)));
    let c = c.borrow();
    assert_eq!(c.spans, vec![vec![0x0A, 0, 0, 0, 0]; 5]);
    assert_eq!(c.now, 5 * 10_000);
}

#[test]
fn room_the_chip_cannot_have_is_no_room() {
    for room in [0, 1025] {
        let (c, mut t) = chip(room);
        assert!(!block_on(t.write(&[0x01])), "room {room}");
        assert!(c.borrow().spans.iter().all(|s| s.len() == 5));
    }
    let (_, mut t) = chip(1024);
    assert!(block_on(t.write(&[0x01])));
    let (_, mut t) = chip(4);
    assert!(block_on(t.write(&COMMAND)), "a packet the room holds exactly");
}

#[test]
fn a_chip_never_ready_is_tried_five_times_with_cs_raised_each_time() {
    let (c, mut t) = chip(128);
    c.borrow_mut().never_ready = true;
    assert!(!block_on(t.write(&COMMAND)));
    let c = c.borrow();
    assert_eq!(c.spans, vec![Vec::<u8>::new(); 5], "nothing clocked");
    assert_eq!(c.now, 5 * (100_000 + 10_000));
}

#[test]
fn irq_stuck_after_a_write_is_counted_and_the_write_stands() {
    let (c, mut t) = chip(128);
    c.borrow_mut().stuck = true;
    assert!(block_on(t.write(&COMMAND)));
    assert_eq!(counts(&t).irq_stuck_write.load(Ordering::Relaxed), 1);
    assert_eq!(c.borrow().now, 1_000_000);
}

#[test]
fn a_read_takes_what_the_header_announces_and_splits_it_into_packets() {
    let (c, mut t) = chip(128);
    let (first, second) = (event(0x05, &[0, 1, 8, 0x13]), event(0x0E, &[1, 1, 0x10, 0]));
    c.borrow_mut().queued.push([first.clone(), second.clone()].concat());
    assert!(t.pending());
    let mut buf = [0xAA; MAX_READ];
    let Read::Bytes(n) = block_on(t.read(&mut buf)) else { panic!() };
    assert_eq!(n, first.len() + second.len());
    let got: Vec<&[u8]> = packets(&buf[..n], counts(&t)).take(3).collect();
    assert_eq!(got, vec![&first[..], &second[..]]);
    let c = c.borrow();
    assert_eq!(c.spans[0][..5], [0x0B, 0, 0, 0, 0]);
    assert!(c.spans[0][5..].iter().all(|&b| b == 0), "zeros clocked out");
    assert_eq!(counts(&t).reads.load(Ordering::Relaxed), 1);
    assert!(!t.pending());
}

#[test]
fn a_read_announcing_more_than_the_buffer_takes_the_buffer() {
    let (c, mut t) = chip(128);
    c.borrow_mut().queued.push(vec![0x04; 40]);
    let mut buf = [0; 16];
    assert_eq!(block_on(t.read(&mut buf)), Read::Bytes(16));
    assert_eq!(c.borrow().spans[0].len(), 5 + 16);
}

#[test]
fn a_packet_cut_short_is_counted_and_drops_what_follows() {
    let counts = Counts::new();
    let whole = event(0x05, &[0, 1, 8, 0x13]);
    let read = [&whole[..], &[0x04, 0xFF, 10, 1, 2], &whole[..]].concat();
    let got: Vec<&[u8]> = packets(&read, &counts).take(3).collect();
    assert_eq!(got, vec![&whole[..]]);
    assert_eq!(counts.cut_short.load(Ordering::Relaxed), 1);
    assert_eq!(packets(&[], &counts).count(), 0);
    assert_eq!(counts.cut_short.load(Ordering::Relaxed), 1);
}

#[test]
fn a_header_too_short_for_its_length_or_bytes_starting_no_packet_end_the_read_counted() {
    let whole = event(0x05, &[0, 1, 8, 0x13]);
    for tail in [&[0x04, 0x0E][..], &[0x02, 0x01, 0x20, 0x02], &[0x00, 0x04, 0x05, 0x00, 0x04, 0x05, 0x04, 0x00, 0x01, 0x08, 0x13]] {
        let counts = Counts::new();
        let read = [&whole[..], tail].concat();
        assert_eq!(packets(&read, &counts).take(3).collect::<Vec<_>>(), vec![&whole[..]], "{tail:02X?}");
        assert_eq!(counts.cut_short.load(Ordering::Relaxed), 1, "{tail:02X?}");
    }
}

#[test]
fn an_empty_read_waits_before_the_next() {
    let (c, mut t) = chip(128);
    c.borrow_mut().queued.push(Vec::new());
    assert_eq!(block_on(t.read(&mut [0; 8])), Read::Empty);
    assert_eq!(c.borrow().now, 10_000);
    assert_eq!(counts(&t).reads.load(Ordering::Relaxed), 0);
}

#[test]
fn irq_stuck_after_a_read_is_counted() {
    let (c, mut t) = chip(128);
    c.borrow_mut().queued.push(event(0x05, &[0, 1, 8, 0x13]));
    c.borrow_mut().stuck = true;
    assert!(matches!(block_on(t.read(&mut [0; 64])), Read::Bytes(7)));
    assert_eq!(counts(&t).irq_stuck_read.load(Ordering::Relaxed), 1);
}

#[test]
fn three_unreadable_headers_in_a_row_are_reported_once_and_a_good_one_ends_them() {
    let (c, mut t) = chip(128);
    let bad = [0x00, 0x00, 0x00, 0x00, 0x00];
    let mut seen = Vec::new();
    for _ in 0..4 {
        c.borrow_mut().header = Some(bad);
        seen.push(block_on(t.read(&mut [0; 8])));
    }
    assert_eq!(&seen[..2], &[Read::Empty, Read::Empty], "the first two read as nothing waiting");
    assert_eq!(seen[2], Read::Unreadable { header: bad, first: true });
    assert_eq!(seen[3], Read::Unreadable { header: bad, first: false });
    assert!(t.backing_off());
    assert_eq!(c.borrow().now, 2 * 10_000, "no wait once backing off");
    block_on(t.read(&mut [0; 8]));
    assert!(!t.backing_off());
}

#[test]
fn an_unreadable_header_still_hands_over_its_bytes_before_the_run_is_reached() {
    let (c, mut t) = chip(128);
    c.borrow_mut().queued.push(vec![0x04, 0x05, 0x00]);
    c.borrow_mut().header = Some([0x02, 0x00, 0x00, 3, 0]);
    assert_eq!(block_on(t.read(&mut [0; 8])), Read::Bytes(3));
    assert!(!t.backing_off());
}

#[test]
fn waiting_for_a_read_gives_up_at_its_time() {
    let (c, mut t) = chip(128);
    assert!(!block_on(t.wait_pending_within(3_000_000)));
    assert_eq!(c.borrow().now, 3_000_000);
    c.borrow_mut().queued.push(vec![0x04]);
    assert!(block_on(t.wait_pending_within(3_000_000)));
    assert_eq!(c.borrow().now, 3_000_000);
}

#[test]
fn waiting_for_a_read_lasts_until_one_is_waiting() {
    let (c, mut t) = chip(128);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    {
        let mut wait = std::pin::pin!(t.wait_pending());
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        c.borrow_mut().queued.push(vec![0x04]);
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }
    assert!(t.pending());
}

#[test]
fn a_reset_finds_cs_high() {
    let (c, mut t) = chip(128);
    c.borrow_mut().cs_low = true;
    t.deselect();
    assert!(!c.borrow().cs_low);
}
