//! Transmit-only USART3 on PC4 (`HARDWARE.md`, "Debug and boot"): queued and
//! drained by its interrupt, or written directly from fault handlers.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use crate::mmio::{
    enable_clock, modify32, read32, write32, GPIOC_BASE, GPIO_AFRL, GPIO_MODER, GPIO_OSPEEDR,
    GPIO_OTYPER, GPIO_PUPDR, NVIC_ICER0, NVIC_ICPR0, NVIC_ISER0, RCC_AHB2ENR1,
    RCC_AHB2ENR1_GPIOCEN, RCC_APB1ENR1, RCC_APB1ENR1_USART3EN, RCC_CCIPR1, RCC_CCIPR1_USART3SEL,
    USART3_BASE, USART3_IRQN, USART_BRR, USART_CR1, USART_CR1_TE, USART_CR1_TXFNFIE,
    USART_CR1_UE, USART_CR2, USART_CR3, USART_ISR, USART_ISR_TC, USART_ISR_TXFNF, USART_PRESC,
    USART_TDR,
};

pub const PCLK1_HZ: u32 = 4_000_000;

pub const BAUD: u32 = 9600;

const BRR_MIN: u32 = 16;
const BRR_MAX: u32 = 0xFFFF;

fn brr_for(clock_hz: u32, baud: u32) -> u32 {
    ((clock_hz + baud / 2) / baud).clamp(BRR_MIN, BRR_MAX)
}

static BRR_USED: AtomicU32 = AtomicU32::new(0);
static BAUD_USED: AtomicU32 = AtomicU32::new(0);

const PC4: u32 = 4;
const PC4_AF_USART3_TX: u32 = 7;

/// Bounded, so a dead or unconfigured USART3 cannot hang a fault handler.
const POLL_BUDGET: u32 = 20_000;

static DROPPED: AtomicU32 = AtomicU32::new(0);
/// Latched on the first synchronous timeout, so a dead USART3 costs one
/// timeout rather than one per byte.
static GAVE_UP: AtomicBool = AtomicBool::new(false);
static CONFIGURED: AtomicBool = AtomicBool::new(false);

const RING: usize = 16 * 1024;

struct Ring {
    buf: [u8; RING],
    head: usize,
    tail: usize,
}

impl Ring {
    const fn new() -> Self {
        Self {
            buf: [0; RING],
            head: 0,
            tail: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// `false` when full.
    fn push(&mut self, b: u8) -> bool {
        let next = (self.head + 1) % RING;
        if next == self.tail {
            return false;
        }
        self.buf[self.head] = b;
        self.head = next;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.is_empty() {
            return None;
        }
        let b = self.buf[self.tail];
        self.tail = (self.tail + 1) % RING;
        Some(b)
    }
}

static TRACK_SLEEP: AtomicBool = AtomicBool::new(false);
static TX_GUARD: Mutex<CriticalSectionRawMutex, RefCell<Option<embassy_stm32::rcc::WakeGuard>>> = Mutex::new(RefCell::new(None));
const TCIE: u32 = 1 << 6;
pub fn track_sleep() { TRACK_SLEEP.store(true, Ordering::Relaxed); Uart::arm(); }

static TX: Mutex<CriticalSectionRawMutex, RefCell<Ring>> = Mutex::new(RefCell::new(Ring::new()));

#[no_mangle]
pub unsafe extern "C" fn USART3() {
    let cr1 = read32(USART3_BASE + USART_CR1);
    if cr1 & TCIE != 0 && read32(USART3_BASE + USART_ISR) & USART_ISR_TC != 0 {
        modify32(USART3_BASE + USART_CR1, TCIE, 0);
        TX_GUARD.lock(|g| *g.borrow_mut() = None);
    }
    if cr1 & USART_CR1_TXFNFIE == 0 { return; }
    loop {
        if read32(USART3_BASE + USART_ISR) & USART_ISR_TXFNF == 0 {
            return;
        }
        let Some(b) = TX.lock(|ring| ring.borrow_mut().pop()) else {
            modify32(USART3_BASE + USART_CR1, USART_CR1_TXFNFIE, if TRACK_SLEEP.load(Ordering::Relaxed) { TCIE } else { 0 });
            return;
        };
        write32(USART3_BASE + USART_TDR, u32::from(b));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Queued,
    Direct,
}

/// Needs no initialised RAM, so a fault handler can make one.
#[derive(Clone, Copy)]
pub struct Uart(Mode);

#[allow(clippy::unused_self)]
impl Uart {
    /// # Safety
    /// Takes exclusive use of PC4 and USART3. Call once, early.
    pub unsafe fn init() -> Self {
        Self::configure(PCLK1_HZ, BAUD)
    }

    /// # Safety
    /// As [`Uart::init`].
    pub unsafe fn init_at(clock_hz: u32) -> Self {
        Self::configure(clock_hz, BAUD)
    }

    /// # Safety
    /// As [`Uart::init`]; flush first, or a byte in flight is lost.
    pub unsafe fn configure(clock_hz: u32, baud: u32) -> Self {
        write32(NVIC_ICER0 + 4 * (USART3_IRQN / 32), 1 << (USART3_IRQN % 32));

        enable_clock(RCC_AHB2ENR1, RCC_AHB2ENR1_GPIOCEN);
        enable_clock(RCC_APB1ENR1, RCC_APB1ENR1_USART3EN);

        // MODER last, so the pin leaves its reset state fully configured.
        modify32(
            GPIOC_BASE + GPIO_AFRL,
            0xF << (PC4 * 4),
            PC4_AF_USART3_TX << (PC4 * 4),
        );
        modify32(GPIOC_BASE + GPIO_OTYPER, 1 << PC4, 0);
        modify32(GPIOC_BASE + GPIO_PUPDR, 0b11 << (PC4 * 2), 0);
        modify32(
            GPIOC_BASE + GPIO_OSPEEDR,
            0b11 << (PC4 * 2),
            0b01 << (PC4 * 2),
        );
        modify32(
            GPIOC_BASE + GPIO_MODER,
            0b11 << (PC4 * 2),
            0b10 << (PC4 * 2),
        );

        modify32(RCC_CCIPR1, RCC_CCIPR1_USART3SEL, 0);

        write32(USART3_BASE + USART_CR1, 0);
        write32(USART3_BASE + USART_CR2, 0);
        write32(USART3_BASE + USART_CR3, 0);
        write32(USART3_BASE + USART_PRESC, 0);
        let brr = brr_for(clock_hz, baud);
        write32(USART3_BASE + USART_BRR, brr);
        write32(USART3_BASE + USART_CR1, USART_CR1_TE | USART_CR1_UE);

        BRR_USED.store(brr, Ordering::Relaxed);
        BAUD_USED.store(baud, Ordering::Relaxed);
        GAVE_UP.store(false, Ordering::Relaxed);
        CONFIGURED.store(true, Ordering::Relaxed);

        write32(NVIC_ICPR0 + 4 * (USART3_IRQN / 32), 1 << (USART3_IRQN % 32));
        write32(NVIC_ISER0 + 4 * (USART3_IRQN / 32), 1 << (USART3_IRQN % 32));
        Self::arm();
        Self(Mode::Queued)
    }

    fn arm() {
        if !CONFIGURED.load(Ordering::Relaxed) {
            return;
        }
        let pending = TX.lock(|ring| !ring.borrow().is_empty());
        if pending {
            if TRACK_SLEEP.load(Ordering::Relaxed) {
                TX_GUARD.lock(|g| {
                    let mut g = g.borrow_mut();
                    if g.is_none() { *g = Some(embassy_stm32::rcc::WakeGuard::new(embassy_stm32::rcc::StopMode::Stop1)); }
                });
            }
            unsafe { modify32(USART3_BASE + USART_CR1, TCIE, USART_CR1_TXFNFIE) };
        }
    }

    pub fn brr() -> u32 {
        BRR_USED.load(Ordering::Relaxed)
    }

    pub fn baud() -> u32 {
        BAUD_USED.load(Ordering::Relaxed)
    }

    /// Clear the give-up latch after a transient stall.
    pub fn retry() {
        GAVE_UP.store(false, Ordering::Relaxed);
    }

    pub fn best_effort() -> Self {
        Self(Mode::Queued)
    }

    /// A synchronous handle for fault handlers; it sends the queued backlog
    /// first.
    pub fn direct() -> Self {
        Self(Mode::Direct)
    }

    pub fn dropped() -> u32 {
        DROPPED.load(Ordering::Relaxed)
    }

    /// `false` on timeout.
    fn poll(flag: u32) -> bool {
        for _ in 0..POLL_BUDGET {
            if unsafe { read32(USART3_BASE + USART_ISR) } & flag != 0 {
                return true;
            }
        }
        false
    }

    fn put_sync(byte: u8) {
        if GAVE_UP.load(Ordering::Relaxed) {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if Self::poll(USART_ISR_TXFNF) {
            unsafe { write32(USART3_BASE + USART_TDR, u32::from(byte)) };
        } else {
            GAVE_UP.store(true, Ordering::Relaxed);
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn drain_sync(budget: usize) {
        unsafe { modify32(USART3_BASE + USART_CR1, USART_CR1_TXFNFIE, 0) };
        for _ in 0..budget {
            let Some(b) = TX.lock(|ring| ring.borrow_mut().pop()) else {
                return;
            };
            Self::put_sync(b);
        }
        let left = TX.lock(|ring| {
            let mut ring = ring.borrow_mut();
            let mut n = 0u32;
            while ring.pop().is_some() {
                n += 1;
            }
            n
        });
        DROPPED.fetch_add(left, Ordering::Relaxed);
    }

    fn write_raw(self, bytes: &[u8]) {
        // On battery nobody can hear it (`power.rs`), and draining it would
        // keep the core out of Stop2.
        if bytes.is_empty() || (self.0 == Mode::Queued && crate::power::on_battery()) {
            return;
        }
        crate::console::tee(bytes);
        match self.0 {
            Mode::Queued => {
                let lost = TX.lock(|ring| {
                    let mut ring = ring.borrow_mut();
                    let kept = bytes.iter().take_while(|&&b| ring.push(b)).count();
                    bytes.len() - kept
                });
                Self::arm();
                if lost > 0 {
                    DROPPED.fetch_add(u32::try_from(lost).unwrap_or(u32::MAX), Ordering::Relaxed);
                }
            }
            Mode::Direct => {
                Self::drain_sync(2048);
                for &b in bytes {
                    Self::put_sync(b);
                }
            }
        }
    }

    pub fn write_byte(self, byte: u8) {
        self.write_raw(&[byte]);
    }

    /// Each `\n` goes out as `\r\n`.
    pub fn write_bytes(self, bytes: &[u8]) {
        for (i, run) in bytes.split(|&b| b == b'\n').enumerate() {
            if i > 0 {
                self.write_raw(b"\r\n");
            }
            self.write_raw(run);
        }
    }

    pub fn write_str(self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    pub fn write_line(self, s: &str) {
        self.write_str(s);
        self.write_raw(b"\r\n");
    }

    pub fn write_hex32(self, value: u32) {
        const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
        let mut buf = *b"0x00000000";
        for (i, slot) in buf[2..].iter_mut().enumerate() {
            *slot = DIGITS[((value >> (28 - 4 * i)) & 0xF) as usize];
        }
        self.write_raw(&buf);
    }

    pub fn write_hex8(self, value: u8) {
        const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
        self.write_raw(&[DIGITS[usize::from(value >> 4)], DIGITS[usize::from(value & 0xF)]]);
    }

    pub fn write_u32(self, value: u32) {
        const DIGITS_MAX: usize = 10;
        let mut buf = [0u8; DIGITS_MAX];
        let mut len = 0usize;
        let mut rest = value;
        for slot in &mut buf {
            *slot = b'0' + u8::try_from(rest % 10).unwrap_or(0);
            len += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        buf[..len].reverse();
        self.write_raw(&buf[..len]);
    }

    pub fn write_reg(self, label: &str, value: u32) {
        self.write_str(label);
        self.write_hex32(value);
        self.write_str("\n");
    }

    /// Blocks until the backlog is on the wire: for rate changes, resets and
    /// fault handlers, never a task's steady state.
    pub fn flush(self) {
        Self::drain_sync(RING);
        if !GAVE_UP.load(Ordering::Relaxed) && !Self::poll(USART_ISR_TC) {
            GAVE_UP.store(true, Ordering::Relaxed);
        }
    }
}
