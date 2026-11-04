//! `replay <capture> [--events]`: a `ble-trace` capture through the core, beside the driver.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: replay <capture> [--events]");
        std::process::exit(2);
    };
    let capture = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    print!("{}", wrist_ble_sim::replay::replay(&capture).render(args.iter().any(|a| a == "--events")));
}
