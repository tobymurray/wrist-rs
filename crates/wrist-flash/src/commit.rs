//! The kernel write as a sequence over any [`Flash`]: what it refuses before bank 1 is
//! touched, the order it erases, programs and records in, and what each record says. `hw`
//! supplies the registers; the host tests a model.

use crate::plan::{bank1_page_address, Plan, PAGE_SIZE, PAGE_SIZE_U32, QUADWORD};

/// A blank page at the tail of the bootloader's partition, which the recovery's
/// erase does not reach (`HARDWARE.md`, "Boot handoff").
pub const STATUS_PAGE: u8 = 45;
pub const STATUS_ADDRESS: u32 = 0x0805_A000;
pub const STATUS_MAGIC: u32 = u32::from_le_bytes(*b"UNAF");
const _: () = assert!(STATUS_ADDRESS == 0x0800_0000 + STATUS_PAGE as u32 * PAGE_SIZE_U32);

/// Slot 0 of the status record, written before bank 1 is touched; the outcome goes in slot 1.
pub const OUTCOME_BEGAN: u32 = 9;
pub const OUTCOME_OK: u32 = 1;
/// `detail`: the page's index in the image `<< 24`, then `NSSR`'s errors.
pub const OUTCOME_ERASE_FAILED: u32 = 2;
/// `detail`: `NSSR`'s errors.
pub const OUTCOME_PROGRAM_FAILED: u32 = 3;
/// `detail`: the byte offset of the first mismatch.
pub const OUTCOME_VERIFY_FAILED: u32 = 4;
/// `detail`: the word read instead of `0xFFFF_FFFF`.
pub const OUTCOME_ERASE_NOT_BLANK: u32 = 5;
/// `detail`: [`POLL_TIMED_OUT`], after the page's index `<< 24` when erasing.
pub const OUTCOME_TIMEOUT: u32 = 6;
/// `detail`: the page's index `<< 24`, then [`NOT_ARMED`] with `NSCR`'s top half.
pub const OUTCOME_ERASE_NOT_ARMED: u32 = 7;

pub const ERR_UNLOCK: i32 = -6;
pub const ERR_STATUS_PAGE: i32 = -7;
pub const ERR_WITNESS_FOREIGN: i32 = -8;

pub const POLL_TIMED_OUT: u32 = 0xFFFF;
pub const NOT_ARMED: u32 = 0xE000;

pub const WRITE_ATTEMPTS: u32 = 3;

#[allow(clippy::cast_possible_truncation)]
const QUADWORD_U32: u32 = QUADWORD as u32;

/// The flash controller, as the sequence uses it.
pub trait Flash {
    /// Whether `NSCR` is writable after.
    fn unlock(&mut self) -> bool;
    fn lock(&mut self);
    fn kick_watchdog(&mut self);
    /// Leaves `PER` set for [`Flash::end_erase`].
    ///
    /// # Errors
    /// `NSSR`'s errors, [`POLL_TIMED_OUT`], or [`NOT_ARMED`] with `NSCR`'s top half.
    fn erase_page(&mut self, page: u8, bank2: bool) -> Result<(), u32>;
    /// Clears `PER` and `STRT`.
    fn end_erase(&mut self);
    /// `words` must be whole quad-words, at a 128-bit-aligned `address`.
    ///
    /// # Errors
    /// `NSSR`'s errors or [`POLL_TIMED_OUT`].
    fn program(&mut self, address: u32, words: &[u32]) -> Result<(), u32>;
    /// The word at `address` as flash now holds it, past the cache.
    fn read(&mut self, address: u32) -> u32;
    /// The byte offset of the first word from `address` that differs from `words`.
    fn verify(&mut self, address: u32, words: &[u32]) -> Option<usize>;
}

fn record<F: Flash>(f: &mut F, slot: u32, outcome: u32, detail: u32, image_crc: u32, plan: Plan) {
    if !f.unlock() {
        return;
    }
    let packed = (u32::from(plan.first_page) << 8) | u32::from(plan.pages);
    let _: Result<(), u32> =
        f.program(STATUS_ADDRESS + slot * QUADWORD_U32, &[STATUS_MAGIC, (outcome << 16) | packed, detail, image_crc]);
    // No lock here: the caller's next erase needs `NSCR` unlocked.
}

fn program_paged<F: Flash>(f: &mut F, address: u32, words: &[u32]) -> Result<(), u32> {
    let mut addr = address;
    for page in words.chunks(PAGE_SIZE / 4) {
        f.kick_watchdog();
        f.program(addr, page)?;
        addr += PAGE_SIZE_U32;
    }
    Ok(())
}

/// Puts `words`, the padded image, over the kernel as `plan` places it. `Err` only if it
/// refused before bank 1 was touched, with flash locked unless the unlock itself failed;
/// `Ok` once the outcome is recorded and flash locked, for the caller to reset.
///
/// # Errors
/// [`ERR_UNLOCK`], [`ERR_WITNESS_FOREIGN`] or [`ERR_STATUS_PAGE`].
pub fn commit<F: Flash>(f: &mut F, plan: Plan, image_crc: u32, words: &[u32]) -> Result<(), i32> {
    if !f.unlock() {
        return Err(ERR_UNLOCK);
    }

    // The witness page is in the bootloader's partition: refuse unless it is blank or ours.
    let witness = f.read(STATUS_ADDRESS);
    if witness != 0xFFFF_FFFF && witness != STATUS_MAGIC {
        f.lock();
        return Err(ERR_WITNESS_FOREIGN);
    }
    let witness_ok = f.erase_page(STATUS_PAGE, false).is_ok();
    f.end_erase();
    if !witness_ok {
        f.lock();
        return Err(ERR_STATUS_PAGE);
    }

    record(f, 0, OUTCOME_BEGAN, 0, image_crc, plan);

    // Point of no return: the kernel's vector table is in these pages.
    let mut outcome = OUTCOME_ERASE_FAILED;
    let mut detail = 0u32;
    let base = bank1_page_address(plan.first_page);
    for _ in 0..WRITE_ATTEMPTS {
        let mut failed = false;
        for i in 0..plan.pages {
            f.kick_watchdog();
            if let Err(errors) = f.erase_page(plan.first_page + i, false) {
                outcome = if errors == POLL_TIMED_OUT {
                    OUTCOME_TIMEOUT
                } else if errors & 0xF000 == NOT_ARMED {
                    OUTCOME_ERASE_NOT_ARMED
                } else {
                    OUTCOME_ERASE_FAILED
                };
                detail = (u32::from(i) << 24) | errors;
                failed = true;
                break;
            }
        }
        f.end_erase();
        if failed {
            continue;
        }

        // A silently dropped erase would otherwise surface as a program failure.
        let mut not_blank = None;
        for i in 0..plan.pages {
            let word = f.read(bank1_page_address(plan.first_page + i));
            if word != 0xFFFF_FFFF {
                not_blank = Some(word);
                break;
            }
        }
        if let Some(word) = not_blank {
            outcome = OUTCOME_ERASE_NOT_BLANK;
            detail = word;
            continue;
        }

        match program_paged(f, base, words) {
            Err(errors) => {
                outcome = if errors == POLL_TIMED_OUT { OUTCOME_TIMEOUT } else { OUTCOME_PROGRAM_FAILED };
                detail = errors;
            }
            Ok(()) => {
                if let Some(at) = f.verify(base, words) {
                    outcome = OUTCOME_VERIFY_FAILED;
                    detail = u32::try_from(at).unwrap_or(u32::MAX);
                } else {
                    outcome = OUTCOME_OK;
                    detail = 0;
                    break;
                }
            }
        }
    }

    record(f, 1, outcome, detail, image_crc, plan);
    f.lock();
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::plan::{plan, KERNEL_BASE, MAX_PAGES};
    use std::collections::BTreeMap;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Op {
        Unlock,
        Lock,
        Kick,
        Erase(u8),
        EndErase,
        Program(u32),
        Verify(u32),
    }

    /// The old kernel's first word in each of its pages.
    const OLD: u32 = 0x1234_5678;

    type EraseResult = Box<dyn FnMut(u8, u32) -> Result<(), u32>>;

    /// Bank 1 as words, unwritten ones erased, and every call logged.
    struct Model {
        words: BTreeMap<u32, u32>,
        log: Vec<Op>,
        unlocks: bool,
        /// From the page and how many times it was erased before.
        erase: EraseResult,
        erased: BTreeMap<u8, u32>,
        /// Kernel erases that answer OK and leave the page as it was.
        drops_erase: bool,
        /// For every kernel program.
        program_errors: Option<u32>,
        /// A word that programs as another.
        stuck: Option<(u32, u32)>,
    }

    impl Model {
        fn new() -> Self {
            let words = (48..48 + MAX_PAGES).map(|page| (bank1_page_address(page), OLD)).collect();
            Self {
                words,
                log: Vec::new(),
                unlocks: true,
                erase: Box::new(|_, _| Ok(())),
                erased: BTreeMap::new(),
                drops_erase: false,
                program_errors: None,
                stuck: None,
            }
        }

        fn failing_erase(erase: impl FnMut(u8, u32) -> Result<(), u32> + 'static) -> Self {
            Self { erase: Box::new(erase), ..Self::new() }
        }

        fn word(&self, address: u32) -> u32 {
            self.words.get(&address).copied().unwrap_or(0xFFFF_FFFF)
        }

        fn status(&self, slot: u32) -> [u32; 4] {
            core::array::from_fn(|w| self.word(STATUS_ADDRESS + slot * 16 + 4 * w as u32))
        }

        fn kernel_erases(&self) -> Vec<u8> {
            self.log
                .iter()
                .filter_map(|op| match *op {
                    Op::Erase(page) if page != STATUS_PAGE => Some(page),
                    _ => None,
                })
                .collect()
        }

        fn kernel_programs(&self) -> usize {
            self.log.iter().filter(|op| matches!(op, Op::Program(a) if *a >= KERNEL_BASE)).count()
        }

        fn kernel_untouched(&self) -> bool {
            self.kernel_erases().is_empty()
                && self.kernel_programs() == 0
                && (48..48 + MAX_PAGES).all(|page| self.word(bank1_page_address(page)) == OLD)
        }
    }

    impl Flash for Model {
        fn unlock(&mut self) -> bool {
            self.log.push(Op::Unlock);
            self.unlocks
        }

        fn lock(&mut self) {
            self.log.push(Op::Lock);
        }

        fn kick_watchdog(&mut self) {
            self.log.push(Op::Kick);
        }

        fn erase_page(&mut self, page: u8, bank2: bool) -> Result<(), u32> {
            assert!(!bank2, "the commit erases bank 1 only");
            self.log.push(Op::Erase(page));
            let before = self.erased.entry(page).or_insert(0);
            let n = *before;
            *before += 1;
            (self.erase)(page, n)?;
            if !(self.drops_erase && page != STATUS_PAGE) {
                let base = bank1_page_address(page);
                self.words.retain(|&a, _| !(base..base + PAGE_SIZE_U32).contains(&a));
            }
            Ok(())
        }

        fn end_erase(&mut self) {
            self.log.push(Op::EndErase);
        }

        fn program(&mut self, address: u32, words: &[u32]) -> Result<(), u32> {
            self.log.push(Op::Program(address));
            assert_eq!(address % QUADWORD_U32, 0);
            assert_eq!(words.len() % (QUADWORD / 4), 0);
            if let Some(errors) = self.program_errors.filter(|_| address >= KERNEL_BASE) {
                return Err(errors);
            }
            for (i, &w) in words.iter().enumerate() {
                let at = address + 4 * i as u32;
                // A quad-word programs once between erases.
                if self.word(at) != 0xFFFF_FFFF {
                    return Err(1 << 3);
                }
                let w = match self.stuck {
                    Some((stuck, value)) if stuck == at => value,
                    _ => w,
                };
                self.words.insert(at, w);
            }
            Ok(())
        }

        fn read(&mut self, address: u32) -> u32 {
            self.word(address)
        }

        fn verify(&mut self, address: u32, words: &[u32]) -> Option<usize> {
            self.log.push(Op::Verify(address));
            words.iter().enumerate().position(|(i, &w)| self.word(address + 4 * i as u32) != w).map(|i| i * 4)
        }
    }

    const CRC: u32 = 0xC0FF_EE00;

    /// Three pages and a bit, every word distinct.
    fn image() -> (Plan, Vec<u32>) {
        let p = plan(KERNEL_BASE, 3 * PAGE_SIZE + 100).unwrap();
        (p, (0..p.padded_len / 4).map(|i| 0xA000_0000 | i as u32).collect())
    }

    fn run(m: &mut Model) -> Result<(), i32> {
        let (p, words) = image();
        commit(m, p, CRC, &words)
    }

    fn packed(outcome: u32) -> u32 {
        let (p, _) = image();
        (outcome << 16) | (u32::from(p.first_page) << 8) | u32::from(p.pages)
    }

    /// The outcome record's code and detail.
    fn outcome(m: &Model) -> (u32, u32) {
        let s = m.status(1);
        assert_eq!((s[0], s[1] & 0xFFFF, s[3]), (STATUS_MAGIC, packed(0), CRC), "{s:08X?}");
        (s[1] >> 16, s[2])
    }

    #[test]
    fn a_good_write_puts_the_image_over_the_kernel_between_two_records() {
        let mut m = Model::new();
        assert_eq!(run(&mut m), Ok(()));
        let (p, words) = image();
        for (i, &w) in words.iter().enumerate() {
            assert_eq!(m.word(KERNEL_BASE + 4 * i as u32), w, "word {i}");
        }
        assert_eq!(m.word(bank1_page_address(48 + p.pages)), OLD, "the page after the image's is left");
        assert_eq!(m.status(0), [STATUS_MAGIC, packed(OUTCOME_BEGAN), 0, CRC]);
        assert_eq!(m.status(1), [STATUS_MAGIC, packed(OUTCOME_OK), 0, CRC]);
        assert_eq!(m.kernel_erases(), (48..48 + p.pages).collect::<Vec<_>>());
    }

    #[test]
    fn the_first_record_is_written_before_the_kernel_is_touched_and_the_last_after() {
        let mut m = Model::new();
        run(&mut m).unwrap();
        let at = |want: Op| m.log.iter().position(|&op| op == want).unwrap();
        let began = at(Op::Program(STATUS_ADDRESS));
        let ended = at(Op::Program(STATUS_ADDRESS + 16));
        assert!(at(Op::Erase(STATUS_PAGE)) < began);
        assert!(began < at(Op::Erase(48)));
        assert!(at(Op::Verify(KERNEL_BASE)) < ended);
        assert_eq!(&m.log[ended - 1..], [Op::Unlock, Op::Program(STATUS_ADDRESS + 16), Op::Lock]);
    }

    #[test]
    fn the_watchdog_is_kicked_before_every_kernel_page_is_erased_or_programmed() {
        let mut m = Model::new();
        run(&mut m).unwrap();
        let mut pages = 0;
        for (i, op) in m.log.iter().enumerate() {
            let starts_a_page = match *op {
                Op::Erase(page) => page >= 48,
                Op::Program(a) => a >= KERNEL_BASE && (a - KERNEL_BASE).is_multiple_of(PAGE_SIZE_U32),
                _ => false,
            };
            if starts_a_page {
                pages += 1;
                assert_eq!(m.log[i - 1], Op::Kick, "{op:?}");
            }
        }
        assert_eq!(pages, 8);
    }

    #[test]
    fn every_refusal_leaves_the_kernel_untouched() {
        let mut m = Model { unlocks: false, ..Model::new() };
        assert_eq!(run(&mut m), Err(ERR_UNLOCK));
        assert_eq!(m.log, [Op::Unlock]);
        assert!(m.kernel_untouched());

        let mut m = Model::new();
        m.words.insert(STATUS_ADDRESS, 0x2000_8000);
        assert_eq!(run(&mut m), Err(ERR_WITNESS_FOREIGN));
        assert_eq!(m.log, [Op::Unlock, Op::Lock]);
        assert_eq!(m.word(STATUS_ADDRESS), 0x2000_8000, "a foreign page is left as found");
        assert!(m.kernel_untouched());

        let mut m = Model::failing_erase(|page, _| if page == STATUS_PAGE { Err(1 << 7) } else { Ok(()) });
        assert_eq!(run(&mut m), Err(ERR_STATUS_PAGE));
        assert_eq!(m.log, [Op::Unlock, Op::Erase(STATUS_PAGE), Op::EndErase, Op::Lock]);
        assert!(m.kernel_untouched());
    }

    #[test]
    fn a_status_page_of_ours_is_reused() {
        let mut m = Model::new();
        run(&mut m).unwrap();
        m.log.clear();
        assert_eq!(run(&mut m), Ok(()));
        assert_eq!(outcome(&m), (OUTCOME_OK, 0));
    }

    #[test]
    fn an_erase_that_fails_once_is_retried() {
        let mut m = Model::failing_erase(|page, n| if page == 50 && n == 0 { Err(1 << 1) } else { Ok(()) });
        assert_eq!(run(&mut m), Ok(()));
        assert_eq!(outcome(&m), (OUTCOME_OK, 0));
        assert_eq!(m.kernel_erases(), [48, 49, 50, 48, 49, 50, 51]);
    }

    #[test]
    fn an_erase_that_keeps_failing_is_recorded_with_its_page_after_every_attempt() {
        for (errors, code) in [
            (1 << 7, OUTCOME_ERASE_FAILED),
            (POLL_TIMED_OUT, OUTCOME_TIMEOUT),
            // `NSCR` read back locked, with `STRT` set.
            (NOT_ARMED | 0x8001, OUTCOME_ERASE_NOT_ARMED),
        ] {
            let mut m = Model::failing_erase(move |page, _| if page == 50 { Err(errors) } else { Ok(()) });
            assert_eq!(run(&mut m), Ok(()), "past the point of no return it resets either way");
            assert_eq!(outcome(&m), (code, (2 << 24) | errors), "{errors:#x}");
            assert_eq!(m.erased[&50], WRITE_ATTEMPTS);
            assert_eq!(m.kernel_programs(), 0);
            assert_eq!(m.log.last(), Some(&Op::Lock));
        }
    }

    #[test]
    fn an_erase_that_did_not_take_is_caught_before_programming() {
        let mut m = Model { drops_erase: true, ..Model::new() };
        assert_eq!(run(&mut m), Ok(()));
        assert_eq!(outcome(&m), (OUTCOME_ERASE_NOT_BLANK, OLD));
        assert_eq!(m.kernel_programs(), 0);
        assert_eq!(m.kernel_erases().len(), 4 * WRITE_ATTEMPTS as usize);
    }

    #[test]
    fn a_program_failure_is_recorded_with_its_errors() {
        for (errors, code) in [(1 << 3, OUTCOME_PROGRAM_FAILED), (POLL_TIMED_OUT, OUTCOME_TIMEOUT)] {
            let mut m = Model { program_errors: Some(errors), ..Model::new() };
            assert_eq!(run(&mut m), Ok(()));
            assert_eq!(outcome(&m), (code, errors));
            assert_eq!(m.kernel_erases().len(), 4 * WRITE_ATTEMPTS as usize);
        }
    }

    #[test]
    fn a_word_that_reads_back_wrong_is_recorded_at_its_offset() {
        let mut m = Model { stuck: Some((KERNEL_BASE + 0x2004, 0)), ..Model::new() };
        assert_eq!(run(&mut m), Ok(()));
        assert_eq!(outcome(&m), (OUTCOME_VERIFY_FAILED, 0x2004));
        assert_eq!(m.log.iter().filter(|op| matches!(op, Op::Verify(_))).count(), WRITE_ATTEMPTS as usize);
    }

    /// `Service.cpp` decodes the record on the watch after an install.
    #[test]
    fn the_installer_reads_the_record_as_it_is_written() {
        let header = include_str!("../../../apps/image-installer/Libs/Header/Service.hpp");
        let source = include_str!("../../../apps/image-installer/Libs/Sources/Service.cpp");
        let constant = |name: &str| {
            let line = header.lines().find(|l| l.contains(&format!("{name} "))).unwrap();
            let hex = line.split("0x").nth(1).unwrap().split('u').next().unwrap().replace('\'', "");
            u32::from_str_radix(&hex, 16).unwrap()
        };
        assert_eq!(constant("kStatusAddress"), STATUS_ADDRESS);
        assert_eq!(constant("kStatusMagic"), STATUS_MAGIC);
        assert_eq!(constant("kKernelBase"), KERNEL_BASE);
        assert!(source.contains(&format!("((st[1] >> 16) & 0xFFFFu) == {OUTCOME_BEGAN}u")));
        assert!(source.contains("const bool finished = (st[4] == kStatusMagic);"));
        assert!(source.contains("const uint32_t outcome = (st[5] >> 16) & 0xFFFFu;"));
        for (code, word) in [
            (OUTCOME_OK, "SUCCEEDED"),
            (OUTCOME_ERASE_FAILED, "ERASE failed"),
            (OUTCOME_PROGRAM_FAILED, "PROGRAM failed"),
            (OUTCOME_VERIFY_FAILED, "VERIFY failed"),
            (OUTCOME_ERASE_NOT_BLANK, "NOT BLANK"),
            (OUTCOME_TIMEOUT, "TIMED OUT"),
            (OUTCOME_ERASE_NOT_ARMED, "DID NOT STICK"),
        ] {
            let case = source.split(&format!("case {code}:")).nth(1).unwrap_or_else(|| panic!("no case {code}"));
            let verdict = case.split("break;").next().unwrap();
            assert!(verdict.contains(&format!("OUTCOME {code}:")) && verdict.contains(word), "case {code}: {verdict}");
        }
        assert!(source.contains(&format!("(install will refuse, {ERR_WITNESS_FOREIGN})")));
    }
}
