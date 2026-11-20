//! `fixture <capture>`: a `ble-trace` capture cut to a replay fixture, with no address and no
//! console text.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: fixture <capture>");
        std::process::exit(2);
    };
    let capture = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    print!("{}", wrist_ble_sim::fixture::fixture(&capture));
}
