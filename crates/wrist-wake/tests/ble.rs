//! The BLE loop's deadlines: idle costs no wake, and everything it does by time has one.

use wrist_wake::ble::{due, timing, Console, Power, State, Transfer};
use wrist_wake::earliest;

const NOW: u64 = 1_000;

fn idle() -> State {
    State {
        now: NOW,
        work: false,
        confirm_by: None,
        transfer: Transfer::None,
        console: None,
        abandon_at: None,
        ask_at: None,
    }
}

#[test]
fn earliest_ignores_the_missing() {
    assert_eq!(earliest([None, None]), None);
    assert_eq!(earliest([None, Some(5), Some(3), None]), Some(3));
    assert_eq!(earliest::<[Option<u64>; 0]>([]), None);
}

#[test]
fn idle_and_advertising_waits_for_events_only() {
    assert_eq!(due(&idle()), None);
    // No console subscriber, or nothing to send: still nothing.
    let s = State { console: Some(Console { pending: false, next_at: 0 }), ..idle() };
    assert_eq!(due(&s), None);
}

#[test]
fn work_left_by_an_event_runs_on_the_next_pass() {
    let s = State { work: true, confirm_by: Some(61_000), ..idle() };
    assert_eq!(due(&s), Some(NOW));
}

#[test]
fn a_comparison_is_rejected_at_its_timeout() {
    let s = State { confirm_by: Some(61_000), ..idle() };
    assert_eq!(due(&s), Some(61_000));
}

#[test]
fn console_output_is_paced_and_quiet_during_a_transfer() {
    let c = Console { pending: true, next_at: 1_010 };
    assert_eq!(due(&State { console: Some(c), ..idle() }), Some(1_010));
    let late = Console { pending: true, next_at: 500 };
    assert_eq!(due(&State { console: Some(late), ..idle() }), Some(NOW), "never in the past");
    let s = State { console: Some(c), transfer: Transfer::Started, ..idle() };
    assert_eq!(due(&s), None, "the mirror waits while image bytes arrive");
    let s = State { console: Some(c), transfer: Transfer::Claimed, ..idle() };
    assert_eq!(due(&s), Some(1_010));
}

#[test]
fn the_earliest_wins() {
    let s = State { confirm_by: Some(61_000), console: Some(Console { pending: true, next_at: 1_010 }), ..idle() };
    assert_eq!(due(&s), Some(1_010));
}

#[test]
fn a_quiet_transfer_is_abandoned_on_time() {
    let s = State { transfer: Transfer::Started, abandon_at: Some(6_000), ..idle() };
    assert_eq!(due(&s), Some(6_000));
}

#[test]
fn every_timing_is_inside_the_specification() {
    for power in [Power::Cable, Power::Battery] {
        for transfer in [false, true] {
            let t = timing(power, transfer);
            assert!((32..=16_384).contains(&t.advertise), "{t:?}");
            assert!((6..=t.interval_max).contains(&t.interval_min) && t.interval_max <= 3_200, "{t:?}");
            assert!(t.latency <= 499 && (10..=3_200).contains(&t.timeout), "{t:?}");
            // Core 6.B 4.5.2: the timeout must exceed (1 + latency) x interval x 2.
            let least_ms = (1 + u32::from(t.latency)) * u32::from(t.interval_max) * 125 / 100 * 2;
            assert!(u32::from(t.timeout) * 10 > least_ms, "{t:?}");
        }
    }
}

#[test]
fn battery_is_slow_until_a_transfer_and_a_cable_is_fast() {
    let idle = timing(Power::Battery, false);
    let cable = timing(Power::Cable, false);
    assert!(idle.advertise >= 1_600, "about a second on battery");
    // An idle link wakes the radio at most about once a second.
    assert!(u32::from(idle.interval_min) * 125 / 100 * (1 + u32::from(idle.latency)) >= 500);
    assert_eq!(cable, timing(Power::Battery, true), "a transfer is as fast as a cable");
    assert_eq!(cable, timing(Power::Cable, true));
    assert!(cable.advertise <= 160 && cable.interval_max <= 24 && cable.latency == 0);
}

#[test]
fn interval_now_keeps_to_its_supervision_timeout() {
    use wrist_wake::ble::{interval_now, supervised, NOW_LATENCY, NOW_TIMEOUT};
    assert!(interval_now(6, 6) && interval_now(12, 12) && interval_now(6, 1_999));
    // 2000 x 1.25 ms x 2 is the 5 s timeout itself.
    assert!(!interval_now(6, 2_000) && !interval_now(2_400, 3_200));
    assert!(!interval_now(5, 12) && !interval_now(24, 12));
    for max in 6..=3_200 {
        assert_eq!(interval_now(6, max), supervised(max, NOW_LATENCY, NOW_TIMEOUT), "{max}");
    }
}

#[test]
fn a_start_that_never_advertised_failed() {
    use wrist_wake::ble::Radio;
    assert!(Radio::Starting.start_failed() && Radio::Failed.start_failed());
    for up in [Radio::Idle, Radio::Advertising, Radio::Connected] {
        assert!(!up.start_failed(), "{up:?}");
    }
    for r in [Radio::Off, Radio::Starting, Radio::Idle, Radio::Advertising, Radio::Connected, Radio::Failed] {
        assert_eq!(Radio::from_u8(r as u8), r);
    }
}

#[test]
fn a_chunk_refused_for_want_of_buffers_goes_again_first() {
    use wrist_wake::ble::Outbox;
    let mut out = Outbox::<8>::default();
    let (chunk, n) = out.next(4, |b| {
        b.copy_from_slice(b"pong");
        4
    });
    assert_eq!(out.sent(chunk, n, 0x64), Ok(()));
    assert!(out.holding());
    let (again, m) = out.next(4, |_| panic!("the held chunk goes first"));
    assert_eq!(&again[..m], b"pong");
    assert_eq!(out.sent(again, m, 0), Ok(()));
    assert!(!out.holding());
    assert_eq!(out.next(4, |_| 0).1, 0);
}

#[test]
fn any_other_refusal_loses_the_chunk_and_a_detach_drops_the_held_one() {
    use wrist_wake::ble::Outbox;
    let mut out = Outbox::<8>::default();
    let (chunk, n) = out.next(8, |b| {
        b[..2].copy_from_slice(b"ok");
        2
    });
    assert_eq!(out.sent(chunk, n, 0x41), Err(0x41));
    assert!(!out.holding());
    out.sent(chunk, n, 0x64).unwrap();
    out.clear();
    assert_eq!(out.next(8, |_| 0).1, 0);
}

#[test]
fn nothing_is_asked_of_a_central_at_connect() {
    use wrist_wake::ble::{PairAsk, ASK_TO_PAIR_AFTER_MS};
    let mut ask = PairAsk::default();
    ask.connected(NOW, 1_000);
    assert!(!ask.take(NOW), "a request at connect crossed a bonded central's encryption");
    assert!(!ask.take(NOW + 1_999));
    assert_eq!(ask.due(), Some(NOW + ASK_TO_PAIR_AFTER_MS));
    assert_eq!(due(&State { ask_at: ask.due(), ..idle() }), Some(NOW + ASK_TO_PAIR_AFTER_MS));
    assert!(ask.take(NOW + ASK_TO_PAIR_AFTER_MS));
    assert!(!ask.take(NOW + ASK_TO_PAIR_AFTER_MS + 1), "once a link");
    assert_eq!(ask.due(), None);
}

#[test]
fn an_encrypted_or_pairing_link_is_never_asked() {
    use wrist_wake::ble::{PairAsk, ASK_TO_PAIR_AFTER_MS};
    let mut ask = PairAsk::default();
    ask.connected(NOW, 32_768);
    ask.settled();
    assert_eq!(ask.due(), None);
    assert!(!ask.take(NOW + ASK_TO_PAIR_AFTER_MS * 33));
    // The next link starts afresh, on the image's own tick.
    ask.connected(NOW, 32_768);
    assert_eq!(ask.due(), Some(NOW + ASK_TO_PAIR_AFTER_MS * 32_768 / 1_000));
}

/// embassy-sync's `Pipe` as the mirror uses it: a write fills only the contiguous span up
/// to the buffer's end, so it can be short even with room after the wrap.
struct Ring<const N: usize> {
    buf: [u8; N],
    start: usize,
    end: usize,
    full: bool,
}

impl<const N: usize> Ring<N> {
    fn new() -> Self {
        Self { buf: [0; N], start: 0, end: 0, full: false }
    }

    fn len(&self) -> usize {
        match (self.full, self.start <= self.end) {
            (true, _) => N,
            (false, true) => self.end - self.start,
            (false, false) => N - self.start + self.end,
        }
    }

    fn read(&mut self, most: usize) -> Vec<u8> {
        let n = self.len().min(most);
        let out = (0..n).map(|i| self.buf[(self.start + i) % N]).collect();
        self.start = (self.start + n) % N;
        self.full &= n == 0;
        out
    }
}

impl<const N: usize> wrist_wake::ble::Sink for Ring<N> {
    fn free(&self) -> usize {
        N - self.len()
    }

    fn write(&mut self, bytes: &[u8]) -> usize {
        if self.full {
            return 0;
        }
        let span = if self.start <= self.end { N - self.end } else { self.start - self.end };
        let n = span.min(bytes.len());
        self.buf[self.end..self.end + n].copy_from_slice(&bytes[..n]);
        self.end = (self.end + n) % N;
        self.full = n > 0 && self.start == self.end;
        n
    }
}

/// A heartbeat line as the image writes it: in pieces.
fn heartbeat(lines: &mut wrist_wake::ble::Lines<64>, ring: &mut Ring<64>, beat: u32) -> u32 {
    let mut lost = 0;
    for piece in ["[hb] ", &beat.to_string(), "  uptime ", &(beat * 2).to_string(), " s", "\r\n"] {
        lost += lines.feed(piece.as_bytes(), ring);
    }
    lost
}

#[test]
fn a_mirrored_line_crosses_the_pipes_end_whole() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_wake::ble::Lines::<64>::default();
    ring.end = 60;
    ring.start = 60;
    assert_eq!(heartbeat(&mut lines, &mut ring, 214), 0);
    assert_eq!(ring.read(64), b"[hb] 214  uptime 428 s\r\n");
}

#[test]
fn a_full_pipe_drops_whole_lines_and_counts_them() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_wake::ble::Lines::<64>::default();
    let mut read = Vec::new();
    let mut lost = 0;
    for beat in 0..200 {
        lost += heartbeat(&mut lines, &mut ring, beat);
        if beat % 7 == 0 {
            read.extend(ring.read(20));
        }
    }
    read.extend(ring.read(64));
    let text = String::from_utf8(read).unwrap();
    let got: Vec<&str> = text.split_terminator("\r\n").collect();
    assert!(lost > 0, "the pipe never filled");
    assert_eq!(got.len() + lost as usize, 200);
    let mut previous = None;
    for line in got {
        let beat: u32 = line.strip_prefix("[hb] ").and_then(|l| l.split(' ').next()).unwrap().parse().unwrap();
        assert_eq!(line, format!("[hb] {beat}  uptime {} s", beat * 2), "spliced");
        assert!(previous < Some(beat));
        previous = Some(beat);
    }
}

#[test]
fn a_line_longer_than_the_holder_cut_short_ends_before_the_next() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_wake::ble::Lines::<16>::default();
    let long = [b'x'; 100];
    assert_eq!(lines.feed(&long, &mut ring), 1);
    assert_eq!(lines.feed(b"yy\r\n", &mut ring), 0);
    assert_eq!(ring.read(64), [b'x'; 64]);
    assert_eq!(lines.feed(b"next\r\n", &mut ring), 0);
    assert_eq!(ring.read(64), b"\r\nnext\r\n");
}

#[test]
fn a_detach_forgets_the_line_in_hand() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_wake::ble::Lines::<64>::default();
    assert_eq!(lines.feed(b"[hb] 3", &mut ring), 0);
    lines.clear();
    assert_eq!(lines.feed(b"[ble] console attached\r\n", &mut ring), 0);
    assert_eq!(ring.read(64), b"[ble] console attached\r\n");
}

#[test]
fn a_line_whose_only_refused_piece_is_its_end_is_not_counted_lost() {
    let mut ring = Ring::<300>::new();
    let mut lines = wrist_wake::ble::Lines::<256>::default();
    ring.end = 44;
    let body = [b'x'; 256];
    assert_eq!(lines.feed(&body, &mut ring) + lines.feed(b"\r\n", &mut ring), 0);
    ring.read(300);
    assert_eq!(lines.feed(b"next\r\n", &mut ring), 0);
    assert_eq!(ring.read(300), b"\r\nnext\r\n");
    // An empty line refused outright is lost.
    ring.full = true;
    assert_eq!(lines.feed(b"\r\n", &mut ring), 1);
}
