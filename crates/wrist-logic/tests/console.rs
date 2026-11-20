use wrist_logic::console::{room, Held, HEADROOM};

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

/// The mirror's queue as `ble.rs` fills it: a line goes in whole or is dropped.
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
