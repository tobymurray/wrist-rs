#![no_std]
#![no_main]

//! embassy-boot's loader, plus what the vendor bootloader's handoff needs
//! (`HARDWARE.md`, "Boot handoff").

use core::cell::RefCell;
use core::ptr::write_volatile;

use cortex_m::peripheral::{NVIC, SCB, SYST};
use cortex_m_rt::{entry, exception};
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embassy_stm32::flash::{Flash, BANK1_REGION};
use embassy_stm32::pac::gpio::vals::{Moder, Ot};
use embassy_stm32::pac::rcc::vals::Rtcsel;
use embassy_stm32::pac::{GPIOF, ICACHE, RCC};
use embassy_stm32::rcc::{LsConfig, LseConfig, LseMode, RtcClockSource};
use embassy_stm32::time::Hertz;
use embassy_sync::blocking_mutex::Mutex;
use wrist_board::{light, Port};
use wrist_flash::vectors::{MAILBOX_BDCR, MAILBOX_MAGIC, MAILBOX_TAG};

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

// One store for both, which cortex-m's `clear_pendst` and `clear_pendsv` would split.
const ICSR_PENDSTCLR: u32 = 1 << 25;
const ICSR_PENDSVCLR: u32 = 1 << 27;

extern "C" fn pre_init_rust() {
    // Only MMIO: `.data`/`.bss` are not initialised yet.
    unsafe {
        let (syst, scb, nvic) = (&*SYST::PTR, &*SCB::PTR, &*NVIC::PTR);
        syst.csr.write(0);
        scb.icsr.write(ICSR_PENDSTCLR | ICSR_PENDSVCLR);
        for (icer, icpr) in nvic.icer.iter().zip(&nvic.icpr) {
            icer.write(!0);
            icpr.write(!0);
        }
        scb.vtor.write(core::ptr::addr_of!(__vector_table) as u32);
    }
    // Off, so the swap reads flash rather than stale lines (RM0456 8.7.1).
    ICACHE.cr().write(|w| w.set_en(false));
}

#[entry]
fn main() -> ! {
    // For the application's report: the backup domain as the vendor bootloader left it.
    unsafe {
        write_volatile(MAILBOX_MAGIC as *mut u32, MAILBOX_TAG);
        write_volatile(MAILBOX_BDCR as *mut u32, RCC.bdcr().read().0);
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
    let bdcr = RCC.bdcr().read();
    if bdcr.lseon()
        && bdcr.lserdy()
        && bdcr.lsebyp()
        && bdcr.rtcsel() == Rtcsel::LSE
        && bdcr.rtcen()
    {
        LsConfig {
            rtc: RtcClockSource::LSE,
            lsi: false,
            lse: Some(LseConfig {
                frequency: Hertz(32_768),
                mode: LseMode::Bypass,
                // Mirrored, so embassy's comparison cannot fail; its wait for `LSESYSRDY` ends
                // because `LSERDY` is set.
                peripherals_clocked: bdcr.lsesysen(),
            }),
        }
    } else {
        LsConfig::off()
    }
}

const _: () = assert!(matches!(light::PIN.port, Port::F));

/// A visible fault: ten backlight flashes, then a reset.
fn signal_and_reset() -> ! {
    let n = usize::from(light::PIN.number);
    RCC.ahb2enr1().modify(|w| w.set_gpiofen(true));
    GPIOF.otyper().modify(|w| w.set_ot(n, if light::OPEN_DRAIN { Ot::OPEN_DRAIN } else { Ot::PUSH_PULL }));
    GPIOF.moder().modify(|w| w.set_moder(n, Moder::OUTPUT));
    for _ in 0..10 {
        GPIOF.bsrr().write(|w| if light::ACTIVE_LOW { w.set_br(n, true) } else { w.set_bs(n, true) });
        cortex_m::asm::delay(200_000);
        GPIOF.bsrr().write(|w| if light::ACTIVE_LOW { w.set_bs(n, true) } else { w.set_br(n, true) });
        cortex_m::asm::delay(200_000);
    }
    cortex_m::asm::delay(4_000_000);
    SCB::sys_reset()
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
