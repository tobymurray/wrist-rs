use wrist_logic::console::{room, Held, Log, Read, HEADROOM};

/// Each release as the console's task makes it: pieces up to a line's end.
fn releases<const N: usize>(held: &mut Held<N>, piece: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; piece];
    let mut line = Vec::new();
    for _ in 0..=N {
        let (n, more) = held.take(&mut buf);
        line.extend(&buf[..n]);
        if n == 0 || buf[n - 1] == b'\n' || !more {
            if !line.is_empty() {
                out.push(core::mem::take(&mut line));
            }
            if !more {
                return out;
            }
        }
    }
    panic!("the held output never emptied");
}

#[test]
fn output_comes_out_a_line_at_a_time() {
    let mut held = Held::<64>::new();
    for part in ["[usb] ", "pong\r\n", "[busy] a", " b\r\n", "tail"] {
        assert!(held.hold(part.as_bytes()));
    }
    assert_eq!(releases(&mut held, 128), [&b"[usb] pong\r\n"[..], b"[busy] a b\r\n", b"tail"]);
    assert_eq!(held.take(&mut [0; 8]), (0, false));
}

#[test]
fn a_line_longer_than_a_piece_comes_out_in_pieces() {
    let mut held = Held::<64>::new();
    assert!(held.hold(b"abcdefghij\r\nk\r\n"));
    let mut piece = [0u8; 4];
    let mut got = Vec::new();
    for _ in 0..64 {
        let (n, more) = held.take(&mut piece);
        assert!(n <= 4);
        got.push(piece[..n].to_vec());
        if !more {
            break;
        }
    }
    assert_eq!(got, [&b"abcd"[..], b"efgh", b"ij\r\n", b"k\r\n"]);
}

#[test]
fn what_does_not_fit_is_refused_whole_and_emptying_makes_room() {
    let mut held = Held::<8>::new();
    assert!(held.hold(b"12345\n"));
    assert!(!held.hold(b"abc"));
    assert!(held.hold(b"ab"));
    assert!(!held.hold(b"x"));
    assert_eq!(releases(&mut held, 8), [&b"12345\n"[..], b"ab"]);
    assert!(held.hold(b"12345678"));
    assert!(!held.hold(b"9"));
}

#[test]
fn a_line_still_being_written_is_not_whole() {
    let mut held = Held::<64>::new();
    assert!(!held.whole_line());
    assert!(held.hold(b"[ble] advertising, events 9"));
    assert!(!held.whole_line());
    assert!(held.hold(b", bonded 4\r\n[hb] 1"));
    assert!(held.whole_line());
    let mut piece = [0u8; 64];
    assert_eq!(held.take(&mut piece), (39, true));
    assert!(!held.whole_line());
}

#[test]
fn the_next_line_is_measured_with_its_end() {
    let mut held = Held::<64>::new();
    assert_eq!(held.next_len(), 0);
    assert!(held.hold(b"[usb] pong\r\n[hb] 1"));
    assert_eq!(held.next_len(), 12);
    held.take(&mut [0; 64]);
    assert_eq!(held.next_len(), 6);
}

#[test]
fn a_line_longer_than_the_headroom_allows_still_goes_into_an_empty_sink() {
    assert!(room(4096, 4096, 3000));
    assert!(!room(4095, 4096, 3000));
    assert!(room(HEADROOM + 100, 4096, 100));
    assert!(!room(HEADROOM + 99, 4096, 100));
}

/// A queue a line goes into whole or not at all.
struct Mirror {
    used: usize,
    dropped: usize,
}

impl Mirror {
    const CAPACITY: usize = 4096;

    fn put(&mut self, len: usize) {
        if self.used + len <= Self::CAPACITY {
            self.used += len;
        } else {
            self.dropped += 1;
        }
    }
}

/// About 2.5 KB of a command's output let out a line at a time, through [`room`] when
/// `paced`, over a battery-timed link that carries 220 bytes a connection event, behind a
/// `ble` line already queued; a full power report, which cannot wait, lands an event in.
/// The lines the mirror dropped.
fn long_output_beside_a_report(paced: bool) -> usize {
    let output = [287, 130, 101, 125, 186, 134, 151, 120, 121, 152, 108, 76, 92, 93, 146, 76, 110, 220, 64, 62];
    let report = [27, 48, 51, 18, 30, 51, 66, 121, 128, 76, 63, 84, 103, 69, 69, 91, 69, 154, 37];
    let mut mirror = Mirror { used: 760, dropped: 0 };
    let mut held = Held::<4096>::new();
    for len in output {
        let mut line = vec![b'x'; len - 2];
        line.extend(b"\r\n");
        assert!(held.hold(&line));
    }
    let mut piece = [0u8; 4096];
    for event in 0..100 {
        while held.whole_line() && (!paced || room(Mirror::CAPACITY - mirror.used, Mirror::CAPACITY, held.next_len())) {
            let (n, _) = held.take(&mut piece);
            mirror.put(n);
        }
        if event == 1 {
            for len in report {
                mirror.put(len);
            }
        }
        mirror.used = mirror.used.saturating_sub(220);
        if !held.whole_line() && mirror.used == 0 {
            return mirror.dropped;
        }
    }
    panic!("the output never drained");
}

#[test]
fn long_output_goes_at_the_links_pace_and_leaves_room_for_a_report() {
    assert!(long_output_beside_a_report(false) > 0);
    assert_eq!(long_output_beside_a_report(true), 0);
}

/// Every read from `at` to the end as it stands, in pieces of `piece` bytes, and where the first began.
fn read_all<const N: usize>(log: &Log<N>, at: u64, piece: usize) -> (Vec<u8>, u64) {
    let (mut out, mut buf, mut at, until) = (Vec::new(), vec![0u8; piece], at, log.end());
    let mut first = None;
    for _ in 0..=N {
        if at >= until {
            return (out, first.unwrap_or(at));
        }
        let r = log.read(at, until, &mut buf);
        first.get_or_insert(r.from);
        out.extend(&buf[..r.len]);
        assert!(r.from + r.len as u64 > at, "no progress from {at}");
        at = r.from + r.len as u64;
    }
    panic!("the log never ran out");
}

#[test]
fn each_line_carries_the_uptime_it_began_at() {
    let mut log = Log::<256>::new();
    log.record(5, b"[ble] conn");
    log.record(1_234_567, b"ected\r\n[pwr] 3900 mV\r\n");
    log.record(60_000, b"tail");
    assert_eq!(read_all(&log, 0, 256).0, b"0.005 [ble] connected\n1234.567 [pwr] 3900 mV\n60.000 tail");
    let mut buf = [0u8; 256];
    assert_eq!(log.read(0, log.end(), &mut buf).len as u64, log.end(), "to the end asked, a line in progress too");
}

#[test]
fn a_read_resumes_where_the_last_ended() {
    let mut log = Log::<256>::new();
    log.record(1000, b"a\n");
    let at = log.end();
    log.record(2000, b"b\n");
    assert_eq!(read_all(&log, at, 256).0, b"2.000 b\n");
    assert_eq!(read_all(&log, log.end(), 256).0, b"");
}

#[test]
fn pieces_end_at_a_line_s_end() {
    let mut log = Log::<256>::new();
    log.record(0, b"one\ntwo\nthree\n");
    let mut buf = [0u8; 16];
    let r = log.read(0, log.end(), &mut buf);
    assert_eq!(&buf[..r.len], b"0.000 one\n");
    assert_eq!(read_all(&log, 0, 16).0, b"0.000 one\n0.000 two\n0.000 three\n");
}

#[test]
fn a_line_longer_than_a_piece_still_comes_out() {
    let mut log = Log::<256>::new();
    log.record(0, b"abcdefghijklmnopqrstuvwxyz\n");
    assert_eq!(read_all(&log, 0, 8).0, b"0.000 abcdefghijklmnopqrstuvwxyz\n");
}

#[test]
fn a_read_from_what_was_overwritten_starts_at_a_whole_line() {
    let mut log = Log::<32>::new();
    for i in 0..10u8 {
        log.record(0, &vec![b'a' + i; 1 + usize::from(i % 2)]);
        log.record(0, b"\n");
    }
    assert_eq!(log.start(), log.end() - 32);
    let (got, from) = read_all(&log, 0, 64);
    assert_eq!(got, b"0.000 hh\n0.000 i\n0.000 jj\n");
    assert_eq!(log.end() - from, got.len() as u64);
    assert_eq!(read_all(&log, 0, 4).0, got);
}

#[test]
fn nothing_past_the_end_or_the_snapshot() {
    let mut log = Log::<64>::new();
    log.record(0, b"x\n");
    let until = log.end();
    log.record(0, b"y\n");
    let mut buf = [0u8; 64];
    let r = log.read(0, until, &mut buf);
    assert_eq!(&buf[..r.len], b"0.000 x\n");
    assert_eq!(log.read(until, until, &mut buf), Read { from: until, len: 0 });
    assert_eq!(log.read(500, 9999, &mut buf).len, 0);
}

#[test]
fn a_stamp_keeps_its_milliseconds_past_the_32_bit_count() {
    let mut log = Log::<64>::new();
    log.record(u64::from(u32::MAX), b"late\n");
    assert_eq!(read_all(&log, 0, 64).0, b"4294967.295 late\n");
}

#[test]
fn a_replay_to_the_last_line_end_leaves_a_line_in_progress_for_the_next() {
    let mut log = Log::<256>::new();
    log.record(0, b"done\r\n[ble] bonded devices: 1");
    let end = log.line_end();
    let mut buf = [0u8; 256];
    let r = log.read(0, end, &mut buf);
    assert_eq!(&buf[..r.len], b"0.000 done\n");
    log.record(0, b" AA:BB\r\n");
    assert_eq!(log.line_end(), log.end());
    let r = log.read(end, log.line_end(), &mut buf);
    assert_eq!(&buf[..r.len], b"0.000 [ble] bonded devices: 1 AA:BB\n");
}

struct Pipe {
    buf: Vec<u8>,
    cap: usize,
}

impl wrist_logic::console::Sink for Pipe {
    fn write(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len().min(self.cap - self.buf.len());
        self.buf.extend(&bytes[..n]);
        n
    }
}

#[test]
fn a_line_cut_short_drops_its_rest_and_the_next_starts_on_its_own() {
    let mut cut = wrist_logic::console::Cut::new();
    let mut pipe = Pipe { buf: Vec::new(), cap: 16 };
    assert_eq!(cut.feed(b"[hb] 1\r\n[hb] ", &mut pipe), 0);
    assert_eq!(cut.feed(b"2  uptime 9 s\r\n", &mut pipe), 12);
    assert_eq!(cut.feed(b"tail of nothing", &mut pipe), 15, "no room for the new line's lead");
    pipe.buf.clear();
    assert_eq!(cut.feed(b"\r\n[hb] 3\r\n", &mut pipe), 2);
    assert_eq!(pipe.buf, b"\r\n[hb] 3\r\n");
    pipe.buf.clear();
    assert_eq!(cut.feed(b"whole\r\n", &mut pipe), 0);
    assert_eq!(pipe.buf, b"whole\r\n");
}

#[test]
fn a_line_cut_short_at_its_end_still_ends_before_the_next() {
    let mut cut = wrist_logic::console::Cut::new();
    let mut pipe = Pipe { buf: Vec::new(), cap: 6 };
    assert_eq!(cut.feed(b"[hb] 1\r\n", &mut pipe), 2);
    pipe.cap = 64;
    assert_eq!(cut.feed(b"[hb] 2\r\n", &mut pipe), 0);
    assert_eq!(pipe.buf, b"[hb] 1\r\n[hb] 2\r\n");
}

#[test]
fn a_line_refused_whole_needs_no_lead() {
    let mut cut = wrist_logic::console::Cut::new();
    let mut pipe = Pipe { buf: b"x\r\n".to_vec(), cap: 4 };
    assert_eq!(cut.feed(b"", &mut pipe), 0);
    pipe.cap = 3;
    assert_eq!(cut.feed(b"[hb] 4\r\n", &mut pipe), 8);
    pipe.cap = 64;
    assert_eq!(cut.feed(b"[hb] 5\r\n", &mut pipe), 0);
    assert_eq!(pipe.buf, b"x\r\n[hb] 5\r\n");
}

#[test]
fn whole_lines_end_at_the_last_line_end() {
    use wrist_logic::console::whole_lines;
    assert_eq!(whole_lines(b"a\nb\nc"), 4);
    assert_eq!(whole_lines(b"abc"), 3);
    assert_eq!(whole_lines(b"a\n"), 2);
    assert_eq!(whole_lines(b""), 0);
}

#[test]
fn a_write_longer_than_the_log_keeps_its_end() {
    let mut log = Log::<16>::new();
    log.record(0, b"x\r\n");
    let text: Vec<u8> = (0..40u8).map(|i| b'a' + i % 26).chain(*b"\r\n").collect();
    log.record(0, &text);
    assert_eq!(log.end(), 8 + 6 + 41);
    assert_eq!(log.line_end(), log.end());
    let mut buf = [0u8; 16];
    let r = log.read(log.start(), log.end(), &mut buf);
    assert_eq!(&buf[..r.len], b"zabcdefghijklmn\n");
}
