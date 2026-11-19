use wrist_logic::console::Held;

/// Each release as the console's task makes it: pieces up to a line's end.
fn releases<const N: usize>(held: &mut Held<N>, piece: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; piece];
    let mut line = Vec::new();
    loop {
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
    loop {
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
