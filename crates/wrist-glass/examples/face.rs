//! A page as the firmware draws it, as raw ABGR2222 bytes, to compare with `page dump`:
//!
//!     cargo run -p wrist-glass --example face -- 09:11 2026-10-23 100 face.raw
//!     cargo run -p wrist-glass --example face -- battery 100 4405 usb battery.raw
//!     cargo run -p wrist-glass --example face -- about 0.1.0 5784ec72019b about.raw
//!     cargo run -p wrist-glass --example face -- stress churn 7 stress.raw
//!
//! `-` for any reading draws it unset.

use wrist_glass::pages::{compose, compose_about, compose_battery, render, About, Battery, Clock, Date, Status, EN_GB, FACES};
use wrist_glass::stress::{compose_stress, Stress};
use wrist_glass::{HEIGHT, WIDTH};

fn arg(s: &str) -> Option<&str> {
    (s != "-").then_some(s)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stress = matches!(args.first().map(String::as_str), Some("stress"));
    let (glass, short) = match args.as_slice() {
        [page, kind, step, _] if page == "stress" => compose_stress(
            Stress::parse(kind).expect("glyphs, churn, edge, full or pages"),
            step.parse().expect("a step"),
            FACES,
        ),
        [page, soc, mv, input, _] if page == "battery" => compose_battery(
            &Battery {
                soc_pct: arg(soc).map(|s| s.parse().expect("a percentage")),
                cell_mv: arg(mv).map(|s| s.parse().expect("millivolts")),
                external: arg(input).map(|s| s == "usb"),
            },
            &EN_GB,
            FACES,
        ),
        [page, version, build, _] if page == "about" => compose_about(&About { version, build }, &EN_GB, FACES),
        [time, date, soc, rest @ ..] if rest.len() <= 2 => {
            let nums = |s: &str, sep| -> Vec<u16> { s.split(sep).map(|n| n.parse().expect("a number")).collect() };
            let byte = |n: u16| u8::try_from(n).expect("a field under 256");
            let clock = arg(time).map(|t| {
                let t = nums(t, ':');
                Clock { h: byte(t[0]), m: byte(t[1]), s: 0 }
            });
            let date = arg(date).map(|d| {
                let d = nums(d, '-');
                Date { year: d[0], month: byte(d[1]), day: byte(d[2]) }
            });
            let soc_pct = arg(soc).map(|s| s.parse().expect("a percentage"));
            let steps = rest.first().filter(|_| rest.len() == 2).and_then(|s| arg(s)).map(|s| s.parse().expect("a step count"));
            compose(&Status { clock, date, soc_pct, steps }, &EN_GB, FACES)
        }
        _ => {
            eprintln!("usage: face HH:MM YYYY-MM-DD PERCENT [STEPS] OUT.raw");
            eprintln!("       face battery PERCENT MV usb|cell OUT.raw");
            eprintln!("       face about VERSION BUILD OUT.raw   ('-' for unset)");
            eprintln!("       face stress KIND STEP OUT.raw");
            std::process::exit(2);
        }
    };
    assert!(stress || short == 0, "a line did not fit");
    let mut fb = vec![0u8; (WIDTH * HEIGHT) as usize];
    assert!(render(&mut fb, &glass));
    std::fs::write(args.last().expect("an output path"), fb).expect("write the frame");
}
