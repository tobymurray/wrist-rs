//! Which firmware this is, for the console's `version` and anything on the watch that shows it.

/// The apps' Cargo version, which a release's tag repeats.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The firmware hash: 12 hex digits naming the inputs this image was built from
/// (`release/build-inputs.txt`), the `build_id` of a release's `manifest.json`, with
/// `-dirty` after them when an input had uncommitted changes. A build given
/// `WRIST_RELEASE_BUILD_ID` carries that instead, and one outside git `unknown`.
pub const BUILD_ID: &str = env!("WRIST_BUILD_ID");
