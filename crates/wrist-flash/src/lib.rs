#![cfg_attr(not(test), no_std)]

//! The installer app's C interface: stage an image, put it over the kernel, reset into it.

pub mod plan;
pub mod survey;
pub mod vectors;

#[cfg(target_arch = "arm")]
pub mod hw;

#[cfg(all(feature = "panic-handler", target_arch = "arm", not(test)))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    hw::reset()
}

#[repr(C)]
pub struct CPlan {
    pub first_page: u8,
    pub pages: u8,
    /// The bank 2 page of the first staged page.
    pub staging_page: u8,
    pub padded_len: u32,
    pub quadwords: u32,
}

/// # Safety
/// `image` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_crc32(image: *const u8, len: usize) -> u32 {
    if image.is_null() || len == 0 {
        return 0;
    }
    plan::crc32(core::slice::from_raw_parts(image, len))
}

/// [`plan::crc32_continue`] over a C buffer.
///
/// # Safety
/// `bytes` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_crc32_continue(crc: u32, bytes: *const u8, len: usize) -> u32 {
    if bytes.is_null() || len == 0 {
        return crc;
    }
    plan::crc32_continue(crc, core::slice::from_raw_parts(bytes, len))
}

/// 0 with `out` filled, or a negative [`plan::Refusal::code`].
///
/// # Safety
/// `out` must be writable, or null.
#[no_mangle]
pub unsafe extern "C" fn wrist_flash_plan(target: u32, len: usize, out: *mut CPlan) -> i32 {
    match plan::plan(target, len) {
        Err(refusal) => refusal.code(),
        Ok(p) => {
            if !out.is_null() {
                (*out) = CPlan {
                    first_page: p.first_page,
                    pages: p.pages,
                    staging_page: plan::STAGING_FIRST_PAGE,
                    padded_len: u32::try_from(p.padded_len).unwrap_or(u32::MAX),
                    quadwords: u32::try_from(p.quadwords).unwrap_or(u32::MAX),
                };
            }
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plan::{KERNEL_BASE, MAX_IMAGE, STAGING_FIRST_PAGE};

    #[test]
    fn the_c_plan_reports_the_staging_page() {
        let mut out = CPlan { first_page: 0, pages: 0, staging_page: 0, padded_len: 0, quadwords: 0 };
        assert_eq!(unsafe { wrist_flash_plan(KERNEL_BASE, MAX_IMAGE, &raw mut out) }, 0);
        assert_eq!(out.first_page, 48);
        assert_eq!(out.pages, 68);
        assert_eq!(out.staging_page, STAGING_FIRST_PAGE);
        assert_eq!(out.padded_len as usize, MAX_IMAGE);
        assert_eq!(unsafe { wrist_flash_plan(KERNEL_BASE, MAX_IMAGE + 1, &raw mut out) }, -4);
    }

    #[test]
    fn chunked_crc_matches_the_whole() {
        let image: Vec<u8> = (0..20_000u32).map(|i| u8::try_from(i % 253).unwrap()).collect();
        let mut crc = 0xFFFF_FFFF;
        for chunk in image.chunks(8192) {
            crc = unsafe { wrist_flash_crc32_continue(crc, chunk.as_ptr(), chunk.len()) };
        }
        assert_eq!(!crc, unsafe { wrist_flash_crc32(image.as_ptr(), image.len()) });
    }
}
