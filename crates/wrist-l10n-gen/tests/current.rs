//! The checked-in module is what `l10n/` generates; after editing `l10n/`, run
//! `cargo run -p wrist-l10n-gen`.

use std::path::Path;

#[test]
fn the_generated_module_is_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fresh = wrist_l10n_gen::generate(&root).unwrap();
    let checked_in = std::fs::read_to_string(root.join(wrist_l10n_gen::OUTPUT)).unwrap();
    assert!(fresh == checked_in, "{} is stale: run `cargo run -p wrist-l10n-gen`", wrist_l10n_gen::OUTPUT);
}
