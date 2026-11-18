//! `update revert`: the image the last kept update replaced, which embassy-boot's swap
//! (`BootLoader::swap`) leaves in the DFU slot one page in.

use crate::layout::bank1;
use crate::plan::{BANK1_BASE, PAGE_SIZE, PAGE_SIZE_U32};
use crate::vectors::Target;

pub const DFU_BASE: u32 = BANK1_BASE + bank1::DFU.start as u32 * PAGE_SIZE_U32;
pub const DFU_SIZE: usize = (bank1::DFU.end - bank1::DFU.start) as usize * PAGE_SIZE;

const ACTIVE_SIZE: usize = Target::Application.size() as usize;
const _: () = assert!(DFU_SIZE == ACTIVE_SIZE + PAGE_SIZE);

/// What flash reads as once erased.
pub const ERASED: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The slot's first page is not the running image's, as a swap leaves it.
    NotReplaced,
    /// Nothing but erased flash where the image would be.
    Empty,
    /// What is there does not start with an application's vector table.
    NotAnImage,
}

impl Refusal {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Refusal::NotReplaced => {
                "the other slot holds no image this one replaced (it does after an update is kept, not after a revert by the loader or an install from stock); nothing written"
            }
            Refusal::Empty => "the other slot is empty; nothing written",
            Refusal::NotAnImage => "the other slot's image has no application vector table; nothing written",
        }
    }
}

/// The replaced image in `dfu`, the whole DFU slot, up to its last byte that is not [`ERASED`].
///
/// # Errors
/// The [`Refusal`] that says why the slot holds no image to go back to.
pub fn previous_image<'a>(dfu: &'a [u8], active: &[u8]) -> Result<&'a [u8], Refusal> {
    if dfu.len() != DFU_SIZE || active.len() < PAGE_SIZE || dfu[..PAGE_SIZE] != active[..PAGE_SIZE] {
        return Err(Refusal::NotReplaced);
    }
    let candidate = &dfu[PAGE_SIZE..];
    let len = candidate.iter().rposition(|&b| b != ERASED).map_or(0, |i| i + 1);
    if len == 0 {
        return Err(Refusal::Empty);
    }
    let image = &candidate[..len];
    if !Target::Application.accepts(image) {
        return Err(Refusal::NotAnImage);
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::SP_MAX;

    /// The two slots, moved page by page as embassy-boot's `BootLoader::swap` and `revert`
    /// move them.
    struct Slots {
        active: Vec<u8>,
        dfu: Vec<u8>,
    }

    impl Slots {
        /// A fresh install: the image in the active slot, the DFU slot erased.
        fn installed(image: &[u8]) -> Self {
            let mut active = vec![ERASED; ACTIVE_SIZE];
            active[..image.len()].copy_from_slice(image);
            Slots { active, dfu: vec![ERASED; DFU_SIZE] }
        }

        /// `prepare_update`, then the image from offset 0.
        fn write_update(&mut self, image: &[u8]) {
            self.dfu.fill(ERASED);
            self.dfu[..image.len()].copy_from_slice(image);
        }

        fn page(n: usize) -> core::ops::Range<usize> {
            n * PAGE_SIZE..(n + 1) * PAGE_SIZE
        }

        fn swap(&mut self) {
            let count = ACTIVE_SIZE / PAGE_SIZE;
            for n in 0..count {
                let from = count - 1 - n;
                let a = self.active[Self::page(from)].to_vec();
                self.dfu[Self::page(count - n)].copy_from_slice(&a);
                let d = self.dfu[Self::page(from)].to_vec();
                self.active[Self::page(from)].copy_from_slice(&d);
            }
        }

        fn revert(&mut self) {
            for n in 0..ACTIVE_SIZE / PAGE_SIZE {
                let a = self.active[Self::page(n)].to_vec();
                self.dfu[Self::page(n)].copy_from_slice(&a);
                let d = self.dfu[Self::page(n + 1)].to_vec();
                self.active[Self::page(n)].copy_from_slice(&d);
            }
        }

        fn previous(&self) -> Result<&[u8], Refusal> {
            previous_image(&self.dfu, &self.active)
        }

        /// What the console's `update revert` does: the previous image as an update.
        fn revert_by_command(&mut self) {
            let image = self.previous().expect("a previous image").to_vec();
            self.write_update(&image);
            self.swap();
        }

        fn runs(&self, image: &[u8]) -> bool {
            self.active[..image.len()] == *image && self.active[image.len()..].iter().all(|&b| b == ERASED)
        }
    }

    /// An application image of `len` bytes: a vector table the loader accepts, then bytes
    /// that differ with `seed` and never end in an erased byte.
    fn image(len: usize, seed: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(len);
        v.extend_from_slice(&(SP_MAX & !7).to_le_bytes());
        v.extend_from_slice(&(Target::Application.base() + 0x201).to_le_bytes());
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        while v.len() < len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push(x.to_le_bytes()[0]);
        }
        v[len - 1] = 0x5A;
        v
    }

    #[test]
    fn a_fresh_install_has_nothing_to_go_back_to() {
        let s = Slots::installed(&image(300_000, 1));
        assert_eq!(s.previous(), Err(Refusal::NotReplaced));
    }

    #[test]
    fn a_kept_update_leaves_the_image_it_replaced() {
        let (a, b) = (image(300_001, 1), image(250_000, 2));
        let mut s = Slots::installed(&a);
        s.write_update(&b);
        s.swap();
        assert!(s.runs(&b));
        assert_eq!(s.previous(), Ok(&a[..]));
    }

    #[test]
    fn going_back_and_forth_swaps_the_two() {
        let (a, b) = (image(300_001, 1), image(310_000, 2));
        let mut s = Slots::installed(&a);
        s.write_update(&b);
        s.swap();
        s.revert_by_command();
        assert!(s.runs(&a));
        assert_eq!(s.previous(), Ok(&b[..]));
        s.revert_by_command();
        assert!(s.runs(&b));
        assert_eq!(s.previous(), Ok(&a[..]));
    }

    #[test]
    fn after_the_loader_reverts_there_is_nothing_to_go_back_to() {
        let (a, b, c) = (image(300_001, 1), image(310_000, 2), image(320_000, 3));
        let mut s = Slots::installed(&a);
        s.write_update(&b);
        s.swap();
        s.write_update(&c);
        s.swap();
        // C never marked itself booted: the loader puts B back.
        s.revert();
        assert!(s.runs(&b));
        assert_eq!(s.previous(), Err(Refusal::NotReplaced));
    }

    #[test]
    fn a_full_size_image_is_found_whole() {
        let (a, b) = (image(ACTIVE_SIZE, 1), image(200_000, 2));
        let mut s = Slots::installed(&a);
        s.write_update(&b);
        s.swap();
        assert_eq!(s.previous(), Ok(&a[..]));
    }

    #[test]
    fn trailing_erased_bytes_are_dropped_and_the_swap_restores_them() {
        let mut a = image(300_000, 1);
        a.extend_from_slice(&[ERASED; 7]);
        let b = image(250_000, 2);
        let mut s = Slots::installed(&a);
        s.write_update(&b);
        s.swap();
        assert_eq!(s.previous(), Ok(&a[..300_000]));
        s.revert_by_command();
        assert!(s.runs(&a));
    }

    #[test]
    fn what_is_not_an_image_is_refused() {
        let b = image(250_000, 2);
        let mut s = Slots::installed(&b);
        s.dfu[..PAGE_SIZE].copy_from_slice(&s.active[..PAGE_SIZE]);
        assert_eq!(s.previous(), Err(Refusal::Empty));
        s.dfu[PAGE_SIZE..PAGE_SIZE + 4].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(s.previous(), Err(Refusal::NotAnImage));
    }

    #[test]
    fn the_wrong_sizes_are_refused() {
        let b = image(250_000, 2);
        assert_eq!(previous_image(&b, &b), Err(Refusal::NotReplaced));
    }
}
