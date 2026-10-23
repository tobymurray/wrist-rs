use wrist_flash::layout::bank1;
use wrist_flash::plan::{crc32, PAGE_SIZE, QUADWORD};
use wrist_store::{
    boot_actions, mounted, nmi_in_store, pages_for, plan_write, retired, valid, CommitRecord, Found, Refusal, Reject,
    StorePage, COMMIT_AREA, MAX_PACK, PAGES, RECORD_LEN, RETIRED_RANGE,
};

#[test]
fn only_store_pages_are_store_pages() {
    for p in 0..=255u8 {
        assert_eq!(StorePage::new(p).is_some(), (196..=255).contains(&p), "{p}");
    }
    let first = StorePage::new(196).unwrap();
    assert_eq!((first.index(), first.pnb(), first.address()), (0, 196, 0x0838_8000));
    assert!(first.bank2());
    assert_eq!(StorePage::nth(PAGES - 1).unwrap().address(), 0x083F_E000);
    assert_eq!(StorePage::nth(PAGES), None);
}

#[test]
fn every_length_fits_the_empty_store_up_to_the_maximum() {
    let free = [true; PAGES];
    assert_eq!(plan_write(0, &free), Err(Refusal::Empty));
    for len in (1..=MAX_PACK).step_by(7).chain([MAX_PACK]) {
        let p = plan_write(len, &free).unwrap();
        assert_eq!(p.first.index(), 0);
        assert_eq!(usize::from(p.pages), pages_for(len));
        assert!(p.pack_range().end <= usize::from(p.pages) * PAGE_SIZE, "{len}");
        assert_eq!(p.padded_len % QUADWORD, 0);
        assert!(p.padded_len >= len && p.padded_len < len + QUADWORD);
    }
    assert_eq!(plan_write(MAX_PACK + 1, &free), Err(Refusal::TooLarge));
    assert_eq!(usize::from(plan_write(MAX_PACK, &free).unwrap().pages), PAGES);
}

/// A deterministic stream for occupancy patterns.
fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

#[test]
fn every_plan_stays_on_free_store_pages_and_never_mirrors_the_boot_path() {
    let mut seed = 1;
    for _ in 0..2000 {
        let mut free = [false; PAGES];
        for f in &mut free {
            *f = !lcg(&mut seed).is_multiple_of(4);
        }
        let len = usize::try_from(lcg(&mut seed)).unwrap() % (12 * PAGE_SIZE) + 1;
        match plan_write(len, &free) {
            Ok(p) => {
                let pages: Vec<StorePage> = p.erase_order().collect();
                assert_eq!(pages.len(), usize::from(p.pages));
                assert_eq!(pages[0], p.first, "the first page is erased first");
                for w in pages.windows(2) {
                    assert_eq!(w[1].index(), w[0].index() + 1);
                }
                for page in pages {
                    assert!(free[page.index()]);
                    // Read as a bank 1 page, never the bootloader, a partition or the state.
                    assert!(u16::from(page.pnb()) >= bank1::UNUSED.start);
                }
            }
            Err(e) => {
                assert_eq!(e, Refusal::NoSpace);
                let need = pages_for(len);
                assert!(!free.windows(need).any(|w| w.iter().all(|&f| f)), "a run was there");
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
    let b = r.encode();
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
}

#[test]
fn impossible_lengths_are_rejected() {
    let at = |p| StorePage::new(p).unwrap();
    for bad in [
        CommitRecord { len: 0, pages: 1, ..record() },
        CommitRecord { len: 9000, pages: 3, ..record() },
        CommitRecord { len: u32::try_from(MAX_PACK).unwrap() + 1, pages: 60, ..record() },
    ] {
        assert_eq!(CommitRecord::decode(&bad.encode(), at(196)), Err(Reject::Length), "{bad:?}");
    }
    // Two pages from the last page would run past the end.
    assert_eq!(CommitRecord::decode(&record().encode(), at(255)), Err(Reject::Length));
    assert!(CommitRecord::decode(&record().encode(), at(254)).is_ok());
}

#[test]
fn a_retirement_cut_short_still_retires() {
    assert!(!retired(&[0xFF; QUADWORD]));
    assert!(retired(&[0; QUADWORD]));
    let mut half = [0xFF; QUADWORD];
    half[..8].fill(0);
    assert!(retired(&half));
}

#[test]
fn the_highest_sequence_of_each_locale_mounts() {
    let f = |page, locale, sequence, retired| Found {
        first: StorePage::new(page).unwrap(),
        record: CommitRecord { locale, sequence, ..record() },
        retired,
    };
    let found = [f(196, 1, 4, false), f(198, 1, 5, false), f(200, 2, 9, false), f(202, 2, 10, true), f(204, 3, 1, false), f(206, 3, 1, false)];
    let m: Vec<bool> = (0..found.len()).map(|i| mounted(&found, i)).collect();
    assert_eq!(m, [false, true, true, false, true, false]);
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

#[test]
fn boot_erases_only_store_pages_it_was_told_of() {
    let at = |code: u32, bank2: bool, offset: u32| (code << 29) | (u32::from(bank2) << 21) | offset;
    let page200 = 200 * 8192;
    let a = boot_actions(at(4, true, page200), 0, None);
    assert_eq!(a.erase_interrupted, StorePage::new(200));
    assert!(!a.interrupted_elsewhere);
    for opsr in [at(4, false, page200), at(4, true, 130 * 8192), at(3, false, 0x1000), at(7, true, page200), at(5, true, page200)] {
        let a = boot_actions(opsr, 0, None);
        assert_eq!(a.erase_interrupted, None, "{opsr:08X}");
        assert!(a.interrupted_elsewhere, "{opsr:08X}");
    }
    assert_eq!(boot_actions(0, 0, None), wrist_store::BootActions::default());
    assert_eq!(boot_actions(0, 0, Some(210)).erase_quarantined, StorePage::new(210));
    assert_eq!(boot_actions(0, 0, Some(150)).erase_quarantined, None);
    assert!(boot_actions(0, (1 << 31) | (1 << 21) | page200, None).ecc_in_store);
    assert!(!boot_actions(0, (1 << 31) | (1 << 21) | (1 << 22) | page200, None).ecc_in_store);
}

/// The store as flash: erase sets a page to `0xFF`, and a quad-word programs once.
struct Flash(Vec<u8>);

impl Flash {
    fn new() -> Self {
        Self(vec![0xFF; PAGES * PAGE_SIZE])
    }
    fn erase(&mut self, p: StorePage) {
        let at = p.index() * PAGE_SIZE;
        self.0[at..at + PAGE_SIZE].fill(0xFF);
    }
    fn program(&mut self, at: usize, bytes: &[u8]) {
        assert_eq!(at % QUADWORD, 0);
        assert_eq!(bytes.len() % QUADWORD, 0);
        assert!(self.0[at..at + bytes.len()].iter().all(|&b| b == 0xFF), "programmed twice at {at}");
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
    }
    fn page(&self, p: StorePage) -> &[u8] {
        &self.0[p.index() * PAGE_SIZE..]
    }

    /// What a boot mounts: `(locale, sequence)` of each valid, unretired, winning pack.
    fn scan(&self) -> Vec<(u8, u32)> {
        let mut found = Vec::new();
        for i in 0..PAGES {
            let p = StorePage::nth(i).unwrap();
            let page = self.page(p);
            let Ok(record) = CommitRecord::decode(page[..RECORD_LEN].try_into().unwrap(), p) else { continue };
            let bytes = &page[COMMIT_AREA..COMMIT_AREA + record.len as usize];
            if valid(&record, bytes) {
                let retired = retired(page[RETIRED_RANGE].try_into().unwrap());
                found.push(Found { first: p, record, retired });
            }
        }
        (0..found.len()).filter(|&i| mounted(&found, i)).map(|i| (found[i].record.locale, found[i].record.sequence)).collect()
    }
}

/// One step of a write, on the flash.
type Step = Box<dyn Fn(&mut Flash)>;

/// The steps of writing `pack`.
fn steps(flash: &Flash, pack: &[u8], locale: u8, sequence: u32) -> Vec<Step> {
    let mut free = [false; PAGES];
    for (i, f) in free.iter_mut().enumerate() {
        let p = StorePage::nth(i).unwrap();
        *f = flash.page(p)[..PAGE_SIZE].iter().all(|&b| b == 0xFF);
    }
    let plan = plan_write(pack.len(), &free).unwrap();
    let base = plan.first.index() * PAGE_SIZE;
    let mut padded = pack.to_vec();
    padded.resize(plan.padded_len, 0xFF);
    let mut out: Vec<Step> = Vec::new();
    for p in plan.erase_order() {
        out.push(Box::new(move |f: &mut Flash| f.erase(p)));
    }
    for (i, chunk) in padded.chunks(1024).enumerate() {
        let chunk = chunk.to_vec();
        let at = base + plan.pack_range().start + i * 1024;
        out.push(Box::new(move |f: &mut Flash| f.program(at, &chunk)));
    }
    let record = CommitRecord {
        locale,
        pages: plan.pages,
        len: u32::try_from(pack.len()).unwrap(),
        sequence,
        pack_crc: crc32(pack),
    }
    .encode();
    // The record is two quad-words, programmed one at a time.
    for q in 0..2 {
        let word = record[q * QUADWORD..(q + 1) * QUADWORD].to_vec();
        out.push(Box::new(move |f: &mut Flash| f.program(base + q * QUADWORD, &word)));
    }
    out
}

#[test]
fn a_write_cut_short_anywhere_leaves_the_old_pack_or_nothing() {
    let old: Vec<u8> = (0..20_000u32).map(|i| u8::try_from(i % 251).unwrap()).collect();
    let new: Vec<u8> = (0..30_001u32).map(|i| u8::try_from(i % 241).unwrap()).collect();
    let mut flash = Flash::new();
    for s in steps(&flash, &old, 4, 1) {
        s(&mut flash);
    }
    assert_eq!(flash.scan(), [(4, 1)]);
    let all = steps(&flash, &new, 4, 2);
    for cut in 0..=all.len() {
        let mut f = Flash(flash.0.clone());
        for s in &all[..cut] {
            s(&mut f);
        }
        let seen = f.scan();
        if cut == all.len() {
            assert_eq!(seen, [(4, 2)], "complete: the new pack wins");
        } else {
            assert_eq!(seen, [(4, 1)], "cut after {cut} of {} steps", all.len());
        }
    }
}

#[test]
fn rewriting_over_a_pack_loses_it_first_and_never_shows_half() {
    let old: Vec<u8> = vec![0x11; 50_000];
    let new: Vec<u8> = vec![0x22; 50_000];
    let mut flash = Flash::new();
    for s in steps(&flash, &old, 4, 1) {
        s(&mut flash);
    }
    // Fill the rest so the new pack must go over the old one's pages.
    let filler = vec![0x33; MAX_PACK - pages_for(old.len()) * PAGE_SIZE];
    for s in steps(&flash, &filler, 9, 1) {
        s(&mut flash);
    }
    assert_eq!(flash.scan(), [(4, 1), (9, 1)]);
    // Retire and erase the old pack to make room.
    for i in 0..pages_for(old.len()) {
        flash.erase(StorePage::nth(i).unwrap());
    }
    let all = steps(&flash, &new, 4, 2);
    for cut in 0..=all.len() {
        let mut f = Flash(flash.0.clone());
        for s in &all[..cut] {
            s(&mut f);
        }
        let want: &[(u8, u32)] = if cut == all.len() { &[(4, 2), (9, 1)] } else { &[(9, 1)] };
        assert_eq!(f.scan(), want, "cut after {cut}");
    }
}
