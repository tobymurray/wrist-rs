//! The Sharp LS012B7DD06A memory LCD, LCP-2619063C Rev C: pixels bit-banged from GPIO a
//! column at a time on the cycle counter, `VCOM` from two LPTIMs on the LSE.
#![allow(clippy::inline_always)]

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_time::{Duration, Instant, Timer};
use wrist_panel::{timing, Clock, Pace};
use wrist_vcom::{Mode, Rate};

use crate::uart::Uart;

use crate::mmio::{
    enable_clock, modify32, read32, write32, GPIOA_BASE, GPIOB_BASE, GPIOD_BASE, GPIOE_BASE,
    GPIO_AFRH, GPIO_AFRL, GPIO_BSRR, GPIO_MODER, GPIO_OSPEEDR, GPIO_OTYPER, GPIO_PUPDR,
    LPTIM1_BASE, LPTIM3_BASE, LPTIM_ARR, LPTIM_ARROK, LPTIM_CCMR1, LPTIM_CCMR1_CC1E,
    LPTIM_CCMR1_CC1P_INVERTED, LPTIM_CCR1, LPTIM_CFGR, LPTIM_CMP1OK, LPTIM_CNT, LPTIM_CR,
    LPTIM_CR_CNTSTRT, LPTIM_CR_COUNTRST, LPTIM_CR_ENABLE, LPTIM_ICR, LPTIM_ISR, RCC_AHB2ENR1,
    RCC_AHB2ENR1_GPIOAEN, RCC_AHB2ENR1_GPIOBEN, RCC_AHB2ENR1_GPIODEN, RCC_AHB2ENR1_GPIOEEN,
    RCC_APB3ENR, RCC_APB3SMENR, RCC_APB3_LPTIM1, RCC_APB3_LPTIM3, RCC_SRDAMR,
};

// Control signals, panel name first, board net second.
const GSP: (u32, u32) = (GPIOB_BASE, 10); // VST
const GCK: (u32, u32) = (GPIOA_BASE, 4); // VCK
const GEN: (u32, u32) = (GPIOA_BASE, 7); // ENB
const INTB: (u32, u32) = (GPIOB_BASE, 1); // XRST
const BSP: (u32, u32) = (GPIOB_BASE, 0); // HST
const BCK: (u32, u32) = (GPIOA_BASE, 6); // HCK
const VCOM_VB: (u32, u32) = (GPIOB_BASE, 2); // FRP
const VA: (u32, u32) = (GPIOD_BASE, 14); // XFRP
const AF_LPTIM1_CH1: u32 = 1;
const AF_LPTIM3_CH1: u32 = 14;

pub const WIDTH: u32 = 240;
pub const HEIGHT: u32 = 240;
const LINES: usize = wrist_panel::LINES;
const PAIRS: usize = wrist_panel::PAIRS;
const GCK_EDGES: u32 = 483;
/// Kept lines fast-forwarded between yields.
const FAST_RUN: u32 = 32;

/// At `rcc_config`'s 48 MHz system clock.
const fn cycles(ns: u32) -> u32 {
    (ns * 48).div_ceil(1000)
}
const INTB_HIGH: Duration = Duration::from_millis(timing::INTB_HIGH_MS as u64);

/// LSE ticks a `VCOM` period: 0.5 Hz, Table 6-3-3's reflective minimum, and about 31 Hz,
/// its transmissive row, while the light is on.
const PERIOD_LOW: u32 = 65_536;
const PERIOD_HIGH: u32 = 1_057;
const LPTIM_POLLS: u32 = 20_000;

static VCOM_MODE: AtomicU8 = AtomicU8::new(Mode::Auto.as_u8());
static LIGHT_ON: AtomicBool = AtomicBool::new(false);
static VCOM_RUNNING: AtomicBool = AtomicBool::new(false);
/// In LSE ticks; 0 until `VCOM` starts.
static VCOM_PERIOD: AtomicU32 = AtomicU32::new(0);

pub fn note_light(on: bool) {
    if LIGHT_ON.swap(on, Ordering::Relaxed) != on {
        apply_vcom();
    }
}

pub fn light_on() -> bool {
    LIGHT_ON.load(Ordering::Relaxed)
}

pub fn set_vcom_mode(mode: Mode) {
    VCOM_MODE.store(mode.as_u8(), Ordering::Relaxed);
    apply_vcom();
}

fn vcom_period() -> u32 {
    match wrist_vcom::rate(Mode::from_u8(VCOM_MODE.load(Ordering::Relaxed)), light_on()) {
        Rate::Low => PERIOD_LOW,
        Rate::High => PERIOD_HIGH,
    }
}

/// Both timers to the period the light and mode ask for, restarted together so that
/// `FRP` and `XFRP` stay complementary.
fn apply_vcom() {
    if !VCOM_RUNNING.load(Ordering::Relaxed) {
        return;
    }
    let period = vcom_period();
    if VCOM_PERIOD.swap(period, Ordering::Relaxed) == period {
        return;
    }
    unsafe {
        for base in [LPTIM1_BASE, LPTIM3_BASE] {
            set_period(base, period);
        }
        for base in [LPTIM1_BASE, LPTIM3_BASE] {
            modify32(base + LPTIM_CR, 0, LPTIM_CR_COUNTRST);
        }
    }
}

/// # Safety
/// Takes over LPTIM1 and LPTIM3, whose kernel clocks `rcc_config` and the time driver put
/// on the LSE, and `FRP` and `XFRP`. Call once, after the first frame (Figure 6-2).
unsafe fn start_vcom() {
    let timers = RCC_APB3_LPTIM1 | RCC_APB3_LPTIM3;
    modify32(RCC_APB3ENR, 0, timers);
    // Clocked through Stop 2 (`HARDWARE.md`, "Stop modes").
    modify32(RCC_SRDAMR, 0, timers);
    modify32(RCC_APB3SMENR, 0, timers);
    let _ = read32(RCC_APB3ENR);
    let period = vcom_period();
    for (base, polarity) in [(LPTIM1_BASE, 0), (LPTIM3_BASE, LPTIM_CCMR1_CC1P_INVERTED)] {
        write32(base + LPTIM_CR, 0);
        write32(base + LPTIM_CFGR, 0);
        write32(base + LPTIM_CCMR1, LPTIM_CCMR1_CC1E | polarity);
        write32(base + LPTIM_CR, LPTIM_CR_ENABLE);
        set_period(base, period);
    }
    set_alternate(VCOM_VB, AF_LPTIM1_CH1);
    set_alternate(VA, AF_LPTIM3_CH1);
    for base in [LPTIM1_BASE, LPTIM3_BASE] {
        modify32(base + LPTIM_CR, 0, LPTIM_CR_CNTSTRT);
    }
    VCOM_PERIOD.store(period, Ordering::Relaxed);
    VCOM_RUNNING.store(true, Ordering::Relaxed);
}

/// # Safety
/// The timer at `base` must be enabled.
unsafe fn set_period(base: u32, period: u32) {
    write_acknowledged(base, LPTIM_ARR, period - 1, LPTIM_ARROK);
    write_acknowledged(base, LPTIM_CCR1, period / 2, LPTIM_CMP1OK);
}

/// A write the timer acknowledges once its LSE clock has taken it; a stale acknowledgement
/// is cleared first (`vendor/embassy-stm32/WRIST-PATCHES.md`).
unsafe fn write_acknowledged(base: u32, reg: u32, value: u32, ok: u32) {
    write32(base + LPTIM_ICR, ok);
    write32(base + reg, value);
    for _ in 0..LPTIM_POLLS {
        if read32(base + LPTIM_ISR) & ok != 0 {
            break;
        }
    }
    write32(base + LPTIM_ICR, ok);
}

pub(crate) unsafe fn set_alternate((base, pin): (u32, u32), af: u32) {
    let (afr, shift) = if pin < 8 { (GPIO_AFRL, pin * 4) } else { (GPIO_AFRH, (pin - 8) * 4) };
    modify32(base + afr, 0xF << shift, af << shift);
    modify32(base + GPIO_MODER, 0b11 << (pin * 2), 0b10 << (pin * 2));
}

/// Read until two reads agree: the counter runs on the LSE.
fn counter(base: u32) -> u32 {
    let mut last = unsafe { read32(base + LPTIM_CNT) };
    for _ in 0..8 {
        let now = unsafe { read32(base + LPTIM_CNT) };
        if now == last {
            break;
        }
        last = now;
    }
    last
}

pub fn write_vcom_status(uart: Uart) {
    let mode = Mode::from_u8(VCOM_MODE.load(Ordering::Relaxed));
    uart.write_str("[lcd] vcom ");
    uart.write_str(mode.name());
    uart.write_str(", light ");
    uart.write_str(if light_on() { "on" } else { "off" });
    let period = VCOM_PERIOD.load(Ordering::Relaxed);
    if period == 0 {
        uart.write_line(": no panel running");
        return;
    }
    let millihz = 32_768_000 / period;
    uart.write_str(": ");
    uart.write_u32(millihz / 1000);
    uart.write_str(".");
    uart.write_u32(millihz % 1000 / 100);
    uart.write_str(" Hz from LPTIM1 and LPTIM3, counters ");
    uart.write_u32(counter(LPTIM1_BASE));
    uart.write_str(" and ");
    uart.write_u32(counter(LPTIM3_BASE));
    if mode == Mode::Low && light_on() {
        uart.write_str(" (forced low with the light on: outside the transmissive row)");
    }
    uart.write_line("");
}

struct Cycles;

impl Clock for Cycles {
    #[inline(always)]
    fn cycles(&mut self) -> u32 {
        crate::busy::cycles()
    }

    #[inline(always)]
    fn masked<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        cortex_m::interrupt::free(|_| f(self))
    }
}

#[inline(always)]
fn pace() -> Pace<Cycles> {
    Pace::new(Cycles)
}

#[inline(always)]
fn set(pin: (u32, u32), high: bool) {
    let bit = if high { 1 << pin.1 } else { 1 << (pin.1 + 16) };
    unsafe { write32(pin.0 + GPIO_BSRR, bit) };
}

#[inline(always)]
fn put_data(word: u32) {
    unsafe { write32(GPIOE_BASE + GPIO_BSRR, word) };
}

/// Screen column `x`'s port E words, LSB plane then MSB: its pairs top first, then a black pair
/// for the last `BCK` labels, which latch too (`HARDWARE.md`).
fn line_words(fb: Option<&[u8]>, x: usize) -> [[u32; PAIRS + 1]; 2] {
    let mut words = [[wrist_panel::BLACK; PAIRS + 1]; 2];
    let w = WIDTH as usize;
    if let Some(fb) = fb.filter(|fb| x < w && fb.len() == w * HEIGHT as usize) {
        let [lsb, msb] = &mut words;
        for ((rows, lsb), msb) in fb.chunks_exact(2 * w).zip(lsb.iter_mut()).zip(msb.iter_mut()) {
            let (odd, even) = (rows[x], rows[w + x]);
            *lsb = wrist_panel::word(odd, even, 0);
            *msb = wrist_panel::word(odd, even, 1);
        }
    }
    words
}

/// One plane of a line, timed as Figure 6-3-3.
fn write_line(words: &[u32; PAIRS + 1]) {
    let (hold, setup) = (cycles(timing::DATA_HOLD), cycles(timing::DATA_SETUP));
    set(BSP, true);
    let mut pace = pace();
    pace.after(cycles(timing::BSP_SETUP), || set(BCK, true));
    let mut bck = 1 << BCK.1;
    for (i, &word) in words.iter().enumerate() {
        pace.after(hold, || {
            if i == 0 {
                set(BSP, false);
            }
            put_data(word);
        });
        bck ^= (1 << BCK.1) | (1 << (BCK.1 + 16));
        pace.after(setup, || unsafe { write32(BCK.0 + GPIO_BSRR, bck) });
    }
    bck ^= (1 << BCK.1) | (1 << (BCK.1 + 16));
    pace.after(hold + setup, || unsafe { write32(BCK.0 + GPIO_BSRR, bck) });
    // An odd edge count leaves `BCK` high; the figure parks it low before `GEN`.
    pace.after(hold + setup, || set(BCK, false));
    pace.after(cycles(timing::GEN_SETUP), || set(GEN, true));
    pace.after(cycles(timing::GEN_HIGH), || set(GEN, false));
    pace.wait(cycles(timing::GEN_HOLD));
}

/// The CPU's part of the last frame written, in cycles.
#[derive(Clone, Copy, Default)]
pub struct Cost {
    pub busy: u32,
    pub longest: u32,
}

impl Cost {
    pub(crate) fn end_step(&mut self, from: u32) {
        let took = crate::busy::cycles().wrapping_sub(from);
        self.busy = self.busy.wrapping_add(took);
        self.longest = self.longest.max(took);
    }
}

pub struct Lcd(());

impl Lcd {
    /// # Safety
    /// Writes RCC and GPIOA/B/D/E. Call once.
    pub unsafe fn init() -> Self {
        enable_clock(
            RCC_AHB2ENR1,
            RCC_AHB2ENR1_GPIOAEN
                | RCC_AHB2ENR1_GPIOBEN
                | RCC_AHB2ENR1_GPIODEN
                | RCC_AHB2ENR1_GPIOEEN,
        );
        for (base, pin) in [GSP, GCK, GEN, INTB, BSP, BCK, VCOM_VB, VA] {
            set((base, pin), false);
            configure_output(base, pin);
        }
        put_data(wrist_panel::BLACK);
        for pin in (0..16).filter(|pin| wrist_panel::DATA_MASK & (1 << pin) != 0) {
            configure_output(GPIOE_BASE, pin);
        }
        #[cfg(feature = "panel-dma")]
        crate::panel_dma::init();
        Self(())
    }

    /// The all-black frame Figure 6-2 asks for before `VCOM`, then `VCOM`.
    pub async fn start(&mut self) {
        let _ = self.scan(None, &[true; LINES]).await;
        unsafe { start_vcom() };
    }

    /// Writes the screen columns of `fb` marked in `columns`; how many, and what it cost.
    pub async fn write_frame(&mut self, fb: &[u8], columns: &[bool; LINES]) -> (u32, Cost) {
        let lines = core::array::from_fn(|line| columns[LINES - 1 - line]);
        self.scan(Some(fb), &lines).await
    }

    /// One frame (Figure 6-3-2, or 6-5-4 for a partial one); a line not in `dirty` is
    /// fast-forwarded without `GEN` and keeps its pixels.
    async fn scan(&mut self, fb: Option<&[u8]>, dirty: &[bool; LINES]) -> (u32, Cost) {
        let mut cost = Cost::default();
        if !dirty.contains(&true) {
            return (0, cost);
        }
        #[cfg(feature = "panel-dma")]
        if crate::panel_dma::usable() {
            if let Ok(done) = crate::panel_dma::scan(fb, dirty).await {
                return done;
            }
        }
        let mut step = crate::busy::cycles();
        let start = Instant::now();
        set(INTB, true);
        let mut pace = pace();
        pace.after(cycles(timing::GSP_SETUP), || set(GSP, true));
        pace.wait(cycles(timing::GSP_SETUP));
        let mut gck = false;
        let mut written = 0;
        let mut words = [[wrist_panel::BLACK; PAIRS + 1]; 2];
        let mut fast = 0;
        for edge in 1..=GCK_EDGES {
            gck = !gck;
            set(GCK, gck);
            if edge == 2 {
                pace = self::pace();
                pace.after(cycles(timing::GSP_SETUP), || set(GSP, false));
            }
            let line = (edge as usize).wrapping_sub(2) / 2;
            if (2..=481).contains(&edge) && dirty[line] {
                // The MSB plane, on the even edge, comes first.
                let plane = usize::from(edge % 2 == 0);
                if plane == 1 {
                    words = line_words(fb, LINES - 1 - line);
                    written += 1;
                }
                write_line(&words[plane]);
                cost.end_step(step);
                embassy_futures::yield_now().await;
                step = crate::busy::cycles();
                continue;
            }
            pace = self::pace();
            if matches!(edge, 1 | 2 | GCK_EDGES) {
                pace.wait(cycles(timing::GCK_HALF));
                continue;
            }
            pace.wait(cycles(timing::GCK_FAST));
            fast += 1;
            if fast % FAST_RUN == 0 {
                cost.end_step(step);
                embassy_futures::yield_now().await;
                step = crate::busy::cycles();
            }
        }
        // `GCK` 483's half-period and thhINTB, then thwINTB and tlwINTB (Table 6-3-2).
        pace.wait(2 * cycles(timing::GCK_HALF));
        cost.end_step(step);
        Timer::at(start + INTB_HIGH).await;
        step = crate::busy::cycles();
        set(INTB, false);
        self::pace().wait(cycles(timing::INTB_LOW));
        cost.end_step(step);
        (written, cost)
    }
}

unsafe fn configure_output(base: u32, pin: u32) {
    modify32(base + GPIO_OTYPER, 1 << pin, 0);
    modify32(base + GPIO_PUPDR, 0b11 << (pin * 2), 0);
    // Slowest slew, for the TXB0108 shifters' edge accelerators.
    modify32(base + GPIO_OSPEEDR, 0b11 << (pin * 2), 0);
    modify32(base + GPIO_MODER, 0b11 << (pin * 2), 0b01 << (pin * 2));
    let _ = read32(base + GPIO_MODER);
}
