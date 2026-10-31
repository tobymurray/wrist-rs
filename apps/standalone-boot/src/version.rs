//! Which firmware this is, for the console's `version` and anything on the watch that shows it.

/// The apps' Cargo version, which a release's tag repeats, then [`VARIANT`].
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("WRIST_BUILD_VARIANT"));

/// How this build differs from the release recipe's, as semver build metadata: `+`, then each
/// enabled feature, and `local` for a build outside the recipe, whose panic messages hold the
/// builder's paths (build.rs names the rest). Empty for the recipe's build.
pub const VARIANT: &str = env!("WRIST_BUILD_VARIANT");

/// The firmware hash: 12 hex digits naming the inputs this image was built from
/// (`release/build-inputs.txt`), the `build_id` of a release's `manifest.json`, with
/// `-dirty` after them when an input had uncommitted changes. A build given
/// `WRIST_RELEASE_BUILD_ID` carries that instead, and one outside git `unknown`.
pub const SOURCE_ID: &str = env!("WRIST_SOURCE_ID");

/// This build's ID: [`SOURCE_ID`] for the recipe's build, and for any other the hash of its
/// inputs and its [`VARIANT`], so that two images that differ have different IDs.
pub const BUILD_ID: &str = env!("WRIST_BUILD_ID");

/// The build ID, then for a variant what it was built from and how: `<id> (<source>+<variant>)`.
pub fn write_build(uart: crate::uart::Uart) {
    uart.write_str(BUILD_ID);
    if !VARIANT.is_empty() {
        uart.write_str(" (");
        uart.write_str(SOURCE_ID);
        uart.write_str(VARIANT);
        uart.write_str(")");
    }
}
