//! The Sharp LS012B7DD06A memory LCD, LCP-2619063C Rev C: pixels bit-banged from GPIO a
//! column at a time, `VCOM` from two LPTIMs on the LSE.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use embassy_time::{Duration, Instant, Timer};
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

// Port E: `0` is the odd pixel of a pair, `1` the even one.
const R0: u32 = 0;
const R1: u32 = 8;
const G0: u32 = 2;
const G1: u32 = 10;
const B0: u32 = 4;
const B1: u32 = 12;
const DATA_MASK: u32 = (1 << R0) | (1 << R1) | (1 << G0) | (1 << G1) | (1 << B0) | (1 << B1);
const BLACK_WORD: u32 = DATA_MASK << 16;

pub const WIDTH: u32 = 240;
pub const HEIGHT: u32 = 240;
const LINES: usize = WIDTH as usize;
const PAIRS: usize = (HEIGHT / 2) as usize;
const BCK_EDGES: u32 = 123;
const GCK_EDGES: u32 = 483;

/// At `rcc_config`'s 48 MHz system clock.
const CYCLES_PER_US: u32 = 48;

/// Table 6-3-3's minimum is 0.4 us.
const BCK_HALF_US: u32 = 1;
/// A `GCK` half-period without a line written: Table 6-3-2's minimum is 62.67 us, or 1.0 us
/// for the lines a partial update fast-forwards (Note 6-3-1, Figure 6-5-4: a reference, not
/// guaranteed).
const GCK_US: u32 = 70;
const GCK_FAST_US: u32 = 2;
/// Table 6-3-2's `thwINTB`, 30.3 ms, which a partial frame is shorter than.
const INTB_HIGH: Duration = Duration::from_millis(31);

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

unsafe fn set_alternate((base, pin): (u32, u32), af: u32) {
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

fn delay_us(us: u32) {
    cortex_m::asm::delay(us * CYCLES_PER_US);
}

fn set(pin: (u32, u32), high: bool) {
    let bit = if high { 1 << pin.1 } else { 1 << (pin.1 + 16) };
    unsafe { write32(pin.0 + GPIO_BSRR, bit) };
}

/// Two bits per channel, packed `0b00RRGGBB`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb2(pub u8);

impl Rgb2 {
    pub const BLACK: Self = Self(0b00_00_00);

    pub const fn from_abgr2222(byte: u8) -> Self {
        let r = byte & 0b11;
        let g = (byte >> 2) & 0b11;
        let b = (byte >> 4) & 0b11;
        Self((r << 4) | (g << 2) | b)
    }

    fn bit(self, shift: u32, plane: u32) -> u32 {
        (u32::from(self.0) >> shift >> plane) & 1
    }
}

/// Port E's `BSRR` word for one pixel pair; `plane` 0 is the LSB.
fn data_word(odd: Rgb2, even: Rgb2, plane: u32) -> u32 {
    let set = (odd.bit(4, plane) << R0)
        | (even.bit(4, plane) << R1)
        | (odd.bit(2, plane) << G0)
        | (even.bit(2, plane) << G1)
        | (odd.bit(0, plane) << B0)
        | (even.bit(0, plane) << B1);
    set | ((!set & DATA_MASK) << 16)
}

/// Both planes' words for screen column `x`, top pair first; `None` is black.
fn column_words(fb: Option<&[u8]>, x: usize, words: &mut [[u32; PAIRS]; 2]) {
    let [lsb, msb] = words;
    for (pair, (lsb, msb)) in lsb.iter_mut().zip(msb).enumerate() {
        let (odd, even) = fb.map_or((Rgb2::BLACK, Rgb2::BLACK), |fb| {
            let at = |y: usize| Rgb2::from_abgr2222(fb[y * WIDTH as usize + x]);
            (at(2 * pair), at(2 * pair + 1))
        });
        *lsb = data_word(odd, even, 0);
        *msb = data_word(odd, even, 1);
    }
}

/// Figure 6-3-3, whose labels count `BCK` half-periods (`HARDWARE.md`).
fn write_line(words: &[u32; PAIRS]) {
    set(BSP, true);
    delay_us(BCK_HALF_US);
    let mut bck = false;
    for edge in 1..=BCK_EDGES {
        let word = match edge {
            2..=121 => Some(words[(edge - 2) as usize]),
            // Labels 122 and 123 latch too, into pixels the glass shows (`HARDWARE.md`).
            122 => Some(BLACK_WORD),
            _ => None,
        };
        if let Some(word) = word {
            unsafe { write32(GPIOE_BASE + GPIO_BSRR, word) };
        }
        delay_us(BCK_HALF_US);
        bck = !bck;
        set(BCK, bck);
        if edge == 1 {
            // tlsBSP.
            delay_us(1);
            set(BSP, false);
        }
    }
    // An odd edge count leaves `BCK` high; the figure parks it low before `GEN`.
    delay_us(BCK_HALF_US);
    set(BCK, false);
    delay_us(1);
    set(GEN, true);
    delay_us(11);
    set(GEN, false);
    delay_us(5);
}

/// The CPU's part of the last frame written, in cycles.
#[derive(Clone, Copy, Default)]
pub struct Cost {
    pub busy: u32,
    pub longest: u32,
}

impl Cost {
    fn end_step(&mut self, from: u32) {
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
        write32(GPIOE_BASE + GPIO_BSRR, DATA_MASK << 16);
        for pin in [R0, R1, G0, G1, B0, B1] {
            configure_output(GPIOE_BASE, pin);
        }
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

    /// One frame (Figure 6-3-2, or 6-5-4 for a partial one). Gate line `n` is screen column
    /// `WIDTH - 1 - n` (`HARDWARE.md`); a line not in `dirty` is fast-forwarded without `GEN`
    /// and keeps its pixels.
    async fn scan(&mut self, fb: Option<&[u8]>, dirty: &[bool; LINES]) -> (u32, Cost) {
        let mut cost = Cost::default();
        if !dirty.contains(&true) {
            return (0, cost);
        }
        let mut step = crate::busy::cycles();
        let start = Instant::now();
        set(INTB, true);
        delay_us(12);
        set(GSP, true);
        delay_us(12);
        let mut gck = false;
        let mut words = [[0u32; PAIRS]; 2];
        let mut written = 0;
        for edge in 1..=GCK_EDGES {
            gck = !gck;
            set(GCK, gck);
            if edge == 2 {
                delay_us(12);
                set(GSP, false);
            }
            let line = (edge as usize).wrapping_sub(2) / 2;
            if (2..=481).contains(&edge) && dirty[line] {
                // The MSB plane, on the even edge, comes first.
                let plane = usize::from(edge % 2 == 0);
                if plane == 1 {
                    column_words(fb, LINES - 1 - line, &mut words);
                    written += 1;
                }
                write_line(&words[plane]);
                cost.end_step(step);
                embassy_futures::yield_now().await;
                step = crate::busy::cycles();
            } else if matches!(edge, 1 | 2 | GCK_EDGES) {
                delay_us(GCK_US);
            } else {
                delay_us(GCK_FAST_US);
            }
        }
        // thhINTB and thwINTB, then tlwINTB (Table 6-3-2).
        delay_us(GCK_US);
        cost.end_step(step);
        Timer::at(start + INTB_HIGH).await;
        step = crate::busy::cycles();
        set(INTB, false);
        delay_us(12);
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
