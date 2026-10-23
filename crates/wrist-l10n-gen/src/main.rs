//! `cargo run -p wrist-l10n-gen`: regenerate `crates/wrist-l10n/src/generated.rs` from `l10n/`.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match wrist_l10n_gen::generate(&root) {
        Ok(text) => match std::fs::write(root.join(wrist_l10n_gen::OUTPUT), text) {
            Ok(()) => {
                println!("wrote {}", wrist_l10n_gen::OUTPUT);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}: {e}", wrist_l10n_gen::OUTPUT);
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
