//! The SPI handshake against a scripted BlueNRG-2 on a virtual clock.

mod common;

use std::future::Future;
use std::sync::atomic::Ordering;
use std::task::Context;

use common::{chip, event, Bus, Clock, Cs, Irq};
use embassy_futures::block_on;
use wrist_bluenrg2::transport::{packets, Counts, Read, Transport, MAX_READ};

fn counts(t: &Transport<'static, Bus, Cs, Irq, Clock>) -> &'static Counts {
    t.counts()
}

const COMMAND: [u8; 4] = [0x01, 0x01, 0x10, 0x00];


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

#[test]
fn a_packet_longer_than_a_command_is_refused_unsent() {
    let (c, mut t) = chip(1024);
    assert!(!block_on(t.write(&[0x01; bluenrg2::MAX_COMMAND + 1])));
    assert!(c.borrow().spans.is_empty(), "nothing clocked");
    assert!(block_on(t.write(&[0x01; bluenrg2::MAX_COMMAND])), "the longest command goes");
}
