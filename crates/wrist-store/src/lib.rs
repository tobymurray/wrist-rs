#![no_std]
// The const fns take page numbers, which fit a u8, and cannot call `try_from`.
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]

//! The glyph store's arithmetic: which pages a pack may use, the order a write goes in, the
//! commit record that makes a pack count, and what a boot must erase before it reads.
//!
//! Pure, so the host tests cover it; the image's flash glue takes only a [`StorePage`] to
//! erase, and a [`StorePage`] can only be a store page (`plan/GLYPH-STORE-2026-10-23.md`).
//!
//! A pack occupies whole pages from its first page. The first [`COMMIT_AREA`] bytes of that
//! page hold its [`CommitRecord`] and its retired quad-word; the pack's own bytes start after
//! them. A write erases its pages first page first, programs the pack, reads it back, and
//! programs the commit record last, so a write cut short anywhere leaves no valid record.

use core::ops::Range;

use wrist_flash::layout::bank2::GLYPH_STORE;
use wrist_flash::plan::{crc32, BANK2_BASE, PAGE_SIZE, PAGE_SIZE_U32, QUADWORD};

/// Pages in the store.
pub const PAGES: usize = (GLYPH_STORE.end - GLYPH_STORE.start) as usize;

/// The commit record, the retired quad-word, and the rest erased for later use.
pub const COMMIT_AREA: usize = 4096;

/// The largest pack the store takes: every page, less one commit area.
pub const MAX_PACK: usize = PAGES * PAGE_SIZE - COMMIT_AREA;
const _: () = assert!(MAX_PACK == 487_424);

/// A page of the glyph store, and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StorePage(u8);

impl StorePage {
    /// For a bank 2 page in the store; `None` for any other.
    #[must_use]
    pub const fn new(bank2_page: u8) -> Option<Self> {
        if bank2_page as u16 >= GLYPH_STORE.start && (bank2_page as u16) < GLYPH_STORE.end {
            Some(Self(bank2_page))
        } else {
            None
        }
    }

    /// The `index`th page of the store, 0 first.
    #[must_use]
    pub const fn nth(index: usize) -> Option<Self> {
        if index < PAGES {
            Some(Self((GLYPH_STORE.start as usize + index) as u8))
        } else {
            None
        }
    }

    /// The page number within bank 2, for `FLASH_NSCR.PNB` with `BKER` set.
    #[must_use]
    pub const fn pnb(self) -> u8 {
        self.0
    }

    /// Always bank 2.
    #[must_use]
    pub const fn bank2(self) -> bool {
        true
    }

    /// 0 for the store's first page.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize - GLYPH_STORE.start as usize
    }

    #[must_use]
    pub const fn bank_offset(self) -> u32 {
        self.0 as u32 * PAGE_SIZE_U32
    }

    #[must_use]
    pub const fn address(self) -> u32 {
        BANK2_BASE + self.bank_offset()
    }
}

/// Why a write is refused, before anything is erased.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    /// More than [`MAX_PACK`] bytes.
    TooLarge,
    /// No run of free pages long enough.
    NoSpace,
    OutsideStore,
    CrossesStoreEnd,
    /// An offset or length that is not a whole quad-word.
    Unaligned,
    /// An update or loader transfer owns the flash controller.
    FlashBusy,
    /// `OPTR.SWAP_BANK` is set, so bank 2's addresses are not the store's.
    BanksSwapped,
    /// Cannot happen through [`StorePage`]; checked anyway.
    WouldEraseOutsideStore,
}

impl Refusal {
    /// Never renumbered.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Empty => -1,
            Self::TooLarge => -2,
            Self::NoSpace => -3,
            Self::OutsideStore => -4,
            Self::CrossesStoreEnd => -5,
            Self::Unaligned => -6,
            Self::FlashBusy => -7,
            Self::BanksSwapped => -8,
            Self::WouldEraseOutsideStore => -9,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "pack is empty",
            Self::TooLarge => "pack is larger than the store",
            Self::NoSpace => "no run of free pages is long enough",
            Self::OutsideStore => "outside the glyph store",
            Self::CrossesStoreEnd => "crosses the end of the glyph store",
            Self::Unaligned => "not a whole quad-word",
            Self::FlashBusy => "an update owns the flash",
            Self::BanksSwapped => "flash banks are swapped",
            Self::WouldEraseOutsideStore => "refused: would erase outside the glyph store",
        }
    }
}

/// Pages a pack of `len` bytes occupies, its commit area included.
#[must_use]
pub const fn pages_for(len: usize) -> usize {
    (len + COMMIT_AREA).div_ceil(PAGE_SIZE)
}

/// How a write goes: erase `pages` first page first, program the pack at
/// [`WritePlan::pack_range`], then the commit record at the first page's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WritePlan {
    pub first: StorePage,
    pub pages: u8,
    /// The pack's length rounded up to a whole quad-word; padding is `0xFF`.
    pub padded_len: usize,
}

impl WritePlan {
    /// The pages to erase, in order.
    pub fn erase_order(&self) -> impl Iterator<Item = StorePage> + '_ {
        (0..usize::from(self.pages)).filter_map(|i| StorePage::nth(self.first.index() + i))
    }

    /// The pack's bytes, as offsets from the first page's address.
    #[must_use]
    pub const fn pack_range(&self) -> Range<usize> {
        COMMIT_AREA..COMMIT_AREA + self.padded_len
    }

    /// The commit record's place, as offsets from the first page's address.
    #[must_use]
    pub const fn commit_range(&self) -> Range<usize> {
        0..RECORD_LEN
    }
}

/// Plan a write of `len` bytes into the first run of free pages; `free[i]` for store page
/// `i`.
///
/// # Errors
/// The first [`Refusal`] that applies.
pub fn plan_write(len: usize, free: &[bool; PAGES]) -> Result<WritePlan, Refusal> {
    if len == 0 {
        return Err(Refusal::Empty);
    }
    if len > MAX_PACK {
        return Err(Refusal::TooLarge);
    }
    let need = pages_for(len);
    let mut run = 0;
    for (i, &f) in free.iter().enumerate() {
        run = if f { run + 1 } else { 0 };
        if run == need {
            let first = StorePage::nth(i + 1 - need).ok_or(Refusal::OutsideStore)?;
            let plan = WritePlan {
                first,
                pages: u8::try_from(need).map_err(|_| Refusal::TooLarge)?,
                padded_len: len.next_multiple_of(QUADWORD),
            };
            check(&plan)?;
            return Ok(plan);
        }
    }
    Err(Refusal::NoSpace)
}

/// The plan's own invariants, checked again before anything is erased.
///
/// # Errors
/// [`Refusal::CrossesStoreEnd`], [`Refusal::WouldEraseOutsideStore`] or
/// [`Refusal::Unaligned`].
pub fn check(plan: &WritePlan) -> Result<(), Refusal> {
    if plan.first.index() + usize::from(plan.pages) > PAGES {
        return Err(Refusal::CrossesStoreEnd);
    }
    if plan.erase_order().count() != usize::from(plan.pages) {
        return Err(Refusal::WouldEraseOutsideStore);
    }
    if !plan.padded_len.is_multiple_of(QUADWORD) || plan.pack_range().end > usize::from(plan.pages) * PAGE_SIZE {
        return Err(Refusal::Unaligned);
    }
    Ok(())
}

/// The commit record's bytes: two quad-words.
pub const RECORD_LEN: usize = 32;
/// The retired quad-word follows the record.
pub const RETIRED_RANGE: Range<usize> = RECORD_LEN..RECORD_LEN + QUADWORD;

const MAGIC: [u8; 4] = *b"WGS1";
pub const FORMAT: u8 = 1;

/// What makes a pack count: written last, after the pack has been read back whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// The pack's locale, from `l10n/locales.toml`.
    pub locale: u8,
    pub pages: u8,
    pub len: u32,
    /// Higher wins when two packs of a locale are both valid.
    pub sequence: u32,
    /// CRC-32 of the pack's `len` bytes.
    pub pack_crc: u32,
}

/// Why a commit record does not count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Never committed, or erased: all `0xFF`.
    Blank,
    Magic,
    Format,
    Crc,
    /// A length or page count that cannot be, or runs past the store's end.
    Length,
}

impl CommitRecord {
    #[must_use]
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[0..4].copy_from_slice(&MAGIC);
        b[4] = FORMAT;
        b[5] = self.pages;
        b[6] = self.locale;
        b[8..12].copy_from_slice(&self.len.to_le_bytes());
        b[12..16].copy_from_slice(&self.sequence.to_le_bytes());
        b[16..20].copy_from_slice(&self.pack_crc.to_le_bytes());
        let crc = crc32(&b[..28]);
        b[28..32].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// The record at the start of `first`.
    ///
    /// # Errors
    /// The first [`Reject`] that applies.
    pub fn decode(b: &[u8; RECORD_LEN], first: StorePage) -> Result<Self, Reject> {
        if b.iter().all(|&x| x == 0xFF) {
            return Err(Reject::Blank);
        }
        if b[0..4] != MAGIC {
            return Err(Reject::Magic);
        }
        let word = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        if crc32(&b[..28]) != word(28) {
            return Err(Reject::Crc);
        }
        if b[4] != FORMAT {
            return Err(Reject::Format);
        }
        let r = Self { pages: b[5], locale: b[6], len: word(8), sequence: word(12), pack_crc: word(16) };
        let len = r.len as usize;
        if b[7] != 0 || b[20..28].iter().any(|&x| x != 0) || len == 0 || len > MAX_PACK {
            return Err(Reject::Length);
        }
        if usize::from(r.pages) != pages_for(len) || first.index() + usize::from(r.pages) > PAGES {
            return Err(Reject::Length);
        }
        Ok(r)
    }
}

/// Whether a retired quad-word says the pack is retired: anything but erased is, since a
/// retirement cut short must not bring a pack back.
#[must_use]
pub fn retired(word: &[u8; QUADWORD]) -> bool {
    word.iter().any(|&b| b != 0xFF)
}

/// A committed pack the boot scan found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub first: StorePage,
    pub record: CommitRecord,
    pub retired: bool,
}

/// Whether `found[i]` is mounted: not retired, and no other unretired pack of its locale has
/// a higher sequence (or the same sequence and a lower first page).
#[must_use]
pub fn mounted(found: &[Found], i: usize) -> bool {
    let Some(me) = found.get(i) else { return false };
    !me.retired
        && found.iter().enumerate().all(|(j, other)| {
            j == i
                || other.retired
                || other.record.locale != me.record.locale
                || (other.record.sequence, core::cmp::Reverse(other.first))
                    < (me.record.sequence, core::cmp::Reverse(me.first))
        })
}

/// `FLASH_ECCR` names a double error in the store: the one NMI the image returns from.
#[must_use]
pub const fn nmi_in_store(eccr: u32) -> bool {
    let double = eccr & (1 << 31) != 0;
    let bank2 = eccr & (1 << 21) != 0;
    let system = eccr & (1 << 22) != 0;
    let offset = eccr & wrist_flash::survey::ADDR_MASK;
    double && bank2 && !system && offset >= GLYPH_STORE.start as u32 * PAGE_SIZE_U32 && offset <= 0x1F_FFF0
}

/// What a boot does before it reads the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootActions {
    /// A store page an interrupted erase or write was on, to erase unread.
    pub erase_interrupted: Option<StorePage>,
    /// The first page of the pack whose validation ended in a reset, to erase unread.
    pub erase_quarantined: Option<StorePage>,
    /// `OPSR` named an operation outside the store: reported, not acted on.
    pub interrupted_elsewhere: bool,
    /// `ECCR` holds an error in the store; its pack fails validation if it still does.
    pub ecc_in_store: bool,
}

/// From `FLASH_OPSR` and `FLASH_ECCR` at boot, and the quarantine register's page.
#[must_use]
pub fn boot_actions(opsr: u32, eccr: u32, quarantine: Option<u8>) -> BootActions {
    let code = opsr >> 29;
    let bank2 = opsr & (1 << 21) != 0;
    let system = opsr & (1 << 22) != 0;
    let page = (opsr & wrist_flash::survey::ADDR_MASK) / PAGE_SIZE_U32;
    let mut a = BootActions::default();
    if code != 0 {
        // Single write, burst write and page erase are the store's own operations.
        match (matches!(code, 1 | 3 | 4) && bank2 && !system).then(|| u8::try_from(page).ok().and_then(StorePage::new)) {
            Some(Some(p)) => a.erase_interrupted = Some(p),
            _ => a.interrupted_elsewhere = true,
        }
    }
    a.erase_quarantined = quarantine.and_then(StorePage::new);
    let in_store = eccr & (0b11 << 30) != 0
        && eccr & (1 << 21) != 0
        && eccr & (1 << 22) == 0
        && (eccr & wrist_flash::survey::ADDR_MASK) >= GLYPH_STORE.start as u32 * PAGE_SIZE_U32;
    a.ecc_in_store = in_store;
    a
}

/// Whether `bytes` are the pack `record` committed.
#[must_use]
pub fn valid(record: &CommitRecord, bytes: &[u8]) -> bool {
    bytes.len() == record.len as usize && crc32(bytes) == record.pack_crc
}
