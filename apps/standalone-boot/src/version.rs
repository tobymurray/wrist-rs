//! Which firmware this is, for the console's `version` and anything on the watch that shows it.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The apps' Cargo version, which a release's tag repeats, then [`VARIANT`].
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("WRIST_BUILD_VARIANT"));

/// How this build differs from the release recipe's, as semver build metadata: `+`, then each
/// feature beyond the recipe's (`app_features` in `release/toolchain.json`), `no-<feature>`
/// for each of those left out, and `local` for a build outside the recipe, whose binary holds
/// the builder's paths (build.rs names the rest). Empty for the recipe's build.
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

/// The CRC-32 of [`image`], once [`compute_crc`] has run, with [`CRC_KNOWN`] then true.
static CRC: AtomicU32 = AtomicU32::new(0);
static CRC_KNOWN: AtomicBool = AtomicBool::new(false);

/// app.bin as it stands in the running slot. objcopy writes app.bin from the vector table to the
/// end of `.data`'s load image, the last flash section, and an update writes those bytes there.
fn image() -> &'static [u8] {
    extern "C" {
        static __sidata: u32;
        static __sdata: u32;
        static __edata: u32;
    }
    let start = crate::image_origin() as usize;
    let (load, data, data_end) = (
        core::ptr::addr_of!(__sidata) as usize,
        core::ptr::addr_of!(__sdata) as usize,
        core::ptr::addr_of!(__edata) as usize,
    );
    // Flash, unchanged while the image runs: an update stages in RAM and writes the other slot.
    unsafe { core::slice::from_raw_parts(start as *const u8, load + (data_end - data) - start) }
}

/// Hashes [`image`] once, at boot, 4 KiB between yields, as `tools/wrist-update.py` does app.bin
/// (zlib's CRC-32). Its loop is 8 instructions a byte, about 13 cycles: some 3 M cycles for a
/// 230 KB image, about 65 ms of the boot's awake time at 48 MHz, in polls of about 1 ms. It
/// waits on no timer, so it adds no wake from Stop 2.
pub async fn compute_crc() {
    let mut crc = 0xFFFF_FFFF;
    for piece in image().chunks(4096) {
        crc = wrist_flash::plan::crc32_continue(crc, piece);
        embassy_futures::yield_now().await;
    }
    CRC.store(!crc, Ordering::Relaxed);
    CRC_KNOWN.store(true, Ordering::Release);
}

/// The image's CRC-32, once [`compute_crc`] has run.
pub fn crc() -> Option<u32> {
    CRC_KNOWN.load(Ordering::Acquire).then(|| CRC.load(Ordering::Relaxed))
}

/// ` crc XXXXXXXX`, as `tools/wrist-update.py` prints it, or ` crc pending` before it is known.
pub fn write_crc(uart: crate::uart::Uart) {
    uart.write_str(" crc ");
    match crc() {
        Some(crc) => crc.to_be_bytes().into_iter().for_each(|b| uart.write_hex8(b)),
        None => uart.write_str("pending"),
    }
}
