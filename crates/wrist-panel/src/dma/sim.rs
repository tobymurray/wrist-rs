//! The lists run on a model of GPDMA1, TIM3 and TIM1 as `HARDWARE.md` measures them, every pin's
//! edges checked against Tables 6-3-2 and 6-3-3, and what the panel latches.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::verbose_bit_mask,
    clippy::manual_range_contains
)]

extern crate std;

use std::boxed::Box;
use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::{
    cycles, Bus, Frame, Latency, Memory, BUS, DATA_HALFWORDS, D_BR2, D_TR1, D_TR2, D_TR3, D_WORDS, E_TR1, E_TR2,
    E_WORDS, CEN, GCK, GPIOA_BRR, GPIOA_BSRR, GPIOB_BRR, GPIOB_BSRR, GPIOE_ODR, GSP, LATENCY, OPM, TABLE_HALFWORDS, TIM1_CR1,
    TIM3_CR1,
};
use crate::{word, DATA_MASK, LINES, PAIRS};

const D_AT: u32 = 0x201A_0000;
const E_AT: u32 = 0x201B_0000;
const TABLE_AT: u32 = 0x201B_8000;
const DATA_AT: u32 = 0x201C_0000;
const TIM3: u32 = 0x4000_0400;
/// Bus words latched for `BCK` edges 2 to 124.
pub const LATCHED: usize = 123;
pub type Latched = BTreeMap<(usize, u32), [u16; LATCHED]>;

/// Byte-addressed memory the channels read.
#[derive(Default)]
pub struct Mem(Vec<(u32, Vec<u8>)>);

impl Mem {
    pub fn add_words(&mut self, at: u32, words: &[u32]) {
        self.0.push((at, words.iter().flat_map(|w| w.to_le_bytes()).collect()));
    }

    pub fn add_halves(&mut self, at: u32, halves: &[u16]) {
        self.0.push((at, halves.iter().flat_map(|h| h.to_le_bytes()).collect()));
    }

    fn read(&self, at: u32, width: u32) -> Result<u32, String> {
        for (base, bytes) in &self.0 {
            if let Some(o) = at.checked_sub(*base).map(|o| o as usize).filter(|&o| o + width as usize <= bytes.len()) {
                return Ok(bytes[o..o + width as usize].iter().rev().fold(0, |v, &b| (v << 8) | u32::from(b)));
            }
        }
        Err(format!("read of {width} bytes at {at:#010x}"))
    }
}

/// What requests a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Update,
    Cc4,
}

/// A channel as the firmware programs it: fixed registers, the first `LLR`, and its request.
#[derive(Clone, Copy, Debug)]
pub struct Channel {
    pub tr1: u32,
    pub tr2: u32,
    pub tr3: u32,
    pub br2: u32,
    pub dar: u32,
    pub lbar: u32,
    pub llr: u32,
    pub request: Request,
    pub link: u32,
    pub write: (u32, u32),
    /// The master ports of its list loads and of its writes.
    pub lap: usize,
    pub dap: usize,
}

/// One request's work: the writes, and whether an item was loaded before it.
#[derive(Clone, Debug)]
struct Service {
    writes: Vec<(u32, u32, u32)>,
    first: bool,
}

fn decode(ch: &Channel, mem: &Mem) -> Result<Vec<Service>, String> {
    let (tr1, tr2, mut tr3) = (ch.tr1, ch.tr2, ch.tr3);
    let (sdw, sinc, sbl) = (1 << (tr1 & 3), tr1 >> 3 & 1, (tr1 >> 4 & 0x3F) + 1);
    let (ddw, dinc, dbl, pam) = (1 << (tr1 >> 16 & 3), tr1 >> 19 & 1, (tr1 >> 20 & 0x3F) + 1, tr1 >> 11 & 3);
    if sbl != dbl || pam > 1 || tr2 >> 9 & 1 != 0 || tr2 >> 10 & 1 != 1 {
        return Err(format!("channel set up outside the model: TR1 {tr1:#x} TR2 {tr2:#x}"));
    }
    let (mut br1, mut sar, mut dar, mut br2, mut llr) = (0u32, 0u32, ch.dar, ch.br2, ch.llr);
    let mut out = Vec::new();
    loop {
        if br1 & 0xFFFF == 0 {
            if llr == 0 {
                return Ok(out);
            }
            let mut at = (ch.lbar & 0xFFFF_0000) | (llr & 0xFFFC);
            let next = |at: &mut u32| -> Result<u32, String> {
                let w = mem.read(*at, 4)?;
                *at += 4;
                Ok(w)
            };
            if llr >> 30 & 3 != 0 {
                return Err("an item reloads TR1 or TR2".into());
            }
            if llr >> 29 & 1 == 0 {
                return Err("an item without BR1".into());
            }
            br1 = next(&mut at)?;
            if llr >> 28 & 1 != 0 {
                sar = next(&mut at)?;
            }
            if llr >> 27 & 1 != 0 {
                dar = next(&mut at)?;
            }
            if llr >> 26 & 1 != 0 {
                tr3 = next(&mut at)?;
            }
            if llr >> 25 & 1 != 0 {
                br2 = next(&mut at)?;
            }
            llr = if llr >> 16 & 1 != 0 { next(&mut at)? } else { 0 };
            if br1 & 0xFFFF == 0 {
                return Err("an empty item".into());
            }
            let (bytes, blocks) = (br1 & 0xFFFF, (br1 >> 16 & 0x7FF) + 1);
            if bytes % (sdw * sbl) != 0 {
                return Err(format!("block of {bytes} bytes in bursts of {sbl}×{sdw}"));
            }
            let (sao, dao) = (tr3 & 0x1FFF, tr3 >> 16 & 0x1FFF);
            let mut first = true;
            for _ in 0..blocks {
                let mut left = bytes;
                while left > 0 {
                    let mut writes = Vec::new();
                    for _ in 0..sbl {
                        let v = mem.read(sar, sdw)?;
                        writes.push((dar, v, ddw));
                        sar = sar.wrapping_add(if sinc == 1 { sdw } else { 0 });
                        dar = dar.wrapping_add(if dinc == 1 { ddw } else { 0 });
                    }
                    left -= sdw * sbl;
                    sar = sar.wrapping_add(if br1 >> 28 & 1 == 1 { sao.wrapping_neg() } else { sao });
                    dar = dar.wrapping_add(if br1 >> 29 & 1 == 1 { dao.wrapping_neg() } else { dao });
                    out.push(Service { writes, first });
                    first = false;
                }
                let (brsao, brdao) = (br2 & 0xFFFF, br2 >> 16);
                sar = sar.wrapping_add(if br1 >> 30 & 1 == 1 { brsao.wrapping_neg() } else { brsao });
                dar = dar.wrapping_add(if br1 >> 31 & 1 == 1 { brdao.wrapping_neg() } else { brdao });
            }
            br1 = 0;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pin {
    Bck,
    Bsp,
    Gen,
    Gck,
    Gsp,
    Data,
}

/// A pin taking `level` somewhere in `early..=late`, in timer cycles from `CEN`.
#[derive(Clone, Copy, Debug)]
struct Change {
    pin: Pin,
    early: u64,
    late: u64,
    level: u16,
}

pub struct Setup {
    pub half: u32,
    /// `CCR1` to `CCR3` before the first update.
    pub idle: [u32; 3],
    pub ctrl: u32,
    /// TIM1's compare and reload: `BSP` rises and falls that long after `D` starts it.
    pub bsp: (u32, u32),
    pub tim1: (u32, u32),
    pub channels: Vec<Channel>,
    pub mem: Mem,
    pub ticks: u32,
    /// A channel's request that never reaches it, by channel and request.
    pub lost: Option<(usize, usize)>,
}

pub struct Run {
    pub latched: Latched,
    pub cycles: u64,
    pub rises: u32,
    pub pulses: u32,
    pub broken: Vec<String>,
    /// Transfers each channel never made.
    pub left: Vec<usize>,
}

/// TIM3 register `reg`, by the address a channel writes.
fn tim3_index(to: u32) -> Option<usize> {
    (to >= TIM3 + 0x34 && to <= TIM3 + 0x40 && to.is_multiple_of(4)).then(|| ((to - TIM3 - 0x34) / 4) as usize)
}

#[allow(clippy::too_many_lines)]
pub fn run(s: &Setup) -> Run {
    let h = u64::from(s.half);
    let mut broken = Vec::new();
    let mut changes = Vec::new();
    // Compare writes: (landing early, late, register, value).
    let mut compares: Vec<(u64, u64, usize, u32)> = Vec::new();
    let mut decoded = Vec::new();
    for (n, ch) in s.channels.iter().enumerate() {
        match decode(ch, &s.mem) {
            Ok(v) => {
                if v.len() != s.ticks as usize {
                    broken.push(format!("channel {n}: {} requests' work for {} ticks", v.len(), s.ticks));
                }
                if let Some(sv) = v.iter().find(|sv| sv.writes.len() > 1 && sv.writes.iter().any(|w| w.0 >> 17 == 0x2000)) {
                    broken.push(format!("channel {n}: a {}-beat burst to a timer never completes", sv.writes.len()));
                    decoded.push(Vec::new());
                } else {
                    decoded.push(v);
                }
            }
            Err(e) => {
                broken.push(e);
                decoded.push(Vec::new());
            }
        }
    }
    // Every channel's transfers in time, sharing the two master ports.
    let request = |n: usize, k: usize| {
        let k = (k + usize::from(s.lost.is_some_and(|(m, j)| m == n && k >= j))) as u64;
        match s.channels[n].request {
            Request::Update => (k + 1) * h,
            Request::Cc4 => k * h + u64::from(s.ctrl),
        }
    };
    let mut timing: Vec<Vec<(u64, u64)>> = decoded.iter().map(|v| Vec::with_capacity(v.len())).collect();
    let (mut next, mut free, mut port) = (vec![0usize; decoded.len()], vec![0u64; decoded.len()], [0u64; 2]);
    // When a write stopping TIM3 lands; no request comes after it.
    let mut stopped: Option<(u64, u64)> = None;
    loop {
        let ready = |n: usize| {
            let k = next[n];
            let sv = decoded[n].get(k)?;
            Some(if sv.first && k > 0 { free[n].min(request(n, k)) } else { request(n, k) })
        };
        let Some(n) = (0..decoded.len()).filter(|&n| ready(n).is_some()).min_by_key(|&n| ready(n)) else { break };
        let (ch, k) = (&s.channels[n], next[n]);
        if let Some((_, late)) = stopped.filter(|&(early, _)| request(n, k) >= early) {
            if request(n, k) <= late {
                broken.push(format!("channel {n}: TIM3 stopped across request {k}"));
            }
            next[n] = decoded[n].len();
            continue;
        }
        let mut at = free[n];
        if decoded[n][k].first && k > 0 {
            let start = at.max(port[ch.lap]);
            port[ch.lap] = start + u64::from(ch.link);
            at = port[ch.lap];
        }
        let start = request(n, k).max(at).max(port[ch.dap]);
        let done = start + u64::from(ch.write.1);
        port[ch.dap] = done;
        free[n] = done;
        timing[n].push((request(n, k) + u64::from(ch.write.0), done));
        if let [(TIM3_CR1, 0, _)] = decoded[n][k].writes[..] {
            stopped = Some((request(n, k) + u64::from(ch.write.0), done));
        }
        if done > request(n, k + 1) {
            broken.push(format!("channel {n}: request {} lost behind the last", k + 1));
        }
        next[n] += 1;
    }
    let last_done = free.iter().copied().max().unwrap_or(0);
    let left = decoded.iter().zip(&timing).map(|(v, t)| v.len() - t.len()).collect();
    let mut tim1_starts = Vec::new();
    for n in 0..s.channels.len() {
        let mut data = 0u16;
        for (sv, &(early, done)) in decoded[n].iter().zip(&timing[n]) {
            let (to, v, _) = sv.writes[0];
            let mut change = |pin, level| changes.push(Change { pin, early, late: done, level });
            match to {
                GPIOE_ODR => {
                    if v & !DATA_MASK != 0 {
                        broken.push(format!("port E driven with {v:#x}"));
                    }
                    if v as u16 != data {
                        data = v as u16;
                        change(Pin::Data, data);
                    }
                }
                _ if v == 0 && [GPIOA_BSRR, GPIOB_BSRR, GPIOA_BSRR + 2, GPIOB_BSRR + 2, GPIOA_BRR, GPIOB_BRR].contains(&to) => {}
                GPIOA_BSRR if v == u32::from(GCK) => change(Pin::Gck, 1),
                GPIOA_BRR if v == u32::from(GCK) => change(Pin::Gck, 0),
                a if a == GPIOA_BSRR + 2 && v == u32::from(GCK) => change(Pin::Gck, 0),
                GPIOB_BSRR if v == u32::from(GSP) => change(Pin::Gsp, 1),
                GPIOB_BRR if v == u32::from(GSP) => change(Pin::Gsp, 0),
                a if a == GPIOB_BSRR + 2 && v == u32::from(GSP) => change(Pin::Gsp, 0),
                TIM1_CR1 if v & u32::from(CEN) != 0 => tim1_starts.push((early, done)),
                TIM1_CR1 if v == u32::from(OPM) => {}
                TIM3_CR1 if v == 0 => {}
                a => match tim3_index(a) {
                    Some(3) if v == s.ctrl => {}
                    Some(i) if i < 3 => compares.push((early, done, i, v)),
                    _ => broken.push(format!("channel {n} writes {v:#x} to {a:#x}")),
                },
            }
        }
    }

    // TIM3: a compare written before an update holds from it.
    let ticks = u64::from(s.ticks) + 2;
    let mut active = vec![s.idle; ticks as usize + 1];
    compares.sort_by_key(|c| c.0);
    let mut by_tick: Vec<Vec<(usize, u32)>> = vec![Vec::new(); ticks as usize + 1];
    // A write of the value already there may land either side of an update.
    let mut last = s.idle;
    for (early, late, reg, v) in compares {
        let (te, tl) = (early / h + 1, late / h + 1);
        if (te != tl || late % h == 0) && v != last[reg] {
            broken.push(format!("a compare write lands across an update at {} ns", ns(early)));
        }
        last[reg] = v;
        if (te as usize) < by_tick.len() {
            by_tick[te as usize].push((reg, v));
        }
    }
    for t in 1..active.len() {
        active[t] = active[t - 1];
        for &(reg, v) in &by_tick[t] {
            active[t][reg] = v;
        }
    }
    let (mut bck, mut gen, mut trgo) = (0u16, 0u16, false);
    let (mut rises, mut pulses, mut tim1_until) = (0u32, 0u32, 0u64);
    for (t, &[c1, c2, c3]) in active.iter().enumerate() {
        let start = t as u64 * h;
        if c1 < s.half {
            bck ^= 1;
            rises += u32::from(bck);
            let at = start + u64::from(c1);
            changes.push(Change { pin: Pin::Bck, early: at, late: at, level: bck });
        }
        let level = u16::from(c2 >= s.half);
        if c2 != 0 && c2 < s.half {
            broken.push("GEN with a partial-tick compare".into());
        }
        if level != gen {
            gen = level;
            changes.push(Change { pin: Pin::Gen, early: start, late: start, level });
        }
        trgo |= c3 != 0;
    }
    if trgo {
        broken.push("TIM3 compare 3 written".into());
    }
    let (rise, fall) = s.bsp;
    tim1_starts.sort_unstable();
    for (early, late) in tim1_starts {
        let (a, b) = (early + u64::from(s.tim1.0), late + u64::from(s.tim1.1));
        if a < tim1_until {
            broken.push(format!("TIM1 started while running at {} ns", ns(a)));
            continue;
        }
        changes.push(Change { pin: Pin::Bsp, early: a + u64::from(rise), late: b + u64::from(rise), level: 1 });
        changes.push(Change { pin: Pin::Bsp, early: a + u64::from(fall), late: b + u64::from(fall), level: 0 });
        tim1_until = b + u64::from(fall);
        pulses += 1;
    }
    if bck != 0 || gen != 0 {
        broken.push(format!("BCK {bck}, GEN {gen} at the end"));
    }
    changes.sort_by_key(|c| (c.early, c.pin));
    let end = (u64::from(s.ticks) + 1) * h;
    let latched = check(&changes, end.max(last_done), &mut broken);
    Run { latched, cycles: end, rises, pulses, broken, left }
}

fn ns(cycles: u64) -> u64 {
    cycles * 1000 / 48
}

/// Tables 6-3-2 and 6-3-3 against `changes`, and what the panel latches.
fn check(changes: &[Change], end: u64, broken: &mut Vec<String>) -> Latched {
    let of = |pin| changes.iter().filter(move |c| c.pin == pin).copied().collect::<Vec<_>>();
    let (bck, bsp, gen, gck, gsp, data) = (of(Pin::Bck), of(Pin::Bsp), of(Pin::Gen), of(Pin::Gck), of(Pin::Gsp), of(Pin::Data));
    let at_least = |ns| u64::from(cycles(ns));
    let mut flag = |what: &str, at: u64| broken.push(format!("{what} at {} ns", ns(at)));
    let from = |pins: &[Change], at: u64| pins.partition_point(|c| c.early < at);

    for w in bck.windows(2) {
        if w[1].early - w[0].late < at_least(400) {
            flag("thwBCK/tlwBCK", w[1].early);
        }
    }
    let mut next = 0;
    for e in &bck {
        while next < data.len() && data[next].late + at_least(200) <= e.early {
            next += 1;
        }
        if next < data.len() && data[next].early < e.late + at_least(200) {
            flag("tsRGB/thRGB", e.early);
        }
    }
    for (i, rise) in bsp.iter().enumerate().filter(|(_, c)| c.level == 1) {
        let first = bck.get(from(&bck, rise.late));
        let (Some(first), Some(fall)) = (first, bsp.get(i + 1)) else {
            flag("BSP with no edge or no fall", rise.early);
            continue;
        };
        let second = bck.get(from(&bck, first.late + 1));
        if first.early < rise.late + at_least(200) || fall.early < first.late + at_least(200) {
            flag("thsBSP/tlsBSP", rise.early);
        }
        if second.is_none_or(|s| s.early < fall.late + at_least(200)) {
            flag("BSP low before the second edge", fall.early);
        }
    }
    for pulse in gen.chunks(2) {
        let [rise, fall] = pulse else {
            flag("GEN left high", pulse[0].early);
            continue;
        };
        if fall.early < rise.late + at_least(9_670) {
            flag("thwGEN", rise.early);
        }
        if bck.get(from(&bck, rise.early)).is_some_and(|e| e.early <= fall.late) {
            flag("tlsGEN: BCK during GEN", rise.early);
        }
        if gck.get(gck.partition_point(|g| g.late < fall.early)).is_none_or(|g| g.early < fall.late + at_least(4_000)) {
            flag("thhGCK", fall.early);
        }
    }
    if gck.len() != 483 || gck.iter().enumerate().any(|(i, g)| g.level != u16::from(i % 2 == 0)) {
        flag(&format!("{} GCK writes, not 483 alternating from high", gck.len()), 0);
    }
    if gsp.len() != 2 || gsp[0].level != 1 || gck.len() < 3 {
        flag("GSP is not one pulse", 0);
        return Latched::new();
    }
    if gsp[0].early < at_least(10_000) || gck[0].early < gsp[0].late + at_least(10_000) {
        flag("INTB to GSP, thsGSP", gsp[0].early);
    }
    if gsp[1].early < gck[1].late + at_least(10_000) || gsp[1].late > gck[2].early {
        flag("tlsGSP", gsp[1].early);
    }
    latch([&gck, &bck, &gen, &bsp, &data], end, &mut flag)
}

/// Each `GCK` half-period's `BCK` edges, and the bus at each where `GEN` follows.
fn latch([gck, bck, gen, bsp, data]: [&[Change]; 5], end: u64, flag: &mut impl FnMut(&str, u64)) -> Latched {
    let at_least = |ns| u64::from(cycles(ns));
    let from = |pins: &[Change], at: u64| pins.partition_point(|c| c.early < at);
    let level_at = |pins: &[Change], at: u64| match pins.partition_point(|c| c.late < at) {
        0 => 0,
        i => pins[i - 1].level,
    };
    let mut latched = Latched::new();
    for (i, g) in gck.iter().enumerate() {
        let until = gck.get(i + 1).map_or(end, |n| n.early);
        let (lo, hi) = (g.early + 1, until);
        let (edges, pulses, rises) =
            (&bck[from(bck, lo)..from(bck, hi)], &gen[from(gen, lo)..from(gen, hi)], &bsp[from(bsp, lo)..from(bsp, hi)]);
        let edge = i + 1;
        let normal = matches!(edge, 1 | 2 | 483) || !pulses.is_empty();
        let half = if normal { at_least(62_670) } else { at_least(1_000) };
        if until < g.late + half {
            flag(&format!("GCK {edge} half-period"), g.early);
        }
        if pulses.is_empty() {
            if !edges.is_empty() || !rises.is_empty() {
                flag(&format!("BCK or BSP after GCK {edge} with no GEN"), g.early);
            }
            continue;
        }
        if !(2..=481).contains(&edge) || edges.len() != 124 || pulses.len() != 2 || rises.len() != 2 {
            flag(&format!("GCK {edge}: {} BCK edges, {} GEN and {} BSP changes", edges.len(), pulses.len(), rises.len()), g.early);
            continue;
        }
        if rises[0].early < g.late {
            flag("thsGCK", rises[0].early);
        }
        let high: Vec<bool> = edges.iter().map(|e| level_at(bsp, e.early) == 1).collect();
        if !high[0] || high[1..].iter().any(|&h| h) {
            flag(&format!("GCK {edge}: BSP not high at exactly the first BCK edge"), g.early);
        }
        let mut words = [0u16; LATCHED];
        for (w, e) in words.iter_mut().zip(&edges[1..]) {
            *w = level_at(data, e.early);
        }
        latched.insert(((edge - 2) / 2, u32::from(edge % 2 == 0)), words);
    }
    latched
}

/// What `lcd.rs`'s bit-banged driver latches: each written line's planes, black past pair 119.
pub fn reference(fb: Option<&[u8]>, dirty: &[bool; LINES]) -> Latched {
    let mut out = Latched::new();
    for line in (0..LINES).filter(|&l| dirty[l]) {
        let x = LINES - 1 - line;
        for plane in [1, 0] {
            let mut words = [0u16; LATCHED];
            if let Some(fb) = fb.filter(|fb| fb.len() == LINES * LINES) {
                for (pair, w) in words.iter_mut().take(PAIRS).enumerate() {
                    let (odd, even) = (fb[2 * pair * LINES + x], fb[(2 * pair + 1) * LINES + x]);
                    *w = (word(odd, even, plane) & DATA_MASK) as u16;
                }
            }
            out.insert((line, plane), words);
        }
    }
    out
}

pub struct Program {
    pub frame: Frame,
    pub mem: Mem,
}

pub fn program(bus: Bus, fb: Option<&[u8]>, dirty: &[bool; LINES]) -> Program {
    let mut d: Box<[u32; D_WORDS]> = vec![0xDEAD_BEEF; D_WORDS].into_boxed_slice().try_into().unwrap();
    let mut e: Box<[u32; E_WORDS]> = vec![0xDEAD_BEEF; E_WORDS].into_boxed_slice().try_into().unwrap();
    let mut table: Box<[u16; TABLE_HALFWORDS]> = vec![0xBEEF; TABLE_HALFWORDS].into_boxed_slice().try_into().unwrap();
    let mut data: Box<[u16; DATA_HALFWORDS]> = vec![0xBEEF; DATA_HALFWORDS].into_boxed_slice().try_into().unwrap();
    let mem = Memory {
        d: &mut d,
        d_at: D_AT,
        e: &mut e,
        e_at: E_AT,
        table: &mut table,
        table_at: TABLE_AT,
        data: &mut data,
        data_at: DATA_AT,
    };
    crate::dma::fill(mem.data, fb, dirty, 0..crate::dma::GROUPS);
    let mut b = crate::dma::Builder::new(bus, mem);
    b.head(dirty);
    for edge in 3..=483 {
        b.edge(edge, dirty);
    }
    let frame = b.finish().expect("the lists hold the frame");
    let mut m = Mem::default();
    m.add_words(D_AT, &d[..]);
    m.add_words(E_AT, &e[..]);
    m.add_halves(TABLE_AT, &table[..]);
    m.add_halves(DATA_AT, &data[..]);
    Program { frame, mem: m }
}

/// The firmware's two channels for `p`, at latency `lat`.
pub fn setup(bus: Bus, lat: Latency, p: Program) -> Setup {
    let e = Channel {
        tr1: E_TR1,
        tr2: E_TR2,
        tr3: 0,
        br2: 0,
        dar: GPIOE_ODR,
        lbar: E_AT,
        llr: p.frame.e_llr,
        request: Request::Update,
        link: lat.e_link,
        write: lat.e_write,
        lap: 0,
        dap: 1,
    };
    let d = Channel {
        tr1: D_TR1,
        tr2: D_TR2,
        tr3: D_TR3,
        br2: D_BR2,
        dar: 0,
        lbar: D_AT,
        llr: p.frame.d_llr,
        request: Request::Cc4,
        link: lat.d_link,
        write: lat.d_write,
        lap: 0,
        dap: 0,
    };
    Setup {
        half: bus.half,
        idle: [super::OFF.into(), 0, 0],
        ctrl: bus.ctrl,
        bsp: bus.bsp(),
        tim1: lat.tim1,
        channels: vec![e, d],
        mem: p.mem,
        ticks: p.frame.ticks,
        lost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dma::{fill, keeps, E_LEFT, RUN};

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
    }

    fn random_fb(seed: u64) -> Vec<u8> {
        let mut r = Lcg(seed);
        (0..LINES * LINES).map(|_| r.next() as u8).collect()
    }

    fn checked(bus: Bus, lat: Latency, fb: Option<&[u8]>, dirty: &[bool; LINES]) -> Run {
        let p = program(bus, fb, dirty);
        let frame = p.frame;
        let out = run(&setup(bus, lat, p));
        assert!(out.broken.is_empty(), "{bus:?}: {:?}", &out.broken[..out.broken.len().min(8)]);
        let want = reference(fb, dirty);
        assert_eq!(out.latched.len(), want.len());
        for (k, v) in &want {
            assert_eq!(out.latched.get(k), Some(v), "line {} plane {}", k.0, k.1);
        }
        assert_eq!(frame.lines as usize, dirty.iter().filter(|&&d| d).count());
        assert_eq!((out.rises, out.pulses), (frame.rises, frame.pulses), "the counts the firmware checks");
        out
    }

    fn pattern(f: impl Fn(usize) -> bool) -> [bool; LINES] {
        core::array::from_fn(f)
    }

    /// `panel_dma::finish`'s verdict: what each channel has left once `D` is done, the counts,
    /// and `BCK` and `GEN` low.
    fn accepted(frame: &Frame, out: &Run) -> bool {
        out.left == [E_LEFT as usize, 0]
            && (out.rises, out.pulses) == (frame.rises, frame.pulses)
            && !out.broken.iter().any(|b| b.ends_with("at the end"))
    }

    #[test]
    fn a_lost_request_fails_the_frame() {
        let fb = random_fb(6);
        let dirty = pattern(|l| l % 4 != 3);
        let p = program(BUS, Some(&fb), &dirty);
        let frame = p.frame;
        let out = run(&setup(BUS, LATENCY, p));
        assert!(out.broken.is_empty() && accepted(&frame, &out), "{:?} {:?}", out.left, out.broken);
        // `D`'s in a quiet stretch: one in a run of compares makes a `BCK` edge the counts catch.
        for (channel, at) in [(0, 0), (0, frame.ticks as usize / 2), (1, 0), (1, 100)] {
            let mut s = setup(BUS, LATENCY, program(BUS, Some(&fb), &dirty));
            s.lost = Some((channel, at));
            let out = run(&s);
            assert_eq!((out.rises, out.pulses), (frame.rises, frame.pulses), "channel {channel} at {at}");
            assert!(!out.broken.iter().any(|b| b.ends_with("at the end")), "channel {channel} at {at}");
            assert_ne!(out.latched, reference(Some(&fb), &dirty), "channel {channel} at {at}");
            assert!(!accepted(&frame, &out), "channel {channel} at {at}: {:?}", out.left);
        }
    }

    #[test]
    fn every_dirty_pattern_latches_what_the_cpu_driver_does() {
        let fb = random_fb(1);
        let mut r = Lcg(7);
        let mut patterns = vec![
            pattern(|_| true),
            pattern(|_| false),
            pattern(|l| l % 2 == 0),
            pattern(|l| l % 2 == 1),
            pattern(|l| l == 0),
            pattern(|l| l == LINES - 1),
            pattern(|l| l == 0 || l == LINES - 1),
            pattern(|l| (100..140).contains(&l)),
        ];
        for density in [2, 8, 50, 90] {
            patterns.push(core::array::from_fn(|_| r.next() % 100 < density));
        }
        for dirty in &patterns {
            checked(BUS, LATENCY, Some(&fb), dirty);
        }
    }

    #[test]
    fn every_pair_on_both_planes() {
        let all = pattern(|_| true);
        for frame in 0..3 {
            let fb: Vec<u8> = (0..LINES * LINES)
                .map(|i| {
                    let (row, x) = (i / LINES, i % LINES);
                    let pair = (frame * LINES * PAIRS + x * PAIRS + row / 2) % 65_536;
                    (if row % 2 == 0 { pair >> 8 } else { pair }) as u8
                })
                .collect();
            checked(BUS, LATENCY, Some(&fb), &all);
        }
    }

    #[test]
    fn the_first_frame_is_black() {
        let run = checked(BUS, LATENCY, None, &pattern(|_| true));
        assert!(run.latched.values().all(|w| w.iter().all(|&w| w == 0)));
    }

    #[test]
    fn fill_matches_word_for_every_column() {
        let fb = random_fb(9);
        let mut r = Lcg(3);
        let dirty: [bool; LINES] = core::array::from_fn(|_| r.next().is_multiple_of(3));
        let mut data: Box<[u16; DATA_HALFWORDS]> = vec![0xAAAA_u16; DATA_HALFWORDS].into_boxed_slice().try_into().unwrap();
        fill(&mut data, Some(&fb), &dirty, 0..crate::dma::GROUPS);
        for line in (0..LINES).filter(|&l| dirty[l]) {
            let x = LINES - 1 - line;
            for (plane, at) in [(1, 2 * line * RUN), (0, (2 * line + 1) * RUN)] {
                let run = &data[at..at + RUN];
                assert_eq!(&run[..4], &[0; 4]);
                assert_eq!(run[RUN - 1], 0);
                for pair in 0..PAIRS {
                    let (odd, even) = (fb[2 * pair * LINES + x], fb[(2 * pair + 1) * LINES + x]);
                    assert_eq!(u32::from(run[4 + pair]), word(odd, even, plane) & DATA_MASK);
                }
            }
        }
    }

    #[test]
    fn a_bus_outside_its_latency_is_caught() {
        let fb = random_fb(3);
        let dirty = pattern(|l| l % 3 == 0);
        let cases = [
            (BUS, Latency { e_write: (2, 18), ..LATENCY }),
            (BUS, Latency { d_link: 40, ..LATENCY }),
            (BUS, Latency { e_link: 60, ..LATENCY }),
            (Bus::new(22, 16), LATENCY),
            (BUS, Latency { tim1: (0, 40), ..LATENCY }),
        ];
        for (bus, lat) in cases {
            assert!(!keeps(&bus, &lat), "{bus:?} {lat:?}");
            let out = run(&setup(bus, lat, program(bus, Some(&fb), &dirty)));
            assert!(!out.broken.is_empty(), "{bus:?} {lat:?}");
        }
    }

    #[test]
    fn the_lists_hold_any_frame() {
        for dirty in [pattern(|_| true), pattern(|l| l % 2 == 0), pattern(|l| l % 3 == 1), pattern(|_| false)] {
            for bus in [BUS, Bus::new(24, 17), Bus::new(48, 30)] {
                program(bus, None, &dirty);
            }
        }
    }

    #[test]
    fn faster_buses_keep_the_datasheet_at_their_latency() {
        let fb = random_fb(4);
        let lat = Latency { e_write: (4, 6), d_write: (8, 10), e_link: 10, d_link: 10, tim1: (2, 6) };
        for bus in [Bus::new(24, 17), Bus::new(22, 16)] {
            assert!(keeps(&bus, &lat), "{bus:?}");
            checked(bus, lat, Some(&fb), &pattern(|l| l % 5 != 0));
        }
    }

    #[test]
    fn frame_times() {
        let fb = random_fb(5);
        for (bus, most) in [(Bus::new(32, 24), 52.0), (Bus::new(28, 20), 46.0), (BUS, 44.0)] {
            assert!(keeps(&bus, &LATENCY), "{bus:?}");
            let ms = |dirty: [bool; LINES]| checked(bus, LATENCY, Some(&fb), &dirty).cycles as f64 / 48_000.0;
            let full = ms(pattern(|_| true));
            let page = ms(pattern(|l| (12..228).contains(&l)));
            let minute = ms(pattern(|l| (60..80).contains(&l) || (150..170).contains(&l)));
            std::println!("{bus:?}: full {full:.2} ms, 216 lines {page:.2} ms, 40 lines {minute:.2} ms");
            assert!(full < most && minute < 11.0);
        }
    }
}
