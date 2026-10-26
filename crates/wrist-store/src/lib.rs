#![no_std]
// The const fns take page numbers, which fit a u8, and cannot call `try_from`.
#![allow(clippy::cast_possible_truncation, clippy::cast_lossless)]

//! The glyph store's arithmetic: which pages a pack may use, the order a write goes in, the
//! commit record that makes a pack count, and what a boot must erase before it reads.

use core::cmp::Reverse;
use core::ops::Range;

use wrist_flash::layout::bank2::GLYPH_STORE;
use wrist_flash::plan::{crc32, BANK2_BASE, PAGE_SIZE, PAGE_SIZE_U32, QUADWORD};
use wrist_flash::survey::{ecc, interrupted, Area};

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
    /// No run of blank pages long enough.
    NoSpace,
    CrossesStoreEnd,
    /// An offset or length that is not a whole quad-word.
    Unaligned,
}

/// Pages a pack of `len` bytes occupies, its commit area included.
#[must_use]
pub const fn pages_for(len: usize) -> usize {
    (len + COMMIT_AREA).div_ceil(PAGE_SIZE)
}

/// How a write goes, so that one cut short leaves no valid record: erase `pages` first page
/// first, program the pack at [`WritePlan::pack_range`], then the commit record last.
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

/// Plan a write of `len` bytes into the first run of blank pages of `map`.
///
/// # Errors
/// The first [`Refusal`] that applies.
pub fn plan_write(len: usize, map: &[Page; PAGES]) -> Result<WritePlan, Refusal> {
    if len == 0 {
        return Err(Refusal::Empty);
    }
    if len > MAX_PACK {
        return Err(Refusal::TooLarge);
    }
    let need = pages_for(len);
    let mut run = 0;
    for (i, page) in map.iter().enumerate() {
        run = if *page == Page::Blank { run + 1 } else { 0 };
        if run == need {
            let Some(first) = StorePage::nth(i + 1 - need) else { break };
            let plan = WritePlan { first, pages: need as u8, padded_len: len.next_multiple_of(QUADWORD) };
            check(&plan)?;
            return Ok(plan);
        }
    }
    Err(Refusal::NoSpace)
}

/// The plan's own invariants, checked again before anything is erased.
///
/// # Errors
/// [`Refusal::CrossesStoreEnd`] or [`Refusal::Unaligned`].
pub fn check(plan: &WritePlan) -> Result<(), Refusal> {
    if plan.first.index() + usize::from(plan.pages) > PAGES {
        return Err(Refusal::CrossesStoreEnd);
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

/// What makes a pack count, written after the pack reads back whole. It names the store page
/// it was written for, so a page image copied elsewhere does not count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// The pack's locale, from `l10n/locales.toml`.
    pub locale: u8,
    pub pages: u8,
    pub len: u32,
    /// Higher wins when two packs of a locale are both valid; see [`next_sequence`].
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
    /// Written for another page.
    Page,
    /// A length or page count that cannot be, or runs past the store's end.
    Length,
}

impl CommitRecord {
    /// The record for the pack starting at `first`.
    #[must_use]
    pub fn encode(&self, first: StorePage) -> [u8; RECORD_LEN] {
        let mut b = [0u8; RECORD_LEN];
        b[0..4].copy_from_slice(&MAGIC);
        b[4] = FORMAT;
        b[5] = self.pages;
        b[6] = self.locale;
        b[7] = first.index() as u8;
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
        if usize::from(b[7]) != first.index() {
            return Err(Reject::Page);
        }
        let r = Self { pages: b[5], locale: b[6], len: word(8), sequence: word(12), pack_crc: word(16) };
        let len = r.len as usize;
        if b[20..28].iter().any(|&x| x != 0) || len == 0 || len > MAX_PACK {
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

/// Whether `bytes` are the pack `record` committed.
#[must_use]
pub fn valid(record: &CommitRecord, bytes: &[u8]) -> bool {
    bytes.len() == record.len as usize && crc32(bytes) == record.pack_crc
}

/// A committed pack the boot scan found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub first: StorePage,
    pub record: CommitRecord,
    pub retired: bool,
}

/// What the caller of [`scan`] reads at the start of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head {
    /// The whole page reads erased.
    Blank,
    /// A record that decodes, over a pack that is [`valid`], and its retired quad-word.
    Pack { record: CommitRecord, retired: bool },
    /// Anything else.
    Other,
}

/// A store page, as [`scan`] finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Blank,
    /// The first page of a valid pack.
    Pack(Found),
    /// A later page of the pack before it.
    Body,
    /// Neither blank nor part of a valid pack: a write, erase or retirement cut short.
    Orphan,
}

/// The store, first page to last. `read` is called for each page no valid pack before it
/// covers, so a pack's own bytes are never read as a record.
pub fn scan(mut read: impl FnMut(StorePage) -> Head) -> [Page; PAGES] {
    let mut map = [Page::Blank; PAGES];
    let mut i = 0;
    while let Some(first) = StorePage::nth(i) {
        match read(first) {
            Head::Blank => i += 1,
            Head::Other => {
                map[i] = Page::Orphan;
                i += 1;
            }
            Head::Pack { record, retired } => {
                let end = (i + usize::from(record.pages)).clamp(i + 1, PAGES);
                map[i] = Page::Pack(Found { first, record, retired });
                map[i + 1..end].fill(Page::Body);
                i = end;
            }
        }
    }
    map
}

/// Whether `map[i]` is a pack that is mounted: not retired, and no other unretired pack of its
/// locale has a higher sequence (or the same sequence and a lower first page).
#[must_use]
pub fn mounted(map: &[Page; PAGES], i: usize) -> bool {
    let Some(Page::Pack(me)) = map.get(i) else { return false };
    !me.retired
        && map.iter().all(|page| match page {
            Page::Pack(other) if other.first != me.first && !other.retired && other.record.locale == me.record.locale => {
                (other.record.sequence, Reverse(other.first)) < (me.record.sequence, Reverse(me.first))
            }
            _ => true,
        })
}

/// The pages to erase to free space, never while a write is under way: orphans, and every page
/// of a pack that is not mounted. Erased in page order, a pack loses its record first.
#[must_use]
pub fn reclaimable(map: &[Page; PAGES]) -> [bool; PAGES] {
    let mut out = [false; PAGES];
    let mut dead = false;
    for (i, page) in map.iter().enumerate() {
        out[i] = match page {
            Page::Blank => false,
            Page::Orphan => true,
            Page::Pack(_) => {
                dead = !mounted(map, i);
                dead
            }
            Page::Body => dead,
        };
    }
    out
}

/// The sequence for the next pack: past every pack in the store, so it outranks any left from
/// before, whatever the host sent then.
#[must_use]
pub fn next_sequence(map: &[Page; PAGES]) -> u32 {
    map.iter()
        .filter_map(|page| match page {
            Page::Pack(found) => Some(found.record.sequence),
            _ => None,
        })
        .max()
        .map_or(1, |s| s.saturating_add(1))
}

/// Whether a bank offset in `area` lies in the store.
const fn in_store(area: Area, offset: u32) -> bool {
    matches!(area, Area::Bank2) && offset >= GLYPH_STORE.start as u32 * PAGE_SIZE_U32
}

/// Whether `FLASH_ECCR` names a double error in the store.
#[must_use]
pub const fn nmi_in_store(eccr: u32) -> bool {
    match ecc(eccr) {
        Some(e) => e.double && in_store(e.area, e.offset),
        None => false,
    }
}

/// What a boot does before it reads the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BootActions {
    /// A store page an interrupted erase or write was on, to erase unread.
    pub erase_interrupted: Option<StorePage>,
    /// The first page of the pack whose validation ended in a reset, to erase unread; its
    /// other pages are then orphans.
    pub erase_quarantined: Option<StorePage>,
    /// `OPSR` named an operation outside the store: reported, not acted on.
    pub interrupted_elsewhere: bool,
    /// `ECCR` holds an error, corrected or not, in the store; its pack fails validation if it
    /// still does.
    pub ecc_in_store: bool,
}

/// From `FLASH_OPSR` and `FLASH_ECCR` at boot, and the quarantine register's page.
#[must_use]
pub fn boot_actions(opsr: u32, eccr: u32, quarantine: Option<u8>) -> BootActions {
    let mut a = BootActions::default();
    if let Some(op) = interrupted(opsr) {
        // Single write, burst write and page erase are the store's own operations.
        let store_op = matches!(op.code, 1 | 3 | 4) && op.bank2 && !op.system_flash;
        match u8::try_from(op.page()).ok().and_then(StorePage::new).filter(|_| store_op) {
            Some(p) => a.erase_interrupted = Some(p),
            None => a.interrupted_elsewhere = true,
        }
    }
    a.erase_quarantined = quarantine.and_then(StorePage::new);
    a.ecc_in_store = ecc(eccr).is_some_and(|e| in_store(e.area, e.offset));
    a
}
