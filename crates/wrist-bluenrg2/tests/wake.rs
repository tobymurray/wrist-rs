//! The BLE loop's deadlines: idle costs no wake, and everything it does by time has one.

use wrist_bluenrg2::wake::ble::{due, scan, timing, Console, Power, State, Transfer};
use wrist_bluenrg2::driver::Unanswered;
use wrist_bluenrg2::wake::ble::{Outbox, LOST, RESENDS};
use wrist_bluenrg2::wake::earliest;

const NOW: u64 = 1_000;

fn idle() -> State {
    State {
        now: NOW,
        work: false,
        confirm_by: None,
        transfer: Transfer::None,
        console: None,
        abandon_at: None,
        scan_end: None,
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
    use wrist_bluenrg2::wake::ble::{interval_now, supervised, NOW_LATENCY, NOW_TIMEOUT};
    assert!(interval_now(6, 6) && interval_now(12, 12) && interval_now(6, 1_999));
    // 2000 x 1.25 ms x 2 is the 5 s timeout itself.
    assert!(!interval_now(6, 2_000) && !interval_now(2_400, 3_200));
    assert!(!interval_now(5, 12) && !interval_now(24, 12));
    for max in 6..=3_200 {
        assert_eq!(interval_now(6, max), supervised(max, NOW_LATENCY, NOW_TIMEOUT), "{max}");
    }
}

#[test]
fn the_radio_survives_its_atomic() {
    use wrist_bluenrg2::wake::ble::Radio;
    for r in [Radio::Off, Radio::Starting, Radio::Idle, Radio::Advertising, Radio::Connected, Radio::Failed] {
        assert_eq!(Radio::from_u8(r as u8), r);
    }
}

/// `text` as the next chunk, of at most `most` bytes, after anything held or owed.
fn next_with(out: &mut Outbox<8>, most: usize, text: &[u8]) -> Vec<u8> {
    out.next(most, |b| {
        let n = text.len().min(b.len());
        b[..n].copy_from_slice(&text[..n]);
        n
    })
    .to_vec()
}

const BUFFERS: Result<u8, Unanswered> = Ok(0x64);
const SENT: Result<u8, Unanswered> = Ok(0);

#[test]
fn a_chunk_refused_for_want_of_buffers_goes_again_first() {
    let mut out = Outbox::<8>::default();
    assert_eq!(next_with(&mut out, 4, b"pong"), b"pong");
    for _ in 0..=RESENDS + 1 {
        assert!(!out.sent(BUFFERS));
        assert!(out.pending());
        assert_eq!(out.next(4, |_| panic!("the held chunk goes first")), b"pong");
    }
    assert!(!out.sent(SENT));
    assert!(!out.pending());
    assert_eq!(out.next(4, |_| 0), b"");
}

#[test]
fn a_definite_refusal_sends_the_chunk_again() {
    let refusals = [Ok(0x0C), Ok(0x41), Ok(0x46), Ok(0x47), Err(Unanswered::NotTaken), Err(Unanswered::Restarted), Err(Unanswered::TooLong)];
    for refused in refusals {
        let mut out = Outbox::<8>::default();
        assert_eq!(next_with(&mut out, 8, b"ok\r\n"), b"ok\r\n");
        assert!(!out.sent(refused), "{refused:?}");
        assert!(out.pending());
        assert_eq!(out.next(8, |_| panic!("the refused chunk goes first")), b"ok\r\n");
        assert!(!out.sent(SENT));
        assert!(!out.pending());
    }
}

#[test]
fn a_chunk_refused_past_its_resends_is_marked_lost() {
    let mut out = Outbox::<64>::default();
    out.next(64, |b| {
        b[..6].copy_from_slice(b"[hb] 1");
        6
    });
    for _ in 0..RESENDS {
        assert!(!out.sent(Ok(0x46)));
        // Want of buffers is not counted against it.
        out.next(64, |_| panic!("held"));
        assert!(!out.sent(BUFFERS));
        out.next(64, |_| panic!("held"));
    }
    assert!(out.sent(Ok(0x46)));
    assert!(out.pending());
    // It never went, so the central is not inside a line.
    let got = out.next(64, |b| {
        b[..4].copy_from_slice(b"x\r\n_");
        4
    });
    assert_eq!(got, [&LOST[2..], b"x\r\n_"].concat());
}

/// A chunk of at most `most` from the pipe, answered `answer`; what the central has, if it
/// went.
fn notify<const N: usize>(out: &mut Outbox<8>, ring: &mut Ring<N>, most: usize, answer: Result<u8, Unanswered>) -> Vec<u8> {
    let chunk = out
        .next(most, |b| {
            let r = ring.read(b.len());
            b[..r.len()].copy_from_slice(&r);
            r.len()
        })
        .to_vec();
    let lost = out.sent(answer);
    assert_eq!(lost, answer == Err(Unanswered::NoReply));
    chunk
}

/// What the central reads as the outbox empties the pipe.
fn drain<const N: usize>(out: &mut Outbox<8>, ring: &mut Ring<N>) -> Vec<u8> {
    let mut central = Vec::new();
    for _ in 0..=N {
        if !out.pending() && ring.len() == 0 {
            return central;
        }
        central.extend(notify(out, ring, 8, SENT));
    }
    panic!("the outbox never emptied");
}

#[test]
fn a_chunk_that_may_have_gone_is_followed_by_a_line_saying_so() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    let mut out = Outbox::<8>::default();
    assert_eq!(heartbeat(&mut lines, &mut ring, 97), 0);
    let mut central = notify(&mut out, &mut ring, 8, SENT);
    // No reply: " uptime " may or may not have reached it.
    notify(&mut out, &mut ring, 8, Err(Unanswered::NoReply));
    assert!(out.pending());
    central.extend(drain(&mut out, &mut ring));
    assert_eq!(central, [&b"[hb] 97 "[..], LOST, b"194 s\r\n"].concat());
}

#[test]
fn a_whole_line_that_may_have_gone_gets_the_marker_and_no_blank_line() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    let mut out = Outbox::<8>::default();
    assert_eq!(lines.feed(b"[hb] 1\r\nnext\r\n", &mut ring), 0);
    notify(&mut out, &mut ring, 8, Err(Unanswered::NoReply));
    assert_eq!(drain(&mut out, &mut ring), [&LOST[2..], b"next\r\n"].concat());
}

#[test]
fn a_clear_while_a_chunk_is_in_flight_counts_that_chunk() {
    // Whether the central had the chunk across a detach and re-attach.
    for (answer, lead) in [(SENT, true), (Err(Unanswered::NoReply), true), (BUFFERS, false), (Ok(0x46), false)] {
        let mut ring = Ring::<64>::new();
        let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
        let mut out = Outbox::<8>::default();
        assert_eq!(lines.feed(b"ab\r\n[hb] 2 uptime\r\n", &mut ring), 0);
        let mut central = notify(&mut out, &mut ring, 4, SENT);
        let chunk = out.next(4, |b| {
            let r = ring.read(b.len());
            b[..r.len()].copy_from_slice(&r);
            r.len()
        });
        assert_eq!(chunk, b"[hb]");
        out.clear();
        ring.clear();
        lines.clear();
        assert_eq!(lines.feed(b"[ble] console attached\r\n", &mut ring), 0);
        assert!(!out.sent(answer), "{answer:?}");
        assert_eq!(out.pending(), lead, "{answer:?}: the stale chunk is not held");
        if answer == SENT {
            central.extend(b"[hb]");
        }
        central.extend(drain(&mut out, &mut ring));
        let expected: &[u8] = match (answer == SENT, lead) {
            (true, _) => b"ab\r\n[hb]\r\n[ble] console attached\r\n",
            (false, true) => b"ab\r\n\r\n[ble] console attached\r\n",
            (false, false) => b"ab\r\n[ble] console attached\r\n",
        };
        assert_eq!(central, expected, "{answer:?}");
    }
}

#[test]
fn a_whole_line_in_flight_at_a_clear_needs_no_new_line() {
    let mut out = Outbox::<8>::default();
    assert_eq!(next_with(&mut out, 8, b"[hb] 1\r\n"), b"[hb] 1\r\n");
    out.clear();
    assert!(!out.sent(SENT));
    assert!(!out.pending());
    assert_eq!(next_with(&mut out, 8, b"x"), b"x");
}

#[test]
fn a_line_ended_in_flight_at_a_clear_needs_no_new_line_after_a_cut_one() {
    let mut out = Outbox::<8>::default();
    assert_eq!(next_with(&mut out, 4, b"[hb] 1\r\n"), b"[hb]");
    assert!(!out.sent(SENT));
    assert_eq!(next_with(&mut out, 8, b" 1\r\n"), b" 1\r\n");
    out.clear();
    assert!(!out.sent(SENT));
    assert!(!out.pending());
}

#[test]
fn an_answer_with_nothing_in_flight_changes_nothing() {
    let mut out = Outbox::<8>::default();
    assert_eq!(out.next(8, |_| 0), b"");
    assert!(!out.sent(Err(Unanswered::NoReply)));
    assert!(!out.sent(Ok(0x46)));
    assert!(!out.pending());
    // A link's outbox replaced while a chunk was in flight.
    let mut out = Outbox::<8>::default();
    assert_eq!(next_with(&mut out, 8, b"[hb]"), b"[hb]");
    out = Outbox::default();
    assert!(!out.sent(BUFFERS));
    assert!(!out.pending());
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

    fn clear(&mut self) {
        self.start = self.end;
        self.full = false;
    }

    fn read(&mut self, most: usize) -> Vec<u8> {
        let n = self.len().min(most);
        let out = (0..n).map(|i| self.buf[(self.start + i) % N]).collect();
        self.start = (self.start + n) % N;
        self.full &= n == 0;
        out
    }
}

impl<const N: usize> wrist_bluenrg2::wake::ble::Sink for Ring<N> {
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
fn heartbeat(lines: &mut wrist_bluenrg2::wake::ble::Lines<64>, ring: &mut Ring<64>, beat: u32) -> u32 {
    let mut lost = 0;
    for piece in ["[hb] ", &beat.to_string(), "  uptime ", &(beat * 2).to_string(), " s", "\r\n"] {
        lost += lines.feed(piece.as_bytes(), ring);
    }
    lost
}

#[test]
fn a_mirrored_line_crosses_the_pipes_end_whole() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    ring.end = 60;
    ring.start = 60;
    assert_eq!(heartbeat(&mut lines, &mut ring, 214), 0);
    assert_eq!(ring.read(64), b"[hb] 214  uptime 428 s\r\n");
}

#[test]
fn a_full_pipe_drops_whole_lines_and_counts_them() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
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
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<16>::default();
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
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    assert_eq!(lines.feed(b"[hb] 3", &mut ring), 0);
    lines.clear();
    assert_eq!(lines.feed(b"[ble] console attached\r\n", &mut ring), 0);
    assert_eq!(ring.read(64), b"[ble] console attached\r\n");
}

#[test]
fn a_line_whose_only_refused_piece_is_its_end_is_not_counted_lost() {
    let mut ring = Ring::<300>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<256>::default();
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

#[test]
fn a_line_half_sent_when_the_pipe_is_cleared_ends_before_the_next() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    let mut out = Outbox::<8>::default();
    assert_eq!(heartbeat(&mut lines, &mut ring, 97), 0);
    let mut central = notify(&mut out, &mut ring, 8, SENT);
    // An update finishes: what was queued is stale.
    ring.clear();
    lines.clear();
    out.clear();
    assert_eq!(lines.feed(b"[update] ok\r\n", &mut ring), 0);
    central.extend(drain(&mut out, &mut ring));
    assert_eq!(central, b"[hb] 97 \r\n[update] ok\r\n");
}

#[test]
fn a_held_chunk_is_dropped_at_a_clear_and_a_whole_line_gets_no_blank_one() {
    let mut ring = Ring::<64>::new();
    let mut lines = wrist_bluenrg2::wake::ble::Lines::<64>::default();
    let mut out = Outbox::<8>::default();
    assert_eq!(lines.feed(b"[hb] 1\r\nabcdefghij\r\n", &mut ring), 0);
    let mut central = notify(&mut out, &mut ring, 8, SENT);
    // Refused for want of buffers, held, and never sent.
    notify(&mut out, &mut ring, 8, BUFFERS);
    ring.clear();
    lines.clear();
    out.clear();
    assert_eq!(lines.feed(b"[update] ok\r\n", &mut ring), 0);
    central.extend(drain(&mut out, &mut ring));
    assert_eq!(central, b"[hb] 1\r\n[update] ok\r\n");
}

#[test]
fn a_scan_wakes_the_loop_when_it_ends() {
    let s = State { scan_end: Some(NOW + 10_000), ..idle() };
    assert_eq!(due(&s), Some(NOW + 10_000));
    let s = State { scan_end: Some(NOW + 10_000), abandon_at: Some(NOW + 5_000), ..idle() };
    assert_eq!(due(&s), Some(NOW + 5_000));
}

#[test]
fn scanning_listens_less_on_battery() {
    for power in [Power::Cable, Power::Battery] {
        let s = scan(power);
        // Core Specification limits: 2.5 ms to 10.24 s, window no longer than interval.
        assert!((4..=0x4000).contains(&s.interval) && (4..=s.interval).contains(&s.window));
    }
    let duty = |s: wrist_bluenrg2::wake::ble::Scan| u32::from(s.window) * 100 / u32::from(s.interval);
    assert_eq!(duty(scan(Power::Cable)), 60);
    assert_eq!(duty(scan(Power::Battery)), 20);
    // About one window per advertising period: the scan interval within 10% of advertising's.
    for power in [Power::Cable, Power::Battery] {
        let (scan, adv) = (u32::from(scan(power).interval), u32::from(timing(power, false).advertise));
        assert!(scan * 10 >= adv * 9 && scan * 10 <= adv * 11, "{power:?}");
    }
}
