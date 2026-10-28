//! The panel's bus from TIM3, TIM1 and GPDMA1 channels 12 and 13, as `wrist_panel::dma` lays it
//! out, with TIM4 counting `BCK`'s rising edges and TIM2 `BSP`'s pulses.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_stm32::rcc::{StopMode, WakeGuard};
use embassy_time::{Duration, Instant, Timer};
use wrist_panel::dma::{self, Builder, Bus, Frame, Memory};
use wrist_panel::{timing, LINES};

use crate::lcd::Cost;
use crate::mmio::{modify32, read32, write32, GPIOA_BASE, GPIOB_BASE, GPIO_BSRR, GPIO_IDR, GPIO_MODER, RCC_BASE};
use crate::uart::Uart;

const RCC_AHB1ENR: u32 = RCC_BASE + 0x88;
const RCC_AHB1SMENR: u32 = RCC_BASE + 0xB0;
const RCC_AHB2SMENR1: u32 = RCC_BASE + 0xB4;
const RCC_AHB2ENR2: u32 = RCC_BASE + 0x90;
const RCC_AHB2SMENR2: u32 = RCC_BASE + 0xB8;
const RCC_APB1ENR1: u32 = RCC_BASE + 0x9C;
const RCC_APB1SMENR1: u32 = RCC_BASE + 0xC4;
const RCC_APB2ENR: u32 = RCC_BASE + 0xA4;
const RCC_APB2SMENR: u32 = RCC_BASE + 0xCC;
const GPDMA1EN: u32 = 1 << 0;
const SRAM5EN: u32 = 1 << 31;
/// TIM2, TIM3 and TIM4 in `APB1ENR1`, TIM1 in `APB2ENR`.
const TIM34EN: u32 = (1 << 0) | (1 << 1) | (1 << 2);
const TIM1EN: u32 = 1 << 11;
/// GPIOA, GPIOB and GPIOE in `AHB2SMENR1`.
const GPIO_SMEN: u32 = (1 << 0) | (1 << 1) | (1 << 4);

const TIM1: u32 = 0x4001_2C00;
const TIM3: u32 = 0x4000_0400;
const TIM4: u32 = 0x4000_0800;
const TIM2: u32 = 0x4000_0000;
const CR1: u32 = 0x00;
const CR2: u32 = 0x04;
const SMCR: u32 = 0x08;
const DIER: u32 = 0x0C;
const TSR: u32 = 0x10;
const EGR: u32 = 0x14;
const CCMR1: u32 = 0x18;
const CCMR2: u32 = 0x1C;
const CCER: u32 = 0x20;
const CNT: u32 = 0x24;
const PSC: u32 = 0x28;
const ARR: u32 = 0x2C;
const CCR1: u32 = 0x34;
const CCR2: u32 = 0x38;
const CCR3: u32 = 0x3C;
const CCR4: u32 = 0x40;
const BDTR: u32 = 0x44;
const CEN: u32 = 1 << 0;
const OPM: u32 = 1 << 3;
const UG: u32 = 1 << 0;
const UDE: u32 = 1 << 8;
const CC2DE: u32 = 1 << 10;
const CC3DE: u32 = 1 << 11;
const CC4DE: u32 = 1 << 12;
/// TIM3: `OC1M` toggle and `OC2M` `PWM` 1, both preloaded, `BCK`'s reference out as `TRGO`
/// (`MMS` 0100); `OC3M` and `OC4M` frozen.
const CCMR1_RUN: u32 = (0b011 << 4) | (1 << 3) | (0b110 << 12) | (1 << 11);
const CCMR2_RUN: u32 = 0;
const CCMR1_LOW: u32 = (0b100 << 4) | (0b100 << 12);
const CCMR2_LOW: u32 = 0b100 << 4;
const TIM3_MMS_OC1: u32 = 0b100 << 4;
/// `BCK` and `GEN` out; channel 3 stays inside.
const TIM3_CCER: u32 = (1 << 0) | (1 << 4);
/// TIM1: one pulse on `CH2N` (`PWM` 2) when `D` sets `CEN`, its update out as `TRGO`.
const TIM1_CCMR1: u32 = 0b111 << 12;
const TIM1_CCER: u32 = 1 << 6;
const TIM1_MMS_UPDATE: u32 = 0b010 << 4;
const MOE: u32 = 1 << 15;
/// TIM4 counts `tim3_trgo`'s rising edges (`ITR2`), TIM2 `tim1_trgo`'s (`ITR0`): external
/// clock mode 1 (RM0456 Table 563).
const TIM4_SMCR: u32 = (0b010 << 4) | 0b111;
const TIM2_SMCR: u32 = 0b111;

const GPDMA1: u32 = 0x4002_0000;
const E: u32 = 12;
const D: u32 = 13;
const R: u32 = 14;
const R2: u32 = 15;
const LBAR: u32 = 0x00;
const FCR: u32 = 0x0C;
const SR: u32 = 0x10;
const CR: u32 = 0x14;
const TR1: u32 = 0x40;
const TR2: u32 = 0x44;
const BR1: u32 = 0x48;
const SAR: u32 = 0x4C;
const DAR: u32 = 0x50;
const TR3: u32 = 0x54;
const BR2: u32 = 0x58;
const LLR: u32 = 0x7C;
const EN: u32 = 1 << 0;
const RESET: u32 = 1 << 1;
const SUSP: u32 = 1 << 2;
const PRIO_HIGH: u32 = 0b11 << 22;
const FLAGS: u32 = 0x7F << 8;
const SUSPF: u32 = 1 << 13;
/// `DTEF`, `ULEF`, `USEF`, `TOF`.
const ERRORS: u32 = (0b111 << 10) | (1 << 14);

const _: () = assert!(dma::E_TR2 & 0x7F == 65 && dma::D_TR2 & 0x7F == 64 && D >= 12);
const _: () = assert!(dma::TIM3_CCR1 == TIM3 + CCR1 && dma::TIM3_CCR4 == TIM3 + CCR4);

/// SRAM5's first pages, outside `memory.x`'s RAM.
const D_AT: u32 = 0x201A_0000;
const E_AT: u32 = 0x201B_0000;
const TABLE_AT: u32 = 0x201B_C000;
const DATA_AT: u32 = 0x201C_0000;
const SRAM5_TOP_PAGE: u32 = 0x2026_0000;
/// `D`'s list and sources, in SRAM2, which neither the CPU nor `E` touches during a frame.
const SRAM2_D_AT: u32 = 0x200C_0000;
const SRAM2_TABLE_AT: u32 = 0x200C_C400;
const SRAM2_END: u32 = 0x200D_0000;
const SRAM2EN: u32 = 1 << 30;
const _: () = assert!(4 * dma::D_WORDS <= (SRAM2_TABLE_AT - SRAM2_D_AT) as usize);
const _: () = assert!(2 * dma::TABLE_HALFWORDS <= (SRAM2_END - SRAM2_TABLE_AT) as usize);
const _: () = assert!(D_AT.is_multiple_of(0x1_0000) && E_AT.is_multiple_of(0x1_0000));
const _: () = assert!(4 * dma::D_WORDS <= (E_AT - D_AT) as usize && 4 * dma::E_WORDS <= (TABLE_AT - E_AT) as usize);
const _: () = assert!(2 * dma::TABLE_HALFWORDS <= (DATA_AT - TABLE_AT) as usize);
const _: () = assert!(2 * dma::DATA_HALFWORDS <= (SRAM5_TOP_PAGE - DATA_AT) as usize);

const BCK: (u32, u32) = (GPIOA_BASE, 6);
const GEN: (u32, u32) = (GPIOA_BASE, 7);
const BSP: (u32, u32) = (GPIOB_BASE, 0);
const INTB: (u32, u32) = (GPIOB_BASE, 1);
const AF_TIM3: u32 = 2;
const AF_TIM1: u32 = 1;
const GROUPS_PER_YIELD: usize = 8;
const EDGES_PER_YIELD: u32 = 64;

static OFF: AtomicBool = AtomicBool::new(false);
static BENCH: AtomicBool = AtomicBool::new(false);
/// The last bench frame's fill and list µs with interrupts masked, fill in the high half.
static LAST_MASKED: AtomicU32 = AtomicU32::new(0);
static PROBE: AtomicU32 = AtomicU32::new(0);
static HALF: AtomicU32 = AtomicU32::new(dma::BUS.half);
static EDGE: AtomicU32 = AtomicU32::new(dma::BUS.edge);
static FRAMES: AtomicU32 = AtomicU32::new(0);
static FAILED: AtomicU32 = AtomicU32::new(0);
static LAST_ERROR: AtomicU32 = AtomicU32::new(0);
static LAST_WHY: AtomicU32 = AtomicU32::new(0);
static LAST_TICKS: AtomicU32 = AtomicU32::new(0);
static LAST_EXPECTED_US: AtomicU32 = AtomicU32::new(0);
static LAST_SEEN_US: AtomicU32 = AtomicU32::new(0);
static LAST_FILL_CYCLES: AtomicU32 = AtomicU32::new(0);
static LAST_LIST_CYCLES: AtomicU32 = AtomicU32::new(0);
/// What the hardware counted against what the frame makes, `BCK` edges then `BSP` pulses.
static LAST_COUNTS: [AtomicU32; 4] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// Why a frame went back to the CPU.
const WHY_TIMEOUT: u32 = 1;
const WHY_DMA: u32 = 2;
const WHY_PINS: u32 = 3;
const WHY_COUNTS: u32 = 4;
const WHY_FULL: u32 = 5;

fn bus() -> Bus {
    Bus::new(HALF.load(Ordering::Relaxed), EDGE.load(Ordering::Relaxed))
}

pub fn usable() -> bool {
    !OFF.load(Ordering::Relaxed)
}

const fn ch(n: u32, reg: u32) -> u32 {
    GPDMA1 + 0x50 + 0x80 * n + reg
}

fn set((base, pin): (u32, u32), high: bool) {
    let bit = if high { 1 << pin } else { 1 << (pin + 16) };
    unsafe { write32(base + GPIO_BSRR, bit) };
}

fn is_high((base, pin): (u32, u32)) -> bool {
    unsafe { read32(base + GPIO_IDR) & (1 << pin) != 0 }
}

/// # Safety
/// Takes over TIM1 to TIM4, GPDMA1 channels 12 to 15, and `BCK`, `GEN` and `BSP` from
/// GPIO, whose outputs `lcd.rs` has already driven low.
pub unsafe fn init() {
    modify32(RCC_AHB1ENR, 0, GPDMA1EN);
    modify32(RCC_AHB2ENR2, 0, SRAM5EN);
    modify32(RCC_APB1ENR1, 0, TIM34EN);
    modify32(RCC_APB2ENR, 0, TIM1EN);
    // The bus runs while the core sleeps.
    modify32(RCC_AHB1SMENR, 0, GPDMA1EN);
    modify32(crate::mmio::RCC_AHB2ENR1, 0, SRAM2EN);
    modify32(RCC_AHB2SMENR1, 0, GPIO_SMEN | SRAM2EN);
    modify32(RCC_AHB2SMENR2, 0, SRAM5EN);
    modify32(RCC_APB1SMENR1, 0, TIM34EN);
    modify32(RCC_APB2SMENR, 0, TIM1EN);
    let _ = read32(RCC_APB2ENR);
    park();
    write32(TIM3 + CCER, TIM3_CCER);
    write32(TIM1 + CCER, TIM1_CCER);
    write32(TIM1 + BDTR, MOE);
    crate::lcd::set_alternate(BCK, AF_TIM3);
    crate::lcd::set_alternate(GEN, AF_TIM3);
    crate::lcd::set_alternate(BSP, AF_TIM1);
}

/// The timers stopped, every output low.
unsafe fn park() {
    write32(TIM3 + CR1, 0);
    write32(TIM3 + DIER, 0);
    write32(TIM3 + CCMR1, CCMR1_LOW);
    write32(TIM3 + CCMR2, CCMR2_LOW);
    write32(TIM1 + CR1, 0);
    write32(TIM1 + SMCR, 0);
    write32(TIM1 + CCMR1, 0b100 << 12);
    write32(TIM4 + CR1, 0);
    write32(TIM2 + CR1, 0);
}

/// Back to `lcd.rs`'s GPIO bus until `page bus` asks again.
fn give_up(why: u32) {
    LAST_WHY.store(why, Ordering::Relaxed);
    FAILED.fetch_add(1, Ordering::Relaxed);
    OFF.store(true, Ordering::Relaxed);
    unsafe {
        park();
        for (base, pin) in [BCK, GEN, BSP] {
            set((base, pin), false);
            modify32(base + GPIO_MODER, 0b11 << (pin * 2), 0b01 << (pin * 2));
        }
    }
}

fn take_back() {
    if OFF.swap(false, Ordering::Relaxed) {
        unsafe {
            crate::lcd::set_alternate(BCK, AF_TIM3);
            crate::lcd::set_alternate(GEN, AF_TIM3);
            crate::lcd::set_alternate(BSP, AF_TIM1);
        }
    }
}

/// # Safety
/// No transfer may be reading these pages.
unsafe fn memory() -> Memory<'static> {
    Memory {
        d: &mut *(D_AT as *mut [u32; dma::D_WORDS]),
        d_at: D_AT,
        e: &mut *(E_AT as *mut [u32; dma::E_WORDS]),
        e_at: E_AT,
        table: &mut *(TABLE_AT as *mut [u16; dma::TABLE_HALFWORDS]),
        table_at: TABLE_AT,
        data: &mut *(DATA_AT as *mut [u16; dma::DATA_HALFWORDS]),
        data_at: DATA_AT,
    }
}

/// A frame's memory, `D`'s list and sources in SRAM2.
///
/// # Safety
/// As [`memory`].
unsafe fn frame_memory() -> Memory<'static> {
    Memory {
        d: &mut *(SRAM2_D_AT as *mut [u32; dma::D_WORDS]),
        d_at: SRAM2_D_AT,
        table: &mut *(SRAM2_TABLE_AT as *mut [u16; dma::TABLE_HALFWORDS]),
        table_at: SRAM2_TABLE_AT,
        ..memory()
    }
}

/// One frame; `Err` if the hardware did not make it as built, after which `lcd.rs` writes it.
pub async fn scan(fb: Option<&[u8]>, dirty: &[bool; LINES]) -> Result<(u32, Cost), ()> {
    let bus = bus();
    let mut cost = Cost::default();
    let mut step = crate::busy::cycles();
    let mem = unsafe { frame_memory() };
    let d_at = mem.d_at;
    // A bench frame works in short steps with interrupts masked, so only its own cycles count.
    let bench = BENCH.swap(false, Ordering::Relaxed);
    let (per_yield, edges_per_yield) = if bench { (1, 8) } else { (GROUPS_PER_YIELD, EDGES_PER_YIELD) };
    let mut masked = [0u32; 2];
    let timed = |f: &mut dyn FnMut()| -> u32 {
        if bench {
            cortex_m::interrupt::free(|_| {
                let from = crate::busy::cycles();
                f();
                crate::busy::cycles().wrapping_sub(from)
            })
        } else {
            f();
            0
        }
    };
    let mut groups = 0;
    while groups < dma::GROUPS {
        let to = (groups + per_yield).min(dma::GROUPS);
        masked[0] += timed(&mut || dma::fill(mem.data, fb, dirty, groups..to));
        groups = to;
        cost.end_step(step);
        embassy_futures::yield_now().await;
        step = crate::busy::cycles();
    }
    LAST_FILL_CYCLES.store(cost.busy, Ordering::Relaxed);
    let filled = cost.busy;
    let mut b = Builder::new(bus, mem);
    masked[1] += timed(&mut || b.head(dirty));
    let mut edge = 3;
    while edge <= 483 {
        let to = (edge + edges_per_yield).min(484);
        masked[1] += timed(&mut || (edge..to).for_each(|e| b.edge(e, dirty)));
        edge = to;
        cost.end_step(step);
        embassy_futures::yield_now().await;
        step = crate::busy::cycles();
    }
    if bench {
        LAST_MASKED.store(((masked[0] / 48) << 16) | (masked[1] / 48).min(0xFFFF), Ordering::Relaxed);
    }
    let Ok(frame) = b.finish() else {
        give_up(WHY_FULL);
        return Err(());
    };
    LAST_LIST_CYCLES.store(cost.busy.wrapping_add(crate::busy::cycles().wrapping_sub(step)).wrapping_sub(filled), Ordering::Relaxed);
    let sleep = WakeGuard::new(StopMode::Stop1);
    let start = unsafe { begin(&bus, &frame, d_at) };
    let mut running = Running(true);
    cost.end_step(step);
    let result = finish(&bus, &frame, start).await;
    step = crate::busy::cycles();
    running.0 = false;
    drop(running);
    drop(sleep);
    cost.end_step(step);
    end_intb(start).await;
    if let Err(why) = result {
        give_up(why);
        return Err(());
    }
    FRAMES.fetch_add(1, Ordering::Relaxed);
    Ok((frame.lines, cost))
}

/// Stops the bus when dropped; `INTB` falls too if the frame was cut short.
struct Running(bool);

impl Drop for Running {
    fn drop(&mut self) {
        unsafe {
            abort();
            park();
        }
        if self.0 {
            set(INTB, false);
        }
    }
}

/// TIM3 ticking at `bus`, compares idle, outputs low, requests off.
unsafe fn timer(bus: &Bus) {
    park();
    write32(TIM3 + PSC, 0);
    write32(TIM3 + ARR, bus.half - 1);
    write32(TIM3 + CCR1, u32::from(dma::OFF));
    write32(TIM3 + CCR2, 0);
    write32(TIM3 + CCR3, 0);
    write32(TIM3 + CCR4, bus.ctrl);
    write32(TIM3 + CNT, 0);
    write32(TIM3 + EGR, UG);
    write32(TIM3 + CR2, TIM3_MMS_OC1);
    write32(TIM3 + CCMR1, CCMR1_RUN);
    write32(TIM3 + CCMR2, CCMR2_RUN);
    write32(TIM3 + TSR, 0);
}

unsafe fn load(n: u32, [tr1, tr2, tr3, br2]: [u32; 4], lbar: u32, dar: u32, llr: u32) {
    write32(ch(n, CR), RESET);
    write32(ch(n, FCR), FLAGS);
    write32(ch(n, LBAR), lbar & 0xFFFF_0000);
    write32(ch(n, TR1), tr1);
    write32(ch(n, TR2), tr2);
    write32(ch(n, BR1), 0);
    write32(ch(n, SAR), 0);
    write32(ch(n, DAR), dar);
    write32(ch(n, TR3), tr3);
    write32(ch(n, BR2), br2);
    write32(ch(n, LLR), llr);
    write32(ch(n, CR), PRIO_HIGH);
}

/// Loads the channels, TIM3, TIM1 and the counters, raises `INTB`, starts the timer.
unsafe fn begin(bus: &Bus, frame: &Frame, d_at: u32) -> Instant {
    timer(bus);
    let (rise, fall) = bus.bsp();
    write32(TIM1 + PSC, 0);
    write32(TIM1 + ARR, fall - 1);
    write32(TIM1 + CCR2, rise);
    write32(TIM1 + CR2, TIM1_MMS_UPDATE);
    write32(TIM1 + CNT, 0);
    write32(TIM1 + EGR, UG);
    write32(TIM1 + CCMR1, TIM1_CCMR1);
    write32(TIM1 + TSR, 0);
    write32(TIM1 + CR1, OPM);
    for (timer, smcr) in [(TIM4, TIM4_SMCR), (TIM2, TIM2_SMCR)] {
        write32(timer + PSC, 0);
        write32(timer + ARR, u32::MAX);
        write32(timer + SMCR, smcr);
        write32(timer + EGR, UG);
        write32(timer + CNT, 0);
        write32(timer + CR1, CEN);
    }
    load(E, [dma::E_TR1, dma::E_TR2, 0, 0], E_AT, dma::GPIOE_ODR, frame.e_llr);
    load(D, [dma::D_TR1, dma::D_TR2, 0, dma::D_BR2], d_at, 0, frame.d_llr);
    cortex_m::asm::dsb();
    for n in [E, D] {
        write32(ch(n, CR), PRIO_HIGH | EN);
    }
    write32(TIM3 + DIER, UDE | CC4DE);
    set(INTB, true);
    let start = Instant::now();
    write32(TIM3 + CR1, CEN);
    start
}

/// Asleep until the frame should be over, then until both channels are; then what the
/// hardware counted.
async fn finish(bus: &Bus, frame: &Frame, start: Instant) -> Result<(), u32> {
    let us = u64::from(frame.cycles(bus) / (dma::TIMER_HZ / 1_000_000));
    LAST_TICKS.store(frame.ticks, Ordering::Relaxed);
    LAST_EXPECTED_US.store(u32::try_from(us).unwrap_or(u32::MAX), Ordering::Relaxed);
    Timer::at(start + Duration::from_micros(us + 100)).await;
    let deadline = start + Duration::from_micros(2 * us + 5_000);
    let result = loop {
        let (e, d) = unsafe { (read32(ch(E, SR)), read32(ch(D, SR))) };
        if (e | d) & ERRORS != 0 {
            LAST_ERROR.store((d << 16) | (e & 0xFFFF), Ordering::Relaxed);
            break Err(WHY_DMA);
        }
        if unsafe { (read32(ch(E, CR)) | read32(ch(D, CR))) & EN == 0 } {
            break Ok(());
        }
        if Instant::now() >= deadline {
            LAST_ERROR.store((d << 16) | (e & 0xFFFF), Ordering::Relaxed);
            break Err(WHY_TIMEOUT);
        }
        Timer::after_micros(200).await;
    };
    LAST_SEEN_US.store(u32::try_from(start.elapsed().as_micros()).unwrap_or(u32::MAX), Ordering::Relaxed);
    // Past the last tick: the counters have seen everything.
    let (rises, pulses) = unsafe { (read32(TIM4 + CNT), read32(TIM2 + CNT)) };
    for (slot, v) in LAST_COUNTS.iter().zip([rises, frame.rises, pulses, frame.pulses]) {
        slot.store(v, Ordering::Relaxed);
    }
    result?;
    let pins = [BCK, GEN, BSP].into_iter().enumerate().fold(0, |m, (i, p)| m | (u32::from(is_high(p)) << i));
    if pins != 0 {
        LAST_ERROR.store(pins, Ordering::Relaxed);
        return Err(WHY_PINS);
    }
    if rises != frame.rises || pulses != frame.pulses {
        return Err(WHY_COUNTS);
    }
    Ok(())
}

/// Every channel suspended, then reset.
unsafe fn abort() {
    for n in [E, D, R, R2] {
        if read32(ch(n, CR)) & EN != 0 {
            modify32(ch(n, CR), 0, SUSP);
            for _ in 0..10_000 {
                if read32(ch(n, SR)) & SUSPF != 0 {
                    break;
                }
            }
        }
        write32(ch(n, CR), RESET);
    }
}

/// `INTB` low once it has been high `thwINTB`, then held low `tlwINTB`.
async fn end_intb(start: Instant) {
    Timer::at(start + Duration::from_millis(u64::from(timing::INTB_HIGH_MS))).await;
    set(INTB, false);
    let from = crate::busy::cycles();
    while crate::busy::cycles().wrapping_sub(from) < dma::cycles(timing::INTB_LOW) {}
}

pub fn command(args: &str, uart: Uart) {
    let mut words = args.split_whitespace();
    let first = words.next();
    if let Some(kind @ ("probe" | "pins")) = first {
        let half = words.next().and_then(|w| w.parse().ok()).unwrap_or(64);
        if !(24..=200).contains(&half) {
            return uart.write_line("[bus] usage: page bus probe|pins [HALF, 24 to 200 cycles]");
        }
        PROBE.store(half | if kind == "pins" { PINS } else { 0 }, Ordering::Relaxed);
        crate::wake_display();
        return uart.write_line("[bus] requested; the display task runs it between frames");
    }
    if first == Some("bench") {
        BENCH.store(true, Ordering::Relaxed);
        crate::wake_display();
        return bench(uart);
    }
    let mut nums = first.into_iter().chain(words).map(str::parse::<u32>);
    match (nums.next(), nums.next(), nums.next()) {
        (None, ..) => {}
        (Some(Ok(half)), Some(Ok(edge)), None) if dma::keeps_timer(&Bus::new(half, edge), &dma::LATENCY) => {
            HALF.store(half, Ordering::Relaxed);
            EDGE.store(edge, Ordering::Relaxed);
            take_back();
            crate::wake_display();
        }
        _ => return uart.write_line("[bus] usage: page bus [HALF EDGE | probe [HALF] | pins [HALF] | bench], in 48 MHz cycles"),
    }
    let bus = bus();
    uart.write_str(if usable() { "[bus] TIM1-4, GPDMA1 12-13" } else { "[bus] off: the CPU writes the bus" });
    uart.write_str(", tick ");
    uart.write_u32(bus.half);
    uart.write_str(" cycles, BCK at ");
    uart.write_u32(bus.edge);
    uart.write_line(if dma::keeps(&bus, &dma::LATENCY) { "; keeps the data window" } else { "; OUTSIDE the assumed data window" });
    uart.write_str("[bus] frames ");
    uart.write_u32(FRAMES.load(Ordering::Relaxed));
    uart.write_str(", failed ");
    uart.write_u32(FAILED.load(Ordering::Relaxed));
    uart.write_str(", last why ");
    uart.write_str(match LAST_WHY.load(Ordering::Relaxed) {
        0 => "-",
        WHY_TIMEOUT => "timeout",
        WHY_DMA => "dma error",
        WHY_PINS => "a pin left high",
        WHY_COUNTS => "counts",
        _ => "list full",
    });
    uart.write_reg(", detail ", LAST_ERROR.load(Ordering::Relaxed));
    uart.write_str("[bus] last frame ");
    uart.write_u32(LAST_TICKS.load(Ordering::Relaxed));
    uart.write_str(" ticks, ");
    uart.write_u32(LAST_EXPECTED_US.load(Ordering::Relaxed));
    uart.write_str(" us due, done by ");
    uart.write_u32(LAST_SEEN_US.load(Ordering::Relaxed));
    uart.write_str(" us; BCK rises ");
    uart.write_u32(LAST_COUNTS[0].load(Ordering::Relaxed));
    uart.write_str(" of ");
    uart.write_u32(LAST_COUNTS[1].load(Ordering::Relaxed));
    uart.write_str(", BSP pulses ");
    uart.write_u32(LAST_COUNTS[2].load(Ordering::Relaxed));
    uart.write_str(" of ");
    uart.write_u32(LAST_COUNTS[3].load(Ordering::Relaxed));
    uart.write_str("; runs filled in ");
    uart.write_u32(LAST_FILL_CYCLES.load(Ordering::Relaxed) / 48);
    uart.write_str(" us, lists in ");
    uart.write_u32(LAST_LIST_CYCLES.load(Ordering::Relaxed) / 48);
    let masked = LAST_MASKED.load(Ordering::Relaxed);
    uart.write_str(" us (interrupts counted); bench frame masked: filled in ");
    uart.write_u32(masked >> 16);
    uart.write_str(" us, lists in ");
    uart.write_u32(masked & 0xFFFF);
    uart.write_line(" us");
}

const PINS: u32 = 1 << 16;

/// The probe the console asked for, run between frames.
pub fn between_frames(uart: Uart) {
    match PROBE.swap(0, Ordering::Relaxed) {
        0 => {}
        v if v & PINS != 0 => pins(v & 0xFFFF, uart),
        v => probe(v, uart),
    }
}

/// Word stores with interrupts masked: SRAM5's data pages, then SRAM1, a cycle count a word.
fn bench(uart: Uart) {
    static mut SRAM1_WORDS: [u32; 1024] = [0; 1024];
    let time = |at: *mut u32| {
        cortex_m::interrupt::free(|_| {
            let from = crate::busy::cycles();
            for (i, v) in (0..1024u32).enumerate() {
                unsafe { at.add(i).write_volatile(v) };
            }
            crate::busy::cycles().wrapping_sub(from)
        })
    };
    let sram5 = time(DATA_AT as *mut u32);
    let sram1 = time(core::ptr::addr_of_mut!(SRAM1_WORDS).cast());
    uart.write_str("[bus] 1024 word stores: SRAM5 ");
    uart.write_u32(sram5);
    uart.write_str(" cycles, SRAM1 ");
    uart.write_u32(sram1);
    uart.write_line(" cycles; the next frame is timed with interrupts masked (`page bus` reports it)");
}

/// Probe rounds each channel runs: four `EGR` writes, then four that change nothing.
const PROBE_ROUNDS: u32 = 32;
const PROBE_SAMPLES: u32 = 4 * PROBE_ROUNDS;
const PROBE_D_TR2: u32 = (0b11 << 30) | (1 << 10) | 0x3E;
const PROBE_E_TR1: u32 = (1 << 30) | (0b10 << 16) | (1 << 3) | 0b01;
const RECORD_TR1: u32 = (0b10 << 16) | (1 << 19) | 0b10;

/// `E` and `D` with frame-shaped lists on a quiet bus, each write stamped by a TIM3 capture
/// that channels 14 and 15 copy out.
pub fn probe(half: u32, uart: Uart) {
    if !usable() {
        return uart.write_line("[bus] probe: the CPU has the bus; `page bus HALF EDGE` first");
    }
    let ctrl = 1;
    let mem = unsafe { memory() };
    let (list, values, stamps) = (mem.d, mem.table, mem.data);
    for (i, v) in [1 << 4, 0, 1 << 3, dma::OFF].into_iter().enumerate() {
        values[4 * i..4 * i + 4].fill(v);
    }
    stamps[..4 * PROBE_SAMPLES as usize].fill(0);
    let words = 5 * 2 * PROBE_ROUNDS;
    for (base, writes) in [(0, [(TIM3 + EGR, 0), (dma::GPIOE_ODR, 1)]), (words, [(TIM3 + EGR, 2), (TIM3 + CCR1, 3)])] {
        for k in 0..2 * PROBE_ROUNDS {
            let (to, value) = writes[(k % 2) as usize];
            let item = base + 5 * k;
            let next = if k + 1 == 2 * PROBE_ROUNDS { 0 } else { dma::D_UPDATE | ((D_AT + 4 * (item + 5)) & 0xFFFC) };
            let words = [dma::BRDDEC | (3 << 16) | 2, TABLE_AT + 8 * value, to, dma::D_TR3, next];
            list[item as usize..item as usize + 5].copy_from_slice(&words);
        }
    }
    let first = |item: u32| dma::D_UPDATE | ((D_AT + 4 * item) & 0xFFFC);
    let (left, errors) = unsafe {
        timer(&Bus::new(half, half / 2));
        // Channels 3 and 4 capture on `EGR` (`CC3S`, `CC4S` = 01); channel 2's compare
        // requests `D`; `BCK` stays forced low and `GEN` frozen low.
        write32(TIM3 + CR2, 0);
        write32(TIM3 + CCMR1, 0b100 << 4);
        write32(TIM3 + CCMR2, 0b01 | (0b01 << 8));
        write32(TIM3 + CCER, TIM3_CCER | (1 << 8) | (1 << 12));
        write32(TIM3 + CCR2, ctrl);
        load(E, [PROBE_E_TR1, dma::E_TR2, dma::D_TR3, dma::D_BR2], D_AT, 0, first(0));
        load(D, [dma::D_TR1, PROBE_D_TR2, dma::D_TR3, dma::D_BR2], D_AT, 0, first(words));
        for (n, request, from, at) in [(R, 64, TIM3 + CCR4, 0), (R2, 63, TIM3 + CCR3, PROBE_SAMPLES)] {
            load(n, [RECORD_TR1, request, 0, 0], 0, DATA_AT + 4 * at, 0);
            write32(ch(n, SAR), from);
            write32(ch(n, BR1), 4 * PROBE_SAMPLES);
        }
        cortex_m::asm::dsb();
        for n in [E, D, R, R2] {
            write32(ch(n, CR), PRIO_HIGH | EN);
        }
        write32(TIM3 + DIER, UDE | CC2DE | CC3DE | CC4DE);
        write32(TIM3 + CR1, CEN);
        let from = crate::busy::cycles();
        while crate::busy::cycles().wrapping_sub(from) < (8 * PROBE_ROUNDS + 4) * half {}
        let left = [E, D, R, R2].map(|n| (read32(ch(n, BR1)) & 0xFFFF) | (read32(ch(n, CR)) & EN));
        let errors = [E, D, R, R2].map(|n| read32(ch(n, SR)) & ERRORS);
        abort();
        park();
        write32(TIM3 + CCER, TIM3_CCER);
        (left, errors)
    };
    uart.write_str("[bus] probe at ");
    uart.write_u32(half);
    uart.write_str(" cycles: left in E, D and the recorders, then their errors:");
    for v in left.into_iter().chain(errors) {
        uart.write_str(" ");
        uart.write_hex32(v);
    }
    uart.write_line("");
    for (name, at, phase) in [("E (port 1, on the update)", 0, 0), ("D (port 0, on compare 2 at 1)", PROBE_SAMPLES, ctrl)] {
        let (mut lo, mut hi, mut seen) = ([u32::MAX; 2], [0; 2], [0; 2]);
        for k in 0..PROBE_SAMPLES {
            let i = 2 * (at + k) as usize;
            let v = u32::from(stamps[i]) | (u32::from(stamps[i + 1]) << 16);
            if v == 0 {
                continue;
            }
            let kind = usize::from(k % 4 != 0);
            let late = v.wrapping_sub(phase);
            lo[kind] = lo[kind].min(late);
            hi[kind] = hi[kind].max(late);
            seen[kind] += 1;
        }
        uart.write_str("[bus] ");
        uart.write_str(name);
        for (kind, label) in [(0, ": after a list load "), (1, "; steady ")] {
            uart.write_str(label);
            uart.write_u32(if seen[kind] == 0 { 0 } else { lo[kind] });
            uart.write_str("..");
            uart.write_u32(hi[kind]);
            uart.write_str(" cycles in ");
            uart.write_u32(seen[kind]);
        }
        uart.write_line("");
    }
}

const EXTI_EXTICR1: u32 = 0x4602_2000 + 0x60;
const EXTI_EXTICR2: u32 = EXTI_EXTICR1 + 4;
/// `EXTI_EXTICRm`'s port code for E (RM0456 23.6).
const EXTI_PORT_E: u32 = 4;
const PIN_SAMPLES: u32 = 64;
/// A channel stamping TIM3's counter on a trigger's rising edge: `SWREQ`, each burst
/// conditioned by a hit (`TRIGM` 11), rising (`TRIGPOL` 01).
const fn stamp_tr2(trigger: u32) -> u32 {
    (0b01 << 24) | (trigger << 16) | (0b11 << 14) | (1 << 9)
}

/// `E` toggling `R1` (`PE0`) every tick and `BCK` toggling on TIM3's compare, with channels 14
/// and 15 stamping TIM3's counter at each rising edge through EXTI 0 and 6.
pub fn pins(half: u32, uart: Uart) {
    if !usable() {
        return uart.write_line("[bus] pins: the CPU has the bus; `page bus HALF EDGE` first");
    }
    let edge = half / 2;
    let mem = unsafe { memory() };
    let (values, stamps) = (mem.table, mem.data);
    for (i, v) in values[..256].iter_mut().enumerate() {
        *v = u16::from(i % 2 == 0);
    }
    values[255] = 0;
    stamps[..4 * PIN_SAMPLES as usize].fill(0);
    let (cr1, cr2) = unsafe { (read32(EXTI_EXTICR1), read32(EXTI_EXTICR2)) };
    let (left, errors) = unsafe {
        write32(EXTI_EXTICR1, (cr1 & !0xFF) | EXTI_PORT_E);
        write32(EXTI_EXTICR2, cr2 & !(0xFF << 16));
        timer(&Bus::new(half, edge));
        write32(TIM3 + CR2, 0);
        write32(TIM3 + CCR1, edge);
        write32(TIM3 + EGR, UG);
        write32(TIM3 + TSR, 0);
        load(E, [dma::E_TR1, dma::E_TR2, 0, 0], 0, dma::GPIOE_ODR, 0);
        write32(ch(E, SAR), TABLE_AT);
        write32(ch(E, BR1), 2 * 256);
        for (n, trigger, at) in [(R, 0, 0), (R2, 6, PIN_SAMPLES)] {
            load(n, [RECORD_TR1, stamp_tr2(trigger), 0, 0], 0, DATA_AT + 4 * at, 0);
            write32(ch(n, SAR), TIM3 + CNT);
            write32(ch(n, BR1), 4 * PIN_SAMPLES);
        }
        cortex_m::asm::dsb();
        for n in [E, R, R2] {
            write32(ch(n, CR), PRIO_HIGH | EN);
        }
        write32(TIM3 + DIER, UDE);
        write32(TIM3 + CR1, CEN);
        let from = crate::busy::cycles();
        while crate::busy::cycles().wrapping_sub(from) < 260 * half {}
        let left = [E, R, R2].map(|n| (read32(ch(n, BR1)) & 0xFFFF) | (read32(ch(n, CR)) & EN));
        let errors = [E, R, R2].map(|n| read32(ch(n, SR)) & ERRORS);
        abort();
        park();
        write32(EXTI_EXTICR1, cr1);
        write32(EXTI_EXTICR2, cr2);
        write32(dma::GPIOE_ODR, 0);
        (left, errors)
    };
    let stamp = |at: u32, k: u32| {
        let i = 2 * (at + k) as usize;
        u32::from(stamps[i]) | (u32::from(stamps[i + 1]) << 16)
    };
    let range = |at: u32, less: u32| {
        (0..PIN_SAMPLES).map(|k| stamp(at, k).wrapping_sub(less)).fold((u32::MAX, 0), |(lo, hi), v| (lo.min(v), hi.max(v)))
    };
    let path = range(PIN_SAMPLES, edge);
    let r1 = range(0, 0);
    uart.write_str("[bus] pins at ");
    uart.write_u32(half);
    uart.write_str(" cycles: left in E and the recorders, then their errors:");
    for v in left.into_iter().chain(errors) {
        uart.write_str(" ");
        uart.write_hex32(v);
    }
    uart.write_line("");
    for (label, (lo, hi)) in [
        ("[bus] BCK to its stamp ", path),
        ("[bus] R1 stamped at ", r1),
        ("[bus] so E's write reaches port E at ", (r1.0.wrapping_sub(path.1), r1.1.wrapping_sub(path.0))),
    ] {
        uart.write_str(label);
        uart.write_u32(lo);
        uart.write_str("..");
        uart.write_u32(hi);
        uart.write_line(" cycles");
    }
}
