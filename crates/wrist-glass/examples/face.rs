//! The face as the firmware draws it, as raw ABGR2222 bytes, to compare with `page dump`:
//!
//!     cargo run -p wrist-glass --example face -- 09:11 2026-10-23 100 face.raw
//!
//! `-` for the time, date or charge draws that line unset.

use wrist_glass::pages::{compose, render, Clock, Date, Status};
use wrist_glass::{HEIGHT, WIDTH};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [time, date, soc, out] = args.as_slice() else {
        eprintln!("usage: face HH:MM YYYY-MM-DD PERCENT OUT.raw ('-' for unset)");
        std::process::exit(2);
    };
    let nums = |s: &str, sep| -> Vec<u16> { s.split(sep).map(|n| n.parse().expect("a number")).collect() };
    let byte = |n: u16| u8::try_from(n).expect("a field under 256");
    let clock = (time != "-").then(|| {
        let t = nums(time, ':');
        Clock { h: byte(t[0]), m: byte(t[1]), s: 0 }
    });
    let date = (date != "-").then(|| {
        let d = nums(date, '-');
        Date { year: d[0], month: byte(d[1]), day: byte(d[2]) }
    });
    let soc_pct = (soc != "-").then(|| soc.parse().expect("a percentage"));
    let (glass, short) = compose(&Status { clock, date, soc_pct });
    assert_eq!(short, 0, "a line did not fit");
    let mut fb = vec![0u8; (WIDTH * HEIGHT) as usize];
    assert!(render(&mut fb, &glass));
    std::fs::write(out, fb).expect("write the frame");
}
