use wrist_flash::layout::bank1;
use wrist_flash::plan::{crc32, PAGE_SIZE, QUADWORD};
use wrist_store::{
    boot_actions, mounted, next_sequence, nmi_in_store, pages_for, plan_write, reclaimable, retired, scan, valid,
    BootActions, CommitRecord, Found, Head, Page, Refusal, Reject, StorePage, COMMIT_AREA, MAX_PACK, PAGES, RECORD_LEN,
    RETIRED_RANGE,
};

#[test]
fn only_store_pages_are_store_pages() {
    for p in 0..=255u8 {
        assert_eq!(StorePage::new(p).is_some(), (196..=255).contains(&p), "{p}");
    }
    let first = StorePage::new(196).unwrap();
    assert_eq!((first.index(), first.pnb(), first.address()), (0, 196, 0x0838_8000));
    assert_eq!(StorePage::nth(PAGES - 1).unwrap().address(), 0x083F_E000);
    assert_eq!(StorePage::nth(PAGES), None);
}

#[test]
fn every_length_fits_the_empty_store_up_to_the_maximum() {
    let blank = [Page::Blank; PAGES];
    assert_eq!(plan_write(0, &blank), Err(Refusal::Empty));
    for len in (1..=MAX_PACK).step_by(7).chain([MAX_PACK]) {
        let p = plan_write(len, &blank).unwrap();
        assert_eq!(p.first.index(), 0);
        assert_eq!(usize::from(p.pages), pages_for(len));
        assert!(p.pack_range().end <= usize::from(p.pages) * PAGE_SIZE, "{len}");
        assert_eq!(p.padded_len % QUADWORD, 0);
        assert!(p.padded_len >= len && p.padded_len < len + QUADWORD);
    }
    assert_eq!(plan_write(MAX_PACK + 1, &blank), Err(Refusal::TooLarge));
    assert_eq!(usize::from(plan_write(MAX_PACK, &blank).unwrap().pages), PAGES);
}

/// A deterministic stream for occupancy patterns.
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

#[test]
fn every_plan_stays_on_blank_store_pages_and_never_mirrors_the_boot_path() {
    let mut seed = 1;
    for _ in 0..2000 {
        let mut map = [Page::Blank; PAGES];
        for page in &mut map {
            if lcg(&mut seed).is_multiple_of(4) {
                *page = Page::Orphan;
            }
        }
        let len = usize::try_from(lcg(&mut seed)).unwrap() % (12 * PAGE_SIZE) + 1;
        match plan_write(len, &map) {
            Ok(p) => {
                let pages: Vec<StorePage> = p.erase_order().collect();
                assert_eq!(pages.len(), usize::from(p.pages));
                assert_eq!(pages[0], p.first, "the first page is erased first");
                for w in pages.windows(2) {
                    assert_eq!(w[1].index(), w[0].index() + 1);
                }
                for page in pages {
                    assert_eq!(map[page.index()], Page::Blank);
                    // Read as a bank 1 page, never the bootloader, a partition or the state.
                    assert!(u16::from(page.pnb()) >= bank1::UNUSED.start);
                }
            }
            Err(e) => {
                assert_eq!(e, Refusal::NoSpace);
                let need = pages_for(len);
                assert!(!map.windows(need).any(|w| w.iter().all(|&p| p == Page::Blank)), "a run was there");
            }
        }
    }
}

fn record() -> CommitRecord {
    CommitRecord { locale: 3, pages: 2, len: 9000, sequence: 7, pack_crc: 0x1234_5678 }
}

#[test]
fn records_round_trip_and_reject_every_flip_and_truncation() {
    let first = StorePage::new(200).unwrap();
    let r = record();
    let b = r.encode(first);
    assert_eq!(CommitRecord::decode(&b, first), Ok(r));
    for byte in 0..RECORD_LEN {
        for bit in 0..8 {
            let mut f = b;
            f[byte] ^= 1 << bit;
            assert!(CommitRecord::decode(&f, first).is_err(), "byte {byte} bit {bit}");
        }
    }
    // Programming cut short leaves the tail erased.
    for kept in 0..RECORD_LEN {
        let mut t = [0xFF; RECORD_LEN];
        t[..kept].copy_from_slice(&b[..kept]);
        assert!(CommitRecord::decode(&t, first).is_err(), "kept {kept}");
    }
    assert_eq!(CommitRecord::decode(&[0xFF; RECORD_LEN], first), Err(Reject::Blank));
    assert_eq!(CommitRecord::decode(&b, StorePage::new(201).unwrap()), Err(Reject::Page));
}

#[test]
fn impossible_lengths_are_rejected() {
    let at = |p| StorePage::new(p).unwrap();
    for bad in [
        CommitRecord { len: 0, pages: 1, ..record() },
        CommitRecord { len: 9000, pages: 3, ..record() },
        CommitRecord { len: u32::try_from(MAX_PACK).unwrap() + 1, pages: 60, ..record() },
    ] {
        assert_eq!(CommitRecord::decode(&bad.encode(at(196)), at(196)), Err(Reject::Length), "{bad:?}");
    }
    // Two pages from the last page would run past the end.
    assert_eq!(CommitRecord::decode(&record().encode(at(255)), at(255)), Err(Reject::Length));
    assert!(CommitRecord::decode(&record().encode(at(254)), at(254)).is_ok());
}

#[test]
fn a_retirement_cut_short_still_retires() {
    assert!(!retired(&[0xFF; QUADWORD]));
    assert!(retired(&[0; QUADWORD]));
    let mut half = [0xFF; QUADWORD];
    half[..8].fill(0);
    assert!(retired(&half));
}

fn map_of(packs: &[(usize, u8, u32, bool)]) -> [Page; PAGES] {
    let mut map = [Page::Blank; PAGES];
    for &(i, locale, sequence, retired) in packs {
        let first = StorePage::nth(i).unwrap();
        map[i] = Page::Pack(Found { first, record: CommitRecord { locale, sequence, ..record() }, retired });
        map[i + 1] = Page::Body;
    }
    map
}

#[test]
fn the_highest_sequence_of_each_locale_mounts() {
    let packs = [(0, 1, 4, false), (2, 1, 5, false), (4, 2, 9, false), (6, 2, 10, true), (8, 3, 1, false), (10, 3, 1, false)];
    let map = map_of(&packs);
    let m: Vec<bool> = packs.iter().map(|&(i, ..)| mounted(&map, i)).collect();
    assert_eq!(m, [false, true, true, false, true, false]);
    assert!(!mounted(&map, 1), "a body page is no pack");
    assert_eq!(next_sequence(&map), 11);
    assert_eq!(next_sequence(&[Page::Blank; PAGES]), 1);
}

#[test]
fn reclaiming_takes_orphans_and_every_page_of_a_pack_not_mounted() {
    let mut map = map_of(&[(0, 1, 4, false), (2, 1, 5, false), (4, 2, 9, true), (6, 3, 1, false)]);
    map[9] = Page::Orphan;
    let r = reclaimable(&map);
    let taken: Vec<usize> = (0..PAGES).filter(|&i| r[i]).collect();
    assert_eq!(taken, [0, 1, 4, 5, 9]);
}

#[test]
fn the_nmi_predicate_takes_exactly_the_store() {
    let eccd = 1 << 31;
    let bank2 = 1 << 21;
    let system = 1 << 22;
    assert!(nmi_in_store(eccd | bank2 | 0x18_8000));
    assert!(nmi_in_store(eccd | bank2 | 0x1F_FFF0));
    assert!(!nmi_in_store(eccd | bank2 | 0x18_7FF0));
    assert!(!nmi_in_store(eccd | 0x18_8000), "bank 1");
    assert!(!nmi_in_store(eccd | bank2 | system | 0x18_8000), "system flash");
    assert!(!nmi_in_store(bank2 | 0x18_8000), "corrected only");
}

/// `OPSR` naming `code` on a page of bank 1 or 2.
fn opsr(code: u32, bank2: bool, offset: u32) -> u32 {
    (code << 29) | (u32::from(bank2) << 21) | offset
}

#[test]
fn boot_erases_only_store_pages_it_was_told_of() {
    let page200 = 200 * 8192;
    for code in [1, 3, 4] {
        let a = boot_actions(opsr(code, true, page200), 0, None);
        assert_eq!(a.erase_interrupted, StorePage::new(200));
        assert!(!a.interrupted_elsewhere);
    }
    for o in [opsr(4, false, page200), opsr(4, true, 130 * 8192), opsr(3, false, 0x1000), opsr(7, true, page200), opsr(5, true, page200)] {
        let a = boot_actions(o, 0, None);
        assert_eq!(a.erase_interrupted, None, "{o:08X}");
        assert!(a.interrupted_elsewhere, "{o:08X}");
    }
    assert_eq!(boot_actions(0, 0, None), BootActions::default());
    assert_eq!(boot_actions(0, 0, Some(210)).erase_quarantined, StorePage::new(210));
    assert_eq!(boot_actions(0, 0, Some(150)).erase_quarantined, None);
    assert!(boot_actions(0, (1 << 31) | (1 << 21) | page200, None).ecc_in_store);
    assert!(boot_actions(0, (1 << 30) | (1 << 21) | page200, None).ecc_in_store);
    assert!(!boot_actions(0, (1 << 31) | (1 << 21) | (1 << 22) | page200, None).ecc_in_store);
}

/// The store as flash: erase sets a page to `0xFF`, a quad-word programs once, and a page a
/// reset tore raises ECC errors until it is erased.
#[derive(Clone)]
struct Flash(Vec<u8>, Option<StorePage>);

impl Flash {
    fn new() -> Self {
        Self(vec![0xFF; PAGES * PAGE_SIZE], None)
    }
    fn erase(&mut self, p: StorePage) {
        let at = p.index() * PAGE_SIZE;
        self.0[at..at + PAGE_SIZE].fill(0xFF);
        if self.1 == Some(p) {
            self.1 = None;
        }
    }
    fn program(&mut self, at: usize, bytes: &[u8]) {
        assert_eq!(at % QUADWORD, 0);
        assert_eq!(bytes.len() % QUADWORD, 0);
        assert!(self.0[at..at + bytes.len()].iter().all(|&b| b == 0xFF), "programmed twice at {at}");
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// What a boot finds, reading as the firmware does.
    fn map(&self) -> [Page; PAGES] {
        assert_eq!(self.1, None, "read a torn page");
        scan(|p| {
            let page = &self.0[p.index() * PAGE_SIZE..];
            if page[..PAGE_SIZE].iter().all(|&b| b == 0xFF) {
                return Head::Blank;
            }
            let Ok(record) = CommitRecord::decode(page[..RECORD_LEN].try_into().unwrap(), p) else { return Head::Other };
            if !valid(&record, &page[COMMIT_AREA..COMMIT_AREA + record.len as usize]) {
                return Head::Other;
            }
            Head::Pack { record, retired: retired(page[RETIRED_RANGE].try_into().unwrap()) }
        })
    }

    fn mounted(&self) -> Vec<(u8, u32)> {
        packs(&self.map())
    }

    /// What a boot does: erase the page `OPSR` names, unread.
    fn boot(&mut self, opsr: u32) {
        let a = boot_actions(opsr, 0, None);
        assert!(!a.interrupted_elsewhere);
        if let Some(p) = a.erase_interrupted {
            self.erase(p);
        }
    }
}

/// `(locale, pack CRC)` of each mounted pack.
fn packs(map: &[Page; PAGES]) -> Vec<(u8, u32)> {
    (0..PAGES)
        .filter_map(|i| match map[i] {
            Page::Pack(f) if mounted(map, i) => Some((f.record.locale, f.record.pack_crc)),
            _ => None,
        })
        .collect()
}

/// One operation on the flash.
#[derive(Clone)]
enum Step {
    Erase(StorePage),
    Program(usize, Vec<u8>),
}

impl Step {
    fn apply(&self, f: &mut Flash) {
        match self {
            Step::Erase(p) => f.erase(*p),
            Step::Program(at, bytes) => f.program(*at, bytes),
        }
    }

    /// Cut short by a reset: half done, the page or quad-word it stopped on left torn. The
    /// `OPSR` that leaves.
    fn tear(&self, f: &mut Flash) -> u32 {
        match self {
            Step::Erase(p) => {
                let at = p.index() * PAGE_SIZE;
                f.0[at..at + PAGE_SIZE / 2].fill(0xFF);
                f.1 = Some(*p);
                opsr(4, true, p.bank_offset())
            }
            Step::Program(at, bytes) => {
                let half = bytes.len() / 2 / QUADWORD * QUADWORD;
                f.0[*at..at + half].copy_from_slice(&bytes[..half]);
                for (d, s) in f.0[at + half..at + half + QUADWORD].iter_mut().zip(&bytes[half..]) {
                    *d &= s | 0x0F;
                }
                let page = StorePage::nth((at + half) / PAGE_SIZE).unwrap();
                f.1 = Some(page);
                opsr(1, true, page.bank_offset())
            }
        }
    }
}

/// Pack bytes per program step.
const CHUNK: usize = 4096;

/// The steps of writing `pack`, planned on `map` as the firmware would.
fn write(map: &[Page; PAGES], pack: &[u8], locale: u8) -> Vec<Step> {
    let plan = plan_write(pack.len(), map).unwrap();
    let base = plan.first.index() * PAGE_SIZE;
    let mut padded = pack.to_vec();
    padded.resize(plan.padded_len, 0xFF);
    let mut out: Vec<Step> = plan.erase_order().map(Step::Erase).collect();
    for (i, chunk) in padded.chunks(CHUNK).enumerate() {
        out.push(Step::Program(base + plan.pack_range().start + i * CHUNK, chunk.to_vec()));
    }
    let record = CommitRecord {
        locale,
        pages: plan.pages,
        len: u32::try_from(pack.len()).unwrap(),
        sequence: next_sequence(map),
        pack_crc: crc32(pack),
    }
    .encode(plan.first);
    // The record is two quad-words, programmed one at a time.
    for q in 0..2 {
        out.push(Step::Program(base + q * QUADWORD, record[q * QUADWORD..(q + 1) * QUADWORD].to_vec()));
    }
    out
}

fn retire(map: &[Page; PAGES], locale: u8) -> Vec<Step> {
    (0..PAGES)
        .filter_map(|i| match map[i] {
            Page::Pack(f) if f.record.locale == locale && mounted(map, i) => {
                Some(Step::Program(i * PAGE_SIZE + RETIRED_RANGE.start, vec![0; QUADWORD]))
            }
            _ => None,
        })
        .collect()
}

fn reclaim(map: &[Page; PAGES]) -> Vec<Step> {
    let r = reclaimable(map);
    (0..PAGES).filter(|&i| r[i]).map(|i| Step::Erase(StorePage::nth(i).unwrap())).collect()
}

fn run(flash: &mut Flash, steps: &[Step]) {
    for s in steps {
        s.apply(flash);
    }
}

/// After a boot, reclaiming leaves only mounted packs' pages, and `pack` can then be written.
fn recovers(mut f: Flash, pack: &[u8], locale: u8) {
    let steps = reclaim(&f.map());
    run(&mut f, &steps);
    let mut map = f.map();
    let held = (0..PAGES).filter(|&i| map[i] != Page::Blank).count();
    let owned: usize =
        (0..PAGES).filter(|&i| mounted(&map, i)).map(|i| if let Page::Pack(p) = map[i] { usize::from(p.record.pages) } else { 0 }).sum();
    assert_eq!(held, owned, "every page held belongs to a mounted pack");
    let before = packs(&map);
    if plan_write(pack.len(), &map) == Err(Refusal::NoSpace) {
        run(&mut f, &retire(&map, locale));
        let steps = reclaim(&f.map());
        run(&mut f, &steps);
        map = f.map();
    }
    run(&mut f, &write(&map, pack, locale));
    let mut want: Vec<(u8, u32)> = before.into_iter().filter(|&(l, _)| l != locale).collect();
    want.push((locale, crc32(pack)));
    let mut got = f.mounted();
    want.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, want);
}

/// Every cut in `steps`, between steps and inside each one, booted, with what it mounts.
fn every_cut(start: &Flash, steps: &[Step]) -> Vec<(usize, bool, Flash)> {
    let mut out = Vec::new();
    for cut in 0..=steps.len() {
        let mut f = start.clone();
        run(&mut f, &steps[..cut]);
        f.boot(0);
        out.push((cut, false, f));
        if let Some(s) = steps.get(cut) {
            let mut f = start.clone();
            run(&mut f, &steps[..cut]);
            let o = s.tear(&mut f);
            f.boot(o);
            out.push((cut, true, f));
        }
    }
    out
}

#[test]
fn a_write_cut_short_anywhere_leaves_the_old_pack_or_nothing() {
    let old: Vec<u8> = (0..20_000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let new: Vec<u8> = (0..30_001u32).map(|i| u8::try_from(i % 241).unwrap()).collect();
    let mut flash = Flash::new();
    run(&mut flash, &write(&[Page::Blank; PAGES], &old, 4));
    assert_eq!(flash.mounted(), [(4, crc32(&old))]);
    let all = write(&flash.map(), &new, 4);
    for (cut, torn, f) in every_cut(&flash, &all) {
        let want = if cut == all.len() { crc32(&new) } else { crc32(&old) };
        assert_eq!(f.mounted(), [(4, want)], "cut at {cut}, torn {torn}");
        recovers(f, &new, 4);
    }
}

#[test]
fn replacing_a_pack_in_a_full_store_never_shows_half_and_never_brings_the_old_back() {
    let old: Vec<u8> = vec![0x11; 20_000];
    let new: Vec<u8> = vec![0x22; 20_000];
    let filler = vec![0x33; MAX_PACK - pages_for(old.len()) * PAGE_SIZE];
    let mut flash = Flash::new();
    let steps = write(&flash.map(), &old, 4);
    run(&mut flash, &steps);
    let steps = write(&flash.map(), &filler, 9);
    run(&mut flash, &steps);
    assert_eq!(flash.mounted(), [(4, crc32(&old)), (9, crc32(&filler))]);
    assert_eq!(plan_write(new.len(), &flash.map()), Err(Refusal::NoSpace));

    // Retire the old pack, reclaim its pages, and write the new one there.
    let mut sim = flash.clone();
    let mut all = retire(&sim.map(), 4);
    run(&mut sim, &all);
    let more = reclaim(&sim.map());
    assert_eq!(more.len(), pages_for(old.len()));
    run(&mut sim, &more);
    all.extend(more);
    all.extend(write(&sim.map(), &new, 4));

    let mut old_gone = false;
    for (cut, torn, f) in every_cut(&flash, &all) {
        let seen = f.mounted();
        let fill = (9, crc32(&filler));
        if cut == all.len() {
            assert_eq!(seen, [(4, crc32(&new)), fill]);
        } else if seen == [fill] {
            old_gone = true;
        } else {
            assert!(!old_gone, "cut at {cut}, torn {torn}: the old pack came back");
            assert_eq!(seen, [(4, crc32(&old)), fill], "cut at {cut}, torn {torn}");
        }
        recovers(f, &new, 4);
    }
    assert!(old_gone);
}

#[test]
fn a_record_inside_a_pack_is_never_read() {
    // The image of a store page holding a small pack, as it would sit at store page `at`.
    let image = |at: usize| {
        let inner = vec![0x5A; 1000];
        let first = StorePage::nth(at).unwrap();
        let record = CommitRecord { locale: 7, pages: 1, len: 1000, sequence: 99, pack_crc: crc32(&inner) };
        let mut img = vec![0xFF; COMMIT_AREA];
        img[..RECORD_LEN].copy_from_slice(&record.encode(first));
        img.extend(inner);
        img
    };
    // The outer pack's bytes from `COMMIT_AREA` on start store page 1.
    for at in [1, 0] {
        let mut outer = vec![0x11; PAGE_SIZE - COMMIT_AREA];
        outer.extend(image(at));
        outer.resize(3 * PAGE_SIZE, 0x22);
        let mut f = Flash::new();
        let steps = write(&f.map(), &outer, 3);
        run(&mut f, &steps);
        assert_eq!(f.mounted(), [(3, crc32(&outer))], "the image for page {at}");
        // With the outer record gone, an image copied from another page still does not count.
        if at == 0 {
            f.erase(StorePage::nth(0).unwrap());
            assert_eq!(f.mounted(), []);
        }
    }
}
