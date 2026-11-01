//! Which firmware this is, for the console's `version` and anything on the watch that shows it.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// The apps' Cargo version, which a release's tag repeats, then [`VARIANT`].
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("WRIST_BUILD_VARIANT"));

/// How this build differs from the release recipe's, as build.rs names it; empty for the
/// recipe's build.
pub const VARIANT: &str = env!("WRIST_BUILD_VARIANT");

/// The firmware hash of the inputs (`release/build-inputs.txt`), or the name
/// `WRIST_RELEASE_BUILD_ID` gives.
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

/// app.bin as the running slot holds it: the vector table to the end of `.data`'s load image.
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

/// Hashes [`image`] with the CRC-32 `tools/wrist-update.py` prints for app.bin, 4 KiB between
/// yields.
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
