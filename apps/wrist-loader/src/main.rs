#![no_std]
#![no_main]

//! embassy-boot's loader, plus what the vendor bootloader's handoff needs
//! (`HARDWARE.md`, "Boot handoff").

use core::cell::RefCell;
use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::{entry, exception};
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embassy_stm32::flash::{Flash, BANK1_REGION};
use embassy_stm32::rcc::{LsConfig, LseConfig, LseMode, RtcClockSource};
use embassy_stm32::time::Hertz;
use embassy_sync::blocking_mutex::Mutex;

extern "C" {
    static __vector_table: u32;
}

// Runs before the stack is touched, the only point the inherited stack pointer can be replaced.
core::arch::global_asm!(
    ".section .text.__pre_init, \"ax\"",
    ".global __pre_init",
    ".thumb_func",
    "__pre_init:",
    "    cpsid i",
    "    movw r0, #:lower16:{vector_table}",
    "    movt r0, #:upper16:{vector_table}",
    "    ldr  r0, [r0]",
    "    msr  msp, r0",
    "    movs r1, #0",
    "    msr  control, r1",
    "    isb",
    "    b    {pre_init_rust}",
    vector_table = sym __vector_table,
    pre_init_rust = sym pre_init_rust,
);

const SYST_CSR: *mut u32 = 0xE000_E010 as *mut u32;
const SCB_ICSR: *mut u32 = 0xE000_ED04 as *mut u32;
const SCB_VTOR: *mut u32 = 0xE000_ED08 as *mut u32;
const ICSR_PENDSTCLR: u32 = 1 << 25;
const ICSR_PENDSVCLR: u32 = 1 << 27;
const NVIC_ICER: *mut u32 = 0xE000_E180 as *mut u32;
const NVIC_ICPR: *mut u32 = 0xE000_E280 as *mut u32;
/// Off, so the swap reads flash rather than stale lines (RM0456 8.7.1).
const ICACHE_CR: *mut u32 = 0x4003_0400 as *mut u32;

extern "C" fn pre_init_rust() {
    // Only MMIO: `.data`/`.bss` are not initialised yet.
    unsafe {
        write_volatile(SYST_CSR, 0);
        write_volatile(SCB_ICSR, ICSR_PENDSTCLR | ICSR_PENDSVCLR);
        for i in 0..16 {
            write_volatile(NVIC_ICER.add(i), !0);
            write_volatile(NVIC_ICPR.add(i), !0);
        }
        write_volatile(SCB_VTOR, core::ptr::addr_of!(__vector_table) as u32);
        write_volatile(ICACHE_CR, 0);
    }
}

#[entry]
fn main() -> ! {
    // For the application's report: the backup domain as the vendor bootloader left it.
    unsafe {
        write_volatile(MAILBOX_MAGIC, MAILBOX_TAG);
        write_volatile(MAILBOX_BDCR, read_volatile(RCC_BDCR));
    }
    let mut config = embassy_stm32::Config::default();
    config.rcc.ls = ls_config_as_found();
    let p = embassy_stm32::init(config);
    let layout = Flash::new_blocking(p.FLASH).into_blocking_regions();
    let flash = Mutex::new(RefCell::new(layout.bank1_region));
    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    // 2048: a divisor of the 8 KiB page and a multiple of the 16-byte write.
    let loader = BootLoader::prepare::<_, _, _, 2048>(config);
    unsafe { loader.load(BANK1_REGION.base() + active_offset) }
}

/// What the backup domain already holds, so embassy neither resets it, RTC calendar and all,
/// nor waits on an LSE start.
fn ls_config_as_found() -> LsConfig {
    const LSEON: u32 = 1 << 0;
    const LSERDY: u32 = 1 << 1;
    const LSEBYP: u32 = 1 << 2;
    const LSESYSEN: u32 = 1 << 7;
    const RTCSEL_MASK: u32 = 0b11 << 8;
    const RTCSEL_LSE: u32 = 0b01 << 8;
    const RTCEN: u32 = 1 << 15;
    let bdcr = unsafe { read_volatile(RCC_BDCR) };
    let running = LSEON | LSERDY | LSEBYP | RTCSEL_LSE | RTCEN;
    if bdcr & (LSEON | LSERDY | LSEBYP | RTCSEL_MASK | RTCEN) == running {
        LsConfig {
            rtc: RtcClockSource::LSE,
            lsi: false,
            lse: Some(LseConfig {
                frequency: Hertz(32_768),
                mode: LseMode::Bypass,
                // Mirrored, so embassy's comparison cannot fail; its wait for `LSESYSRDY` ends
                // because `LSERDY` is set.
                peripherals_clocked: bdcr & LSESYSEN != 0,
            }),
        }
    } else {
        LsConfig::off()
    }
}

const RCC_BDCR: *const u32 = 0x4602_0CF0 as *const u32;
const MAILBOX_MAGIC: *mut u32 = 0x2026_FFF0 as *mut u32;
const MAILBOX_BDCR: *mut u32 = 0x2026_FFF4 as *mut u32;
const MAILBOX_TAG: u32 = 0x4C44_4352;

/// A visible fault: ten backlight flashes, then a reset.
fn signal_and_reset() -> ! {
    const RCC_AHB2ENR1: *mut u32 = 0x4602_0C8C as *mut u32;
    const GPIOF_MODER: *mut u32 = 0x4202_1400 as *mut u32;
    const GPIOF_OTYPER: *mut u32 = 0x4202_1404 as *mut u32;
    const GPIOF_BSRR: *mut u32 = 0x4202_1418 as *mut u32;
    const PF3: u32 = 3;
    const LIGHT_ON: u32 = 1 << (PF3 + 16);
    const LIGHT_OFF: u32 = 1 << PF3;
    unsafe {
        write_volatile(RCC_AHB2ENR1, read_volatile(RCC_AHB2ENR1) | (1 << 5));
        write_volatile(GPIOF_OTYPER, read_volatile(GPIOF_OTYPER) | (1 << PF3));
        let moder = read_volatile(GPIOF_MODER) & !(0b11 << (PF3 * 2)) | (0b01 << (PF3 * 2));
        write_volatile(GPIOF_MODER, moder);
        for _ in 0..10 {
            write_volatile(GPIOF_BSRR, LIGHT_ON);
            cortex_m::asm::delay(200_000);
            write_volatile(GPIOF_BSRR, LIGHT_OFF);
            cortex_m::asm::delay(200_000);
        }
        cortex_m::asm::delay(4_000_000);
    }
    cortex_m::peripheral::SCB::sys_reset()
}

#[exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    signal_and_reset()
}

#[exception]
unsafe fn DefaultHandler(_irqn: i16) -> ! {
    signal_and_reset()
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    signal_and_reset()
}
