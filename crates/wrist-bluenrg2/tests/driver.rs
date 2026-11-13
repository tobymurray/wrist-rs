//! A command's exchange and a reset's boot against a scripted BlueNRG-2 on a virtual clock.

mod common;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use bluenrg2::BootReason;
use common::{driver, driver_at, event, Shared, TestDriver};
use embassy_futures::block_on;
use wrist_ble::{ConnHandle, RadioEvent, ResetCause, Status};
use wrist_bluenrg2::driver::{Got, Reply, Unanswered, BOOT_WITHIN_US, DRAIN_READS, RESET_MS, REPLY_WITHIN_US};
use wrist_bluenrg2::gatt::Handles;
use wrist_bluenrg2::transport::MAX_READ;

const OPCODE: u16 = 0xFC81;

/// Command Complete for `opcode`.
fn complete(opcode: u16, status: u8, params: &[u8]) -> Vec<u8> {
    event(0x0E, &[&[0x01][..], &opcode.to_le_bytes(), &[status], params].concat())
}

/// `ACI_BLUE_INITIALIZED` with `reason`.
fn boot(reason: u8) -> Vec<u8> {
    event(0xFF, &[0x01, 0x00, reason])
}

/// A link going down: a packet that is neither a reply nor a boot.
fn down(link: u8) -> Vec<u8> {
    event(0x05, &[0x00, link, 0x08, 0x13])
}

fn link_down(link: u8) -> RadioEvent {
    RadioEvent::LinkDown { link: ConnHandle::new(0x0800 | u16::from(link)), reason: Status::new(0x13) }
}

/// What drains handed out, each with its bytes.
type Handed = Vec<(Vec<u8>, Got)>;

fn drained(d: &mut TestDriver) -> Handed {
    let mut out = [0u8; MAX_READ];
    let mut all = Vec::new();
    while let Some(got) = block_on(d.next(&mut out)) {
        let len = match got {
            Got::Packet { len, .. } | Got::Boot { len, .. } => len,
            Got::Unreadable { .. } => 0,
        };
        all.push((out[..len].to_vec(), got));
    }
    all
}

/// A whole exchange as ble.rs runs it, the chip answering with `reads` once it has the command:
/// what the drains handed out meanwhile, and the reply.
fn exchange(c: &Shared, d: &mut TestDriver, reads: Vec<Vec<u8>>) -> (Handed, Result<Reply, Unanswered>) {
    block_on(d.send(OPCODE, &[])).unwrap();
    c.borrow_mut().queued.extend(reads);
    let mut all = Vec::new();
    for _ in 0..100 {
        if !block_on(d.awaiting()) {
            return (all, d.reply());
        }
        all.extend(drained(d));
    }
    panic!("the exchange never ended");
}

fn now(c: &Shared) -> u64 {
    c.borrow().now
}

#[test]
fn a_reply_ends_the_exchange_once_the_drain_that_brought_it_is_over() {
    let (c, mut d) = driver(128);
    let reply = complete(OPCODE, 0x00, &[0xAB, 0xCD]);
    let (got, r) = exchange(&c, &mut d, vec![down(1), [reply.clone(), down(2)].concat(), down(3)]);
    let bytes: Vec<Vec<u8>> = got.iter().map(|(b, _)| b.clone()).collect();
    assert_eq!(bytes, [down(1), reply, down(2), down(3)]);
    assert!(matches!(got[0].1, Got::Packet { event, .. } if event == Some(link_down(1))));
    assert!(matches!(got[1].1, Got::Packet { event: None, .. }), "a reply is no event");
    let r = r.unwrap();
    assert_eq!((r.status, &r.params[..r.len]), (0x00, &[0xAB, 0xCD][..]));
    assert_eq!(drained(&mut d), [], "nothing is drained until asked");
}

#[test]
fn a_command_status_is_a_reply_with_no_parameters() {
    let (c, mut d) = driver(128);
    let r = exchange(&c, &mut d, vec![event(0x0F, &[&[0x12, 0x01][..], &OPCODE.to_le_bytes()].concat())]).1.unwrap();
    assert_eq!((r.status, r.len), (0x12, 0));
}

#[test]
fn another_commands_reply_is_handed_out_and_the_wait_goes_on_to_its_end() {
    let (c, mut d) = driver(128);
    let (got, r) = exchange(&c, &mut d, vec![complete(0xFC83, 0x00, &[])]);
    assert_eq!(got.len(), 1);
    assert_eq!(r, Err(Unanswered::NoReply));
    assert!(now(&c) >= REPLY_WITHIN_US, "{}", now(&c));
}

#[test]
fn another_commands_status_is_handed_out_and_the_wait_goes_on_to_its_end() {
    let (c, mut d) = driver(128);
    let (got, r) = exchange(&c, &mut d, vec![event(0x0F, &[&[0x00, 0x01][..], &0xFC83u16.to_le_bytes()].concat())]);
    assert_eq!(got.len(), 1);
    assert_eq!(r, Err(Unanswered::NoReply));
    assert!(now(&c) >= REPLY_WITHIN_US, "{}", now(&c));
}

#[test]
fn a_chip_that_restarts_ends_the_wait_once_the_rest_of_its_read_is_handed_out() {
    let (c, mut d) = driver(128);
    let (got, r) = exchange(&c, &mut d, vec![[boot(0x05), down(1)].concat()]);
    let events: Vec<Got> = got.iter().map(|(_, g)| *g).collect();
    assert_eq!(events, [Got::Packet { len: 6, event: Some(RadioEvent::ChipReset(ResetCause::Watchdog)) }, Got::Packet { len: 7, event: Some(link_down(1)) }]);
    assert_eq!(r, Err(Unanswered::NoReply));
    assert!(now(&c) < REPLY_WITHIN_US);
}

#[test]
fn a_command_the_chip_does_not_take_or_cannot_be_sent_has_no_exchange() {
    let (c, mut d) = driver(128);
    c.borrow_mut().never_ready = true;
    assert_eq!(block_on(d.send(OPCODE, &[])), Err(Unanswered::NotTaken));
    assert_eq!(d.reply(), Err(Unanswered::NoReply));
    let (c, mut d) = driver(512);
    assert_eq!(block_on(d.send(OPCODE, &[0; 256])), Err(Unanswered::TooLong));
    assert!(c.borrow().spans.is_empty(), "nothing clocked");
}

#[test]
fn a_reply_that_comes_after_the_deadline_is_not_waited_for() {
    let (c, mut d) = driver(128);
    block_on(d.send(OPCODE, &[])).unwrap();
    c.borrow_mut().now += REPLY_WITHIN_US;
    c.borrow_mut().queued.push(complete(OPCODE, 0x00, &[]));
    assert!(!block_on(d.awaiting()));
    assert_eq!(d.reply(), Err(Unanswered::NoReply));
}

#[test]
fn an_unanswered_exchange_ends_on_the_tick_its_second_is_up() {
    for start in 0..64 {
        for handling in 1..48 {
            let (c, mut d) = driver_at::<32_768>(128);
            c.borrow_mut().now = start;
            block_on(d.send(OPCODE, &[])).unwrap();
            c.borrow_mut().queued.push(down(1));
            let mut out = [0u8; MAX_READ];
            for round in 0.. {
                if !block_on(d.awaiting()) {
                    break;
                }
                assert!(round < 2, "the packet's drain, then the wait for the rest of the second");
                while block_on(d.next(&mut out)).is_some() {
                    c.borrow_mut().now += handling;
                }
            }
            assert_eq!(d.reply(), Err(Unanswered::NoReply));
            assert_eq!(now(&c) - start, 32_768, "sent on tick {start}, a packet handled in {handling} ticks");
        }
    }
}

#[test]
fn a_drain_reads_while_irq_stays_high_and_no_more_than_its_limit() {
    let (c, mut d) = driver(128);
    let reads = usize::try_from(DRAIN_READS).unwrap() + 2;
    c.borrow_mut().queued.extend((0..reads).map(|i| down(u8::try_from(i).unwrap())));
    d.drain();
    assert_eq!(drained(&mut d).len(), reads - 2);
    d.drain();
    assert_eq!(drained(&mut d).len(), 2);
    d.drain();
    assert_eq!(drained(&mut d), [], "IRQ low");
}

#[test]
fn an_unreadable_run_ends_the_drain() {
    let (c, mut d) = driver(128);
    c.borrow_mut().sticky_header = Some([0xFF; 5]);
    c.borrow_mut().queued.extend([down(1), down(2), down(3)]);
    d.drain();
    assert!(!d.backing_off());
    let got: Vec<Got> = drained(&mut d).into_iter().map(|(_, g)| g).collect();
    assert!(d.backing_off());
    assert_eq!(got, [Got::Packet { len: 7, event: Some(link_down(1)) }, Got::Packet { len: 7, event: Some(link_down(2)) }, Got::Unreadable { header: [0xFF; 5], first: true }]);
    assert_eq!(drained(&mut d), [], "the drain is over");
}

#[test]
fn a_resets_own_boot_is_kept_back_and_what_was_read_before_it_dropped() {
    let (c, mut d) = driver(128);
    c.borrow_mut().queued.push([down(1), down(2)].concat());
    d.drain();
    let mut out = [0u8; MAX_READ];
    assert!(matches!(block_on(d.next(&mut out)), Some(Got::Packet { .. })), "the first of a read");
    c.borrow_mut().boot = Some([down(3), boot(0x01), down(4)].concat());
    block_on(d.reset());
    assert!(block_on(d.boot()));
    let got: Vec<Got> = drained(&mut d).into_iter().map(|(_, g)| g).collect();
    assert_eq!(got, [Got::Boot { len: 6, reason: Some(BootReason::STARTED) }, Got::Packet { len: 7, event: Some(link_down(4)) }]);
    let pin: Vec<bool> = c.borrow().resets.iter().map(|&(_, high)| high).collect();
    assert_eq!(pin, [false, true]);
    let held = c.borrow().resets[1].0 - c.borrow().resets[0].0;
    assert_eq!(held, u64::from(RESET_MS) * 1_000);
}

#[test]
fn a_boot_after_the_resets_own_is_a_packet() {
    let (c, mut d) = driver(128);
    c.borrow_mut().boot = Some([boot(0x01), boot(0x07)].concat());
    block_on(d.reset());
    assert!(block_on(d.boot()));
    let got: Vec<Got> = drained(&mut d).into_iter().map(|(_, g)| g).collect();
    assert_eq!(got, [Got::Boot { len: 6, reason: Some(BootReason::STARTED) }, Got::Packet { len: 6, event: Some(RadioEvent::ChipReset(ResetCause::Brownout)) }]);
}

#[test]
fn a_reset_that_never_boots_ends_its_window() {
    let (c, mut d) = driver(128);
    block_on(d.reset());
    assert!(!block_on(d.boot()));
    assert!(now(&c) >= u64::from(BOOT_WITHIN_US));
    c.borrow_mut().queued.push(boot(0x01));
    d.drain();
    let got: Vec<Got> = drained(&mut d).into_iter().map(|(_, g)| g).collect();
    assert_eq!(got, [Got::Packet { len: 6, event: Some(RadioEvent::ChipReset(ResetCause::PowerOn)) }], "a late boot is a restart");
}

#[test]
fn a_reset_forgets_the_command_it_cut_off() {
    let (c, mut d) = driver(128);
    block_on(d.send(OPCODE, &[])).unwrap();
    c.borrow_mut().boot = Some(boot(0x01));
    block_on(d.reset());
    c.borrow_mut().queued.push(complete(OPCODE, 0x00, &[]));
    d.drain();
    drained(&mut d);
    assert_eq!(d.reply(), Err(Unanswered::NoReply));
}

#[test]
fn waiting_for_the_chip_lasts_until_it_has_something() {
    let (c, mut d) = driver(128);
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut wait = pin!(d.wait_pending());
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        c.borrow_mut().queued.push(down(1));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Ready(()));
    }
}

#[test]
fn the_server_is_the_one_served_until_the_chip_boots() {
    let (c, mut d) = driver(128);
    assert_eq!(d.server(), None);
    let mut h = Handles::default();
    let tx = wrist_wake::ble::server::CONSOLE.chars[1];
    assert!(h.push(0x0013, 0x0016, &tx));
    d.serve(h);
    assert_eq!(d.server(), Some(&h));
    c.borrow_mut().queued.push(boot(0x05));
    d.drain();
    drained(&mut d);
    assert_eq!(d.server(), None);
}
