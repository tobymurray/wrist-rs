//! A frame as two GPDMA linked lists on TIM3's ticks: `E` writes the data lines, `D` one
//! register a tick of TIM3's compares (`BCK`, `GEN`), `GCK`, `GSP` and TIM1's one pulse (`BSP`).

use crate::{timing, LINES, PAIRS};

/// TIM1's and TIM3's clock.
pub const TIMER_HZ: u32 = 48_000_000;

/// Timer cycles in at least `ns`.
#[must_use]
pub const fn cycles(ns: u32) -> u32 {
    (ns * (TIMER_HZ / 1_000_000)).div_ceil(1_000)
}

/// One TIM3 period is a tick, a `BCK` half-period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bus {
    pub half: u32,
    /// Cycles into a tick at which `BCK` toggles; `E` is requested at 0, `D` at `ctrl`.
    pub edge: u32,
    pub ctrl: u32,
}

impl Bus {
    #[must_use]
    pub const fn new(half: u32, edge: u32) -> Self {
        Self { half, edge, ctrl: 1 }
    }

    /// TIM1's compare and reload: `BSP` rises a tick after `D` starts TIM1 and falls halfway
    /// through `BCK`'s first label, which is two ticks long and begins three ticks after the
    /// start of the tick `D` writes in.
    #[must_use]
    pub const fn bsp(&self) -> (u32, u32) {
        let (min, max) = LATENCY.d_write;
        (self.half, 4 * self.half + self.edge - self.ctrl - u32::midpoint(min, max))
    }

    const fn exact(&self, ns: u32) -> u32 {
        cycles(ns).div_ceil(self.half)
    }

    /// Ticks covering `ns` between two of `D`'s GPIO writes, each the first of its item.
    const fn written(&self, ns: u32) -> u32 {
        let (min, max) = LATENCY.d_write;
        (cycles(ns) + max + LATENCY.d_link - min).div_ceil(self.half)
    }
}

/// What the bus assumes of the GPDMA and TIM1, in timer cycles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Latency {
    /// From TIM3's update to `E`'s write on port E.
    pub e_write: (u32, u32),
    /// From `D`'s request to its single-beat write.
    pub d_write: (u32, u32),
    /// Loading an item, added before its first transfer.
    pub e_link: u32,
    pub d_link: u32,
    /// From `D`'s write to TIM1's `CR1` to its counter starting.
    pub tim1: (u32, u32),
}

/// As `page bus probe` and `pins` measure them (`HARDWARE.md`, GPDMA).
pub const LATENCY: Latency = Latency { e_write: (3, 9), d_write: (11, 14), e_link: 2, d_link: 2, tim1: (0, 3) };
pub const BUS: Bus = Bus::new(26, 19);

/// Whether `bus` keeps Table 6-3-3's minimums at every latency in `lat`.
#[must_use]
pub const fn keeps(bus: &Bus, lat: &Latency) -> bool {
    let (min, max) = lat.e_write;
    keeps_timer(bus, lat) && bus.edge >= max + cycles(200) && bus.half - bus.edge + min >= cycles(200)
}

/// All of [`keeps`] but the data window: `BCK`, `BSP`, and both channels keeping up, an item
/// loaded straight after the last transfer of the one before.
#[must_use]
pub const fn keeps_timer(bus: &Bus, lat: &Latency) -> bool {
    let (h, edge, ns200) = (bus.half, bus.edge, cycles(200));
    let (rise, fall) = bus.bsp();
    let (early, late) = (bus.ctrl + lat.d_write.0 + lat.tim1.0, bus.ctrl + lat.d_write.1 + lat.tim1.1);
    let first = 3 * h + edge;
    h >= cycles(400)
        && edge < h
        && bus.ctrl + lat.d_write.1 <= h
        && lat.e_link + 2 * lat.e_write.1 <= 2 * h
        && lat.d_link + 2 * lat.d_write.1 <= 2 * h
        && first >= late + rise + ns200
        && early + fall >= first + ns200
        && first + 2 * h >= late + fall + ns200
}

const _: () = assert!(keeps(&BUS, &LATENCY), "tsRGB, thRGB, thwBCK, thsBSP, tlsBSP");

/// `thwGCK` over a written plane, `thwGEN`, and `thhGCK` (Table 6-3-2), each kept with a tick to spare.
const THW_GCK: u32 = 62_670;
const THW_GEN: u32 = 9_670;
const THH_GCK: u32 = 4_000;

pub const GPIOA_BSRR: u32 = 0x4202_0018;
pub const GPIOA_BRR: u32 = 0x4202_0028;
pub const GPIOB_BSRR: u32 = 0x4202_0418;
pub const GPIOB_BRR: u32 = 0x4202_0428;
pub const GPIOE_ODR: u32 = 0x4202_1014;
pub const TIM3_CCR1: u32 = 0x4000_0434;
pub const TIM3_CCR2: u32 = TIM3_CCR1 + 4;
pub const TIM3_CCR4: u32 = TIM3_CCR1 + 12;
pub const TIM1_CR1: u32 = 0x4001_2C00;
/// TIM1's `CR1`: one pulse, and counting.
pub const OPM: u16 = 1 << 3;
pub const CEN: u16 = 1;
pub const GCK: u16 = 1 << 4;
pub const GSP: u16 = 1 << 10;
/// A compare past the period: no toggle, and `PWM` mode 1 held high.
pub const OFF: u16 = 0xFFFF;

/// `GPDMA_CxLLR`'s update bits (RM0456 17.8.18): `E` reloads `BR1`, `SAR` and `LLR`; `D` also
/// `DAR` and `TR3`.
pub const E_UPDATE: u32 = (1 << 29) | (1 << 28) | (1 << 16);
pub const D_UPDATE: u32 = E_UPDATE | (1 << 27) | (1 << 26);
/// `GPDMA_CxBR1` (17.8.13): `DAR` falls by `TR3`'s offset after each transfer, or by `BR2`'s
/// after each block.
pub const DDEC: u32 = 1 << 29;
pub const BRDDEC: u32 = 1 << 31;
/// `D`'s `TR3` walking down the compares, and `BR2` stepping back after a one-transfer block.
pub const D_TR3: u32 = 4 << 16;
pub const D_BR2: u32 = 4 << 16;
/// `E`: half-words to port 1 on `tim3_upd_dma` (Table 137), one event at the end.
pub const E_TR1: u32 = (1 << 30) | (0b01 << 16) | (1 << 3) | 0b01;
pub const E_TR2: u32 = (0b11 << 30) | (1 << 10) | 0x41;
/// `D`: half-words read, written as zero-extended words on port 0, on `tim3_cc4_dma`.
pub const D_TR1: u32 = (0b10 << 16) | (1 << 3) | 0b01;
pub const D_TR2: u32 = (0b11 << 30) | (1 << 10) | 0x40;

/// A written plane's data: `BCK` 1, two quiet ticks before it and one after, the pairs, then
/// black for 122.
pub const RUN: usize = 4 + PAIRS + 1;
const RUN_TICKS: u32 = 125;
/// `D`'s `CCR1` item: a spare tick, then `BCK`'s first 123 edges with a tick out after the first;
/// the 124th follows on the compare held.
const SHIFT: u32 = 125;
/// Zeros a single quiet item reads; longer waits take more items.
const QUIET: u32 = 512;
/// The tables `D` reads, in half-words: `GCK` then zeros, `GSP` then zeros, zeros, the compare
/// sequences.
const T_GCK: usize = 0;
const T_GSP: usize = T_GCK + 1 + QUIET as usize;
const T_ZERO: usize = T_GSP + 1 + QUIET as usize;
const T_ON: usize = T_ZERO + QUIET as usize;
const T_SHIFT: usize = T_ON + 2;
const T_OFF: usize = T_SHIFT + RUN;
const T_GEN: usize = T_OFF + 4;
/// The `GEN` table: at most this many ticks high, then low.
const GEN_MAX: u32 = 64;
const HOLD_MAX: u32 = 64;
pub const TABLE_HALFWORDS: usize = T_GEN + (GEN_MAX + HOLD_MAX) as usize;
pub const DATA_HALFWORDS: usize = 2 * LINES * RUN;
const GCK_EDGES: u32 = 483;
const D_ITEMS: u32 = 5 * GCK_EDGES + 64;
const PLANES: u32 = 480;
const E_ITEMS: u32 = 2 * PLANES + 256;
pub const D_WORDS: usize = 5 * D_ITEMS as usize;
pub const E_WORDS: usize = 3 * E_ITEMS as usize;

const _: () = assert!(RUN_TICKS as usize == RUN && RUN == 125 && QUIET.is_multiple_of(2) && PLANES as usize == 2 * LINES);
const _: () = assert!(D_WORDS * 4 <= 1 << 16 && E_WORDS * 4 <= 1 << 16, "each list fits one LBAR window");

/// Where the program goes; `d_at` and `e_at` are 64 KiB-aligned, each channel's `LBAR`.
pub struct Memory<'a> {
    pub d: &'a mut [u32; D_WORDS],
    pub d_at: u32,
    pub e: &'a mut [u32; E_WORDS],
    pub e_at: u32,
    pub table: &'a mut [u16; TABLE_HALFWORDS],
    pub table_at: u32,
    pub data: &'a mut [u16; DATA_HALFWORDS],
    pub data_at: u32,
}

/// What `D` and `E` start from, and how long they run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub d_llr: u32,
    pub e_llr: u32,
    pub ticks: u32,
    pub lines: u32,
    /// `BCK` rising edges and `BSP` pulses the frame makes.
    pub rises: u32,
    pub pulses: u32,
}

impl Frame {
    /// Timer cycles from the start to the last write.
    #[must_use]
    pub const fn cycles(&self, bus: &Bus) -> u32 {
        (self.ticks + 1) * bus.half
    }
}

/// A list ran out of room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// Items laid end to end, each linking the next.
struct Chain {
    next: u32,
    end: u32,
}

/// The run of gate `line`'s plane, MSB first.
const fn run_at(line: usize, msb: bool) -> usize {
    (2 * line + if msb { 0 } else { 1 }) * RUN
}

/// Four-column groups [`fill`] works through.
pub const GROUPS: usize = LINES / 4;

/// Writes the runs of every line in `dirty` from `fb`, for the four-column groups in `groups`;
/// a clean line in a group gets a run too, which no list reads.
pub fn fill(data: &mut [u16; DATA_HALFWORDS], fb: Option<&[u8]>, dirty: &[bool; LINES], groups: core::ops::Range<usize>) {
    let w = LINES;
    let fb = fb.filter(|fb| fb.len() == w * w);
    for group in groups.map(|g| 4 * g) {
        // Columns `group` to `group + 3` are gate lines `top` down to `top - 3`, whose eight
        // runs lie together from the lowest line's.
        let top = LINES - 1 - group;
        if !(top - 3..=top).any(|l| dirty[l]) {
            continue;
        }
        let base = run_at(top - 3, true);
        let Ok(runs) = <&mut [u16; 8 * RUN]>::try_from(&mut data[base..base + 8 * RUN]) else { continue };
        match fb {
            Some(fb) => {
                for (pair, rows) in fb.chunks_exact(2 * w).enumerate() {
                    let word = |at: usize| rows[at..at + 4].try_into().map_or(0, u32::from_le_bytes);
                    let (odd, even) = (word(group), word(w + group));
                    for (shift, plane) in [(1, 0), (0, 1)] {
                        let (o, e) = ((odd >> shift) & 0x1515_1515, (even >> shift) & 0x1515_1515);
                        let low = (o & 0x00FF_00FF) | ((e & 0x00FF_00FF) << 8);
                        let high = ((o >> 8) & 0x00FF_00FF) | (e & 0xFF00_FF00);
                        #[allow(clippy::cast_possible_truncation)]
                        let columns = [low as u16, high as u16, (low >> 16) as u16, (high >> 16) as u16];
                        for (j, v) in columns.into_iter().enumerate() {
                            runs[(2 * (3 - j) + plane) * RUN + 4 + pair] = v;
                        }
                    }
                }
                for run in runs.as_chunks_mut::<RUN>().0 {
                    run[..4].fill(0);
                    run[RUN - 1] = 0;
                }
            }
            None => runs.fill(0),
        }
    }
}

/// Builds the lists edge by edge from runs [`fill`] wrote.
pub struct Builder<'a> {
    bus: Bus,
    mem: Memory<'a>,
    d: Chain,
    e: Chain,
    e_idle: u32,
    ticks: [u32; 2],
    frame: Frame,
    /// Ticks after `GSP`, `GCK` 1, a kept line and `GCK` 483.
    waits: [u32; 4],
    gen: u32,
    /// Ticks of the `GEN` item after it falls, then of quiet after that.
    hold: (u32, u32),
    full: bool,
}

impl<'a> Builder<'a> {
    #[must_use]
    pub fn new(bus: Bus, mem: Memory<'a>) -> Self {
        let gen = bus.exact(THW_GEN) + 1;
        let hold = (bus.exact(THH_GCK) + 1).max(bus.written(THW_GCK).saturating_sub(2 + 2 + SHIFT + 2 + gen + 1));
        let (mut near, mut far) = (hold.min(HOLD_MAX - 1), hold.saturating_sub(HOLD_MAX - 1));
        if far == 1 {
            (near, far) = (near - 1, 2);
        }
        let wait = |ns| bus.written(ns).max(2);
        let b = Self {
            bus,
            mem,
            d: Chain { next: 0, end: 5 * D_ITEMS },
            e: Chain { next: 0, end: 3 * E_ITEMS },
            e_idle: 0,
            ticks: [0; 2],
            frame: Frame { d_llr: 0, e_llr: 0, ticks: 0, lines: 0, rises: 0, pulses: 0 },
            waits: [timing::GSP_SETUP, timing::GCK_HALF, timing::GCK_FAST, 2 * timing::GCK_HALF].map(wait),
            gen,
            hold: (near, far),
            full: gen > GEN_MAX,
        };
        let c1 = u16::try_from(bus.edge).unwrap_or(OFF);
        let t = &mut *b.mem.table;
        t.fill(0);
        t[T_GCK] = GCK;
        t[T_GSP] = GSP;
        t[T_ON..T_ON + 2].copy_from_slice(&[OPM, OPM | CEN]);
        t[T_SHIFT..T_SHIFT + 3].copy_from_slice(&[OFF, c1, OFF]);
        t[T_SHIFT + 3..T_SHIFT + SHIFT as usize].fill(c1);
        t[T_OFF..T_OFF + 2].copy_from_slice(&[0, OFF]);
        t[T_GEN..T_GEN + gen as usize].fill(OFF);
        b
    }

    /// `INTB` is already high; `GSP`, `GCK` 1 and 2, then line 0's MSB plane.
    pub fn head(&mut self, dirty: &[bool; LINES]) {
        self.quiet(self.bus.exact(timing::GSP_SETUP) + 1);
        let [setup, half, ..] = self.waits;
        self.edge_write(GPIOB_BSRR, T_GSP, setup);
        self.edge_write(GPIOA_BSRR, T_GCK, half);
        self.edge_write(GPIOA_BRR, T_GCK, setup);
        if dirty[0] {
            self.plane(GPIOB_BRR, T_GSP, 0, true);
        } else {
            self.edge_write(GPIOB_BRR, T_GSP, half);
        }
    }

    /// `GCK` edge `edge`, 3 to 483, and what follows it.
    pub fn edge(&mut self, edge: u32, dirty: &[bool; LINES]) {
        let to = if edge % 2 == 1 { GPIOA_BSRR } else { GPIOA_BRR };
        let line = (edge as usize).wrapping_sub(2) / 2;
        match edge {
            3..=481 if dirty[line] => self.plane(to, T_GCK, line, edge.is_multiple_of(2)),
            GCK_EDGES => self.edge_write(to, T_GCK, self.waits[3]),
            _ => self.edge_write(to, T_GCK, self.waits[2]),
        }
    }

    /// # Errors
    /// [`Full`] if a list ran out of room.
    pub fn finish(mut self) -> Result<Frame, Full> {
        self.flush_idle();
        if self.full || self.ticks[0] != self.ticks[1] || self.d.next == 0 || self.e.next == 0 {
            return Err(Full);
        }
        self.mem.d[self.d.next as usize - 1] = 0;
        self.mem.e[self.e.next as usize - 1] = 0;
        self.frame.d_llr = D_UPDATE | (self.mem.d_at & 0xFFFC);
        self.frame.e_llr = E_UPDATE | (self.mem.e_at & 0xFFFC);
        self.frame.ticks = self.ticks[0];
        Ok(self.frame)
    }

    /// A `GCK` or `GSP` write, then quiet until `ns` after it.
    fn edge_write(&mut self, to: u32, table: usize, n: u32) {
        let head = match n {
            n if n <= QUIET + 1 => n,
            n if n - (QUIET + 1) < 2 => QUIET - 1,
            _ => QUIET + 1,
        };
        self.d_item(to, table, head, false);
        self.quiet_d(n - head);
        self.idle(n);
    }

    /// A written plane after its `GCK` (or `GSP`) write.
    fn plane(&mut self, to: u32, table: usize, line: usize, msb: bool) {
        if msb {
            self.frame.lines += 1;
        }
        self.frame.rises += 62;
        self.frame.pulses += 1;
        self.d_item(to, table, 2, false);
        self.d_item(TIM1_CR1, T_ON, 2, false);
        self.d_item(TIM3_CCR1, T_SHIFT, SHIFT, false);
        self.d_item(TIM3_CCR2, T_OFF, 2, true);
        let (gen, (near, far)) = (self.gen, self.hold);
        let hold = near + far;
        self.d_item(TIM3_CCR2, T_GEN, gen + 1 + near, false);
        self.quiet_d(far);
        self.idle(2 + 2 + SHIFT - RUN_TICKS - 1);
        self.flush_idle();
        let from = self.mem.data_at + 2 * u32::try_from(run_at(line, msb)).unwrap_or(0);
        self.e_push([2 * RUN_TICKS, from, 0]);
        self.ticks[0] += RUN_TICKS;
        self.idle(3 + gen + 1 + hold);
    }

    fn quiet(&mut self, n: u32) {
        self.quiet_d(n);
        self.idle(n);
    }

    /// `D` writing nothing for `n` ticks, through `GPIOA`'s `BSRR`.
    fn quiet_d(&mut self, mut n: u32) {
        while n > 0 {
            let take = if n > QUIET && n - QUIET < 2 { QUIET - 2 } else { n.min(QUIET) };
            self.d_item(GPIOA_BSRR, T_ZERO, take, false);
            n -= take;
        }
    }

    /// `n` of `table`'s half-words, one a tick: all to `to`, or to `to` and the registers below.
    fn d_item(&mut self, to: u32, table: usize, n: u32, down: bool) {
        let from = self.mem.table_at + 2 * u32::try_from(table).unwrap_or(0);
        // One block of `n` transfers, so the channel reads its sources ahead of the requests
        // rather than only on each (RM0456 17.4.11).
        let (br1, tr3) = if down { (DDEC | (2 * n), D_TR3) } else { (2 * n, 0) };
        self.ticks[1] += n;
        let (at, i) = (self.mem.d_at, self.d.next as usize);
        if self.d.next + 5 > self.d.end {
            self.full = true;
            return;
        }
        let d = &mut *self.mem.d;
        d[i] = br1;
        d[i + 1] = from;
        d[i + 2] = to;
        d[i + 3] = tr3;
        d[i + 4] = D_UPDATE | ((at + 4 * (self.d.next + 5)) & 0xFFFC);
        self.d.next += 5;
    }

    fn idle(&mut self, n: u32) {
        self.e_idle += n;
    }

    fn flush_idle(&mut self) {
        let mut n = core::mem::take(&mut self.e_idle);
        let zeros = self.mem.table_at + 2 * u32::try_from(T_ZERO).unwrap_or(0);
        while n > 0 {
            let take = if n > QUIET && n - QUIET < 2 { QUIET - 2 } else { n.min(QUIET) };
            self.e_push([2 * take, zeros, 0]);
            self.ticks[0] += take;
            n -= take;
        }
    }

    fn e_push(&mut self, [br1, from, _]: [u32; 3]) {
        let (at, i) = (self.mem.e_at, self.e.next as usize);
        if self.e.next + 3 > self.e.end {
            self.full = true;
            return;
        }
        let e = &mut *self.mem.e;
        e[i] = br1;
        e[i + 1] = from;
        e[i + 2] = E_UPDATE | ((at + 4 * (self.e.next + 3)) & 0xFFFC);
        self.e.next += 3;
    }
}

/// The whole frame for the gate lines in `dirty`.
///
/// # Errors
/// [`Full`] if a list ran out of room.
pub fn build(bus: Bus, mem: Memory<'_>, fb: Option<&[u8]>, dirty: &[bool; LINES]) -> Result<Frame, Full> {
    fill(mem.data, fb, dirty, 0..GROUPS);
    let mut b = Builder::new(bus, mem);
    b.head(dirty);
    for edge in 3..=GCK_EDGES {
        b.edge(edge, dirty);
    }
    b.finish()
}

#[cfg(test)]
mod sim;
