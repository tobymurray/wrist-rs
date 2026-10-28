//! The firmware the vendor bootloader starts in place of its kernel.

#![no_std]
#![no_main]

#[cfg(feature = "ble")]
mod ble;
mod busy;
mod console;
mod crash;
mod flash;
mod glass;
mod lcd;
mod mmio;
mod modules;
mod panel_dma;
mod park;
mod pmic;
mod rtc;
mod power;
mod sleep;
mod uart;
mod update;
mod usblog;
mod version;
mod watchdog;

use core::cell::Cell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m_rt::{entry, exception, ExceptionFrame};
use embassy_stm32::executor::Executor;
use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::Pull;
use embassy_stm32::i2c::{self, I2c};
use embassy_stm32::mode::Async;
use embassy_stm32::{bind_interrupts, dma};
use embassy_stm32::rcc;
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};
use embassy_time::{Duration, Instant, Timer};
use static_cell::{ConstStaticCell, StaticCell};
use embassy_futures::select::{select3, select4, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use wrist_glass::pages;
use wrist_glass::scene::Scene;
use wrist_glass::text::Glass;
use wrist_pages::{Button, Nav, Page, Press};

use crate::mmio::{
    enable_clock, modify32, read32, write32, DBGMCU_IDCODE, FLASH_ACR, FLASH_OPTR, GPIOA_BASE,
    GPIOB_BASE, GPIOC_BASE, GPIOD_BASE, GPIOF_BASE, GPIO_BSRR, GPIO_IDR, GPIO_MODER, GPIO_OSPEEDR,
    GPIO_OTYPER, GPIO_PUPDR, ICACHE_CR, ICACHE_CR_EN, ICSR_PENDSTCLR, PWR_SVMCR, PWR_SVMCR_USV, PWR_SVMCR_UVMEN, PWR_SVMSR,
    PWR_SVMSR_VDDUSBRDY, PWR_VOSR, PWR_VOSR_USBBOOSTEN, PWR_VOSR_USBBOOSTRDY, PWR_VOSR_USBPWREN, ICSR_PENDSVCLR, NVIC_ICER0, NVIC_ICPR0, NVIC_REGS,
    RCC_AHB1ENR, RCC_AHB2ENR1, RCC_AHB2ENR2, RCC_AHB3ENR, RCC_APB1ENR1, RCC_APB1ENR2, RCC_APB2ENR,
    RCC_APB3ENR, RCC_AHB2ENR1_GPIOAEN, RCC_AHB2ENR1_GPIOBEN, RCC_AHB2ENR1_GPIOCEN,
    RCC_AHB2ENR1_GPIODEN, RCC_AHB2ENR1_GPIOFEN, RCC_BDCR, RCC_CCIPR1, RCC_CFGR1,
    RCC_CFGR1_SWS_MASK, RCC_CFGR1_SW_MASK, RCC_CFGR2, RCC_CR, RCC_CR_MSISON, RCC_CR_MSISRDY,
    RCC_CSR, RCC_CSR_RMVF, RCC_ICSCR1, RCC_ICSCR1_MSIRGSEL, RCC_ICSCR1_MSISRANGE_4, RCC_ICSCR1_MSISRANGE_MASK,
    RCC_ICSCR2, RCC_ICSCR3, RCC_PLL1CFGR, SCB_BFAR, SCB_CFSR, SCB_HFSR, SCB_ICSR, SCB_MMFAR,
    SCB_SHCSR, SCB_VTOR, SHCSR_BUSFAULTENA, SHCSR_MEMFAULTENA, SHCSR_USGFAULTENA, SYST_CSR,
};
use crate::uart::{Uart, BAUD, PCLK1_HZ};

extern "C" {
    static __vector_table: [u32; 2];
}

/// The initial SP words the bootloader accepts (`HARDWARE.md`, "Boot handoff").
const SRAM_WINDOW: core::ops::RangeInclusive<u32> = 0x2000_0000..=0x2027_0000;

/// RM0456 11.8.7.
const RCC_CFGR2_RESET: u32 = 0x0000_6000;

/// The rates 4 MHz reaches cleanly (`HARDWARE.md`, "Debug and boot").
const SWEEP_BAUDS: [u32; 5] = [115_200, 57_600, 38_400, 19_200, 9600];

#[derive(Clone, Copy)]
pub enum Report {
    Full,
    UsbLog,
    Staging(usize),
    FlashSurvey,
}

pub static REPORT: Signal<CriticalSectionRawMutex, Report> = Signal::new();

fn image_origin() -> u32 {
    core::ptr::addr_of!(__vector_table) as u32
}

/// PB15 high lets the debug UART out (`HARDWARE.md`, "Debug and boot"); PC5 and
/// PB3 go high as probe points.
///
/// # Safety
/// Runs from `__pre_init`, before `.data` and `.bss` exist: registers only.
unsafe fn assert_alive_pins() {
    enable_clock(
        RCC_AHB2ENR1,
        RCC_AHB2ENR1_GPIOBEN | RCC_AHB2ENR1_GPIOCEN | RCC_AHB2ENR1_GPIOFEN,
    );

    // PF3, `BACKLIGHT_ON`: open-drain, released (`HARDWARE.md`, "Power and reset domain").
    write32(GPIOF_BASE + GPIO_BSRR, 1 << 3);
    modify32(GPIOF_BASE + GPIO_OTYPER, 0, 1 << 3);
    modify32(GPIOF_BASE + GPIO_PUPDR, 0b11 << 6, 0);
    modify32(GPIOF_BASE + GPIO_MODER, 0b11 << 6, 0b01 << 6);

    for (base, pin) in [(GPIOB_BASE, 15u32), (GPIOC_BASE, 5u32), (GPIOB_BASE, 3u32)] {
        modify32(base + GPIO_OTYPER, 1 << pin, 0);
        modify32(base + GPIO_PUPDR, 0b11 << (pin * 2), 0);
        modify32(base + GPIO_OSPEEDR, 0b11 << (pin * 2), 0);
        modify32(base + GPIO_MODER, 0b11 << (pin * 2), 0b01 << (pin * 2));
        write32(base + GPIO_BSRR, 1 << pin);
    }
}

// MSP from the vector table and SPSEL = 0 before anything is pushed
// (`HARDWARE.md`, "Boot handoff").
core::arch::global_asm!(
    ".section .text.__pre_init, \"ax\"",
    ".global __pre_init",
    ".thumb_func",
    "__pre_init:",
    "    movw r0, #:lower16:{vector_table}",
    "    movt r0, #:upper16:{vector_table}",
    "    ldr  r0, [r0]",
    "    msr  msp, r0",
    "    movs r1, #0",
    "    msr  control, r1",
    "    isb",
    "    b    {pre_init_rust}", // LR still holds the reset handler's return.
    vector_table = sym __vector_table,
    pre_init_rust = sym pre_init_rust,
);

extern "C" fn pre_init_rust() {
    unsafe { assert_alive_pins() };
}

/// Quiets what the bootloader left in the NVIC and `SysTick` before unmasking
/// interrupts (`HARDWARE.md`, "Boot handoff").
///
/// # Safety
/// Call once, before anything enables an interrupt.
unsafe fn take_over_core() {
    write32(SCB_VTOR, image_origin());

    write32(SYST_CSR, 0);
    write32(SCB_ICSR, ICSR_PENDSTCLR | ICSR_PENDSVCLR);
    for i in 0..NVIC_REGS {
        write32(NVIC_ICER0 + i * 4, 0xFFFF_FFFF);
        write32(NVIC_ICPR0 + i * 4, 0xFFFF_FFFF);
    }
    core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));

    modify32(
        SCB_SHCSR,
        0,
        SHCSR_MEMFAULTENA | SHCSR_BUSFAULTENA | SHCSR_USGFAULTENA,
    );
    core::arch::asm!("cpsie i", options(nomem, nostack));
}

/// False if the invalidation never finished, leaving the cache off.
unsafe fn icache_on() -> bool {
    if !update::invalidate_icache() {
        return false;
    }
    modify32(ICACHE_CR, 0, ICACHE_CR_EN);
    true
}

#[derive(Clone, Copy)]
struct RccSnapshot {
    cr: u32,
    icscr1: u32,
    cfgr1: u32,
    cfgr2: u32,
    csr: u32,
    ccipr1: u32,
    pll1cfgr: u32,
    flash_acr: u32,
}

impl RccSnapshot {
    fn take() -> Self {
        unsafe {
            Self {
                cr: read32(RCC_CR),
                icscr1: read32(RCC_ICSCR1),
                cfgr1: read32(RCC_CFGR1),
                cfgr2: read32(RCC_CFGR2),
                csr: read32(RCC_CSR),
                ccipr1: read32(RCC_CCIPR1),
                pll1cfgr: read32(RCC_PLL1CFGR),
                flash_acr: read32(FLASH_ACR),
            }
        }
    }
}

const CLOCK_POLL_BUDGET: u32 = 200_000;

/// Returns a bitfield: 1 MSIS ready, 2 range written, 4 SYSCLK switched.
///
/// # Safety
/// Call before anything depends on the system clock.
unsafe fn force_msis_4mhz() -> u32 {
    let mut status = 0u32;

    modify32(RCC_CR, 0, RCC_CR_MSISON);
    let mut ready = false;
    for _ in 0..CLOCK_POLL_BUDGET {
        if read32(RCC_CR) & RCC_CR_MSISRDY != 0 {
            ready = true;
            break;
        }
    }
    if !ready {
        return status;
    }
    status |= 1;

    // RM0456 11.8.2.
    modify32(
        RCC_ICSCR1,
        RCC_ICSCR1_MSISRANGE_MASK,
        RCC_ICSCR1_MSISRANGE_4 | RCC_ICSCR1_MSIRGSEL,
    );
    status |= 2;

    write32(RCC_CFGR2, RCC_CFGR2_RESET);
    modify32(RCC_CFGR1, RCC_CFGR1_SW_MASK, 0);
    for _ in 0..CLOCK_POLL_BUDGET {
        if read32(RCC_CFGR1) & RCC_CFGR1_SWS_MASK == 0 {
            status |= 4;
            break;
        }
    }
    status
}

#[allow(clippy::too_many_lines)]
#[entry]
fn main() -> ! {
    // The UART first, and every later step announces itself, so a capture
    // shows how far boot got.
    let handoff = RccSnapshot::take();
    let forced = unsafe { force_msis_4mhz() };
    beacon(2);

    let mut uart = unsafe { Uart::init() };
    uart.write_line("");
    uart.write_str("[1] uart up (MSIS forced to range 4, 9600 baud; force status ");
    uart.write_u32(forced);
    uart.write_line(" of 7)");
    report_handoff(uart, &handoff);
    report_reset_cause(uart, handoff.csr);
    // So the next boot's flags name only its own reset.
    unsafe { modify32(RCC_CSR, 0, RCC_CSR_RMVF) };
    uart.flush();
    beacon(if Uart::dropped() == 0 { 3 } else { 8 });

    let (measured_hz, trustworthy) = measure_pclk1(uart);
    if trustworthy && measured_hz != PCLK1_HZ {
        uart = unsafe { Uart::init_at(measured_hz) };
        uart.write_str("[2] re-inited at measured ");
        uart.write_u32(measured_hz);
        uart.write_line(" Hz; anything above was garbled");
    } else {
        uart.write_str("[2] clock ");
        uart.write_u32(measured_hz);
        uart.write_str(if trustworthy {
            " Hz (trusted)"
        } else {
            " Hz (NOT trusted)"
        });
        uart.write_line("");
    }
    uart.flush();

    uart.write_line("[3] sweeping banner across baud rates");
    uart.flush();
    uart = sweep_banner(measured_hz);

    uart.write_line("[4] taking over core (VTOR, SysTick, NVIC, faults)");
    uart.flush();
    unsafe { take_over_core() };

    uart.write_str("[5] core taken over; instruction cache ");
    uart.write_line(if unsafe { icache_on() } { "on; arming watchdog" } else { "NOT on (invalidation never finished); arming watchdog" });
    uart.flush();
    let watchdog = unsafe { watchdog::start() };

    uart.write_str("[6] watchdog: ");
    uart.write_line(watchdog.as_str());
    uart.flush();

    beacon(4);
    unsafe { init_buttons() };
    uart.write_line("[7] buttons armed");
    report_buttons(uart);
    uart.write_line("    hold any button ~3 s to reset into the bootloader");
    uart.flush();

    let hse_ok = unsafe { start_hse(HSE_POLLS_4MHZ) };
    uart.write_str("[8] HSE 16 MHz crystal ");
    uart.write_line(if hse_ok {
        "ready"
    } else {
        "DID NOT START; no USB this boot"
    });
    uart.flush();
    if !backup_domain_as_measured(unsafe { read32(RCC_BDCR) }) {
        uart.write_str("[8b] LSE not running as measured; LSE in bypass and the RTC started: ");
        uart.write_line(rtc::prepare(measured_hz / 1000).unwrap_or_else(|why| why));
        uart.flush();
    }
    let mut config = embassy_stm32::Config::default();
    config.rcc = rcc_config(hse_ok);
    config.enable_debug_during_sleep = false;
    config.min_stop_pause = Duration::from_micros(200);
    let p = embassy_stm32::init(config);
    // `panel_dma.rs` drives these through registers.
    let _ = (p.TIM1, p.TIM3, p.TIM4, p.GPDMA1_CH12, p.GPDMA1_CH13, p.GPDMA1_CH14, p.GPDMA1_CH15, p.PA6, p.PA7, p.PB0);
    uart = unsafe { Uart::init_at(48_000_000) };
    uart.write_line(if unsafe { read32(RCC_BDCR) } & BDCR_LSERDY != 0 {
        "[9] embassy-stm32 initialised: MSIS 48 MHz, time driver on LPTIM4/LSE, UART re-divided"
    } else {
        "[9] embassy-stm32 initialised: MSIS 48 MHz, time driver on LPTIM4/LSI (NO LSE: timers 2.3 % slow), UART re-divided"
    });
    uart.flush();

    // Blocking transfers only before the first awaited one (`HARDWARE.md`, "Debug and boot").
    let mut i2c_config = embassy_stm32::i2c::Config::default();
    i2c_config.frequency = Hertz::khz(100);
    i2c_config.timeout = embassy_time::Duration::from_millis(20);
    let mut bus = I2c::new(p.I2C1, p.PB6, p.PB7, p.GPDMA1_CH0, p.GPDMA1_CH1, I2cIrqs, i2c_config);
    let power = pmic::snapshot_blocking(&mut bus);
    uart.write_line("[10] I2C1 up; power chips read:");
    pmic::write_report(&mut uart.lines(), &power);
    pmic::read_modes_blocking(&mut bus);
    pmic::read_gauge_config_blocking(&mut bus);
    uart.write_str("[10] gauge reset: ");
    uart.write_line(match pmic::restore_gauge_blocking(&mut bus) {
        Ok(false) => "none",
        Ok(true) => pmic::RESTORED,
        Err(why) => why,
    });
    pmic::write_rails(&mut uart.lines(), power.top_cntl3);
    pmic::write_summary(&mut uart.lines(), &power);
    match pmic::slow_gauge_hibernate(&mut bus) {
        Some(cfg) => {
            let [hi, lo] = cfg.to_be_bytes();
            uart.write_str("[10] gauge HibCfg ");
            uart.write_hex8(hi);
            uart.write_hex8(lo);
            uart.write_line("");
        }
        None => uart.write_line("[10] gauge HibCfg: the gauge did not answer"),
    }
    uart.write_str("[10b] input-power interrupt on PWR_INT_L (PG7): ");
    match crate::power::arm_input_interrupt(&mut bus) {
        Some(mask) => {
            uart.write_str("SUB_INT0_MASK ");
            uart.write_hex8(mask);
            uart.write_line("");
        }
        None => uart.write_line("the PMIC did not answer; the input is sampled each minute"),
    }
    uart.flush();

    uart.write_line("[10c] module EEPROMs (only another module's name refuses a slot):");
    modules::read_at_boot(modules::Slot::View, &mut bus, uart);
    {
        let mut link1_config = embassy_stm32::i2c::Config::default();
        link1_config.frequency = Hertz::khz(100);
        link1_config.timeout = embassy_time::Duration::from_millis(20);
        let mut link1 = I2c::new_blocking(p.I2C3, p.PC0, p.PC1, link1_config);
        modules::read_at_boot(modules::Slot::Link1, &mut link1, uart);
    }
    let mut sensor_config = embassy_stm32::i2c::Config::default();
    sensor_config.frequency = Hertz::khz(400);
    sensor_config.timeout = embassy_time::Duration::from_millis(20);
    let mut sensor_bus = I2c::new_blocking(p.I2C4, p.PD12, p.PD13, sensor_config);
    modules::read_at_boot(modules::Slot::Link2, &mut sensor_bus, uart);
    uart.flush();

    let usb = usb_console(uart, hse_ok, p.USB_OTG_HS, p.PA12, p.PA11, p.RCC);
    let flash = update::init(p.FLASH);
    uart.write_str("[12] flash shared; loader state ");
    uart.write_str(update::state_name(flash));
    uart.write_str("; build ");
    uart.write_line(version::BUILD_ID);
    uart.flush();

    let parked = park::Pins {
        emmc_en: p.PD10,
        gnss_en: p.PG5,
        gnss_wake: p.PD1,
        #[cfg(not(feature = "ble"))]
        ble_rst: p.PA8,
        body_rst: p.PF13,
        body_cs: p.PG12,
        altimeter_en: p.PG0,
        haptic_rst: p.PF2,
        buzzer_en1: p.PB9,
        buzzer_en2: p.PD5,
    };
    let sensors = modules::allowed(modules::Slot::Link2).then_some(&mut sensor_bus);
    park::park(parked, sensors, uart);
    drop(sensor_bus);
    uart.flush();

    sleep::init();
    uart::track_sleep();
    busy::init();
    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        let buttons = ButtonLines {
            up: ExtiInput::new(p.PD4, p.EXTI4, Pull::Up, ExtiIrqs),
            down: ExtiInput::new(p.PA5, p.EXTI5, Pull::Up, ExtiIrqs),
            back: ExtiInput::new(p.PD8, p.EXTI8, Pull::Up, ExtiIrqs),
            select: ExtiInput::new(p.PD15, p.EXTI15, Pull::None, ExtiIrqs),
        };
        // Open-drain from the PMIC and the USB-PD controller, with no pull-up on the board.
        let pwr_int = ExtiInput::new(p.PG7, p.EXTI7, Pull::Up, ExtiIrqs);
        spawn(spawner, "buttons", button_watch(buttons).unwrap());
        glass::arm_lag_probe(glass::LAG_BOOT_S);
        spawn(spawner, "lag", glass::lag_probe().unwrap());
        spawn(spawner, "heartbeat", heartbeat(flash).unwrap());
        spawn(spawner, "report", report(handoff, measured_hz, watchdog).unwrap());
        spawn(spawner, "display", display(bus, pwr_int).unwrap());
        if let Some(pins) = usb {
            spawn(spawner, "console", console::service(pins, flash).unwrap());
        }
        #[cfg(feature = "ble")]
        spawn(
            spawner,
            "ble",
            ble::run(
                ble::Pins {
                    spi: p.SPI1,
                    sck: p.PG2,
                    miso: p.PG3,
                    mosi: p.PB5,
                    cs: p.PA15,
                    irq: p.PA9,
                    irq_exti: p.EXTI9,
                    rst: p.PA8,
                },
                flash,
            )
            .unwrap(),
        );
    })
}

bind_interrupts!(struct I2cIrqs {
    I2C1_EV => i2c::EventInterruptHandler<peripherals::I2C1>;
    I2C1_ER => i2c::ErrorInterruptHandler<peripherals::I2C1>;
    GPDMA1_CHANNEL0 => dma::InterruptHandler<peripherals::GPDMA1_CH0>;
    GPDMA1_CHANNEL1 => dma::InterruptHandler<peripherals::GPDMA1_CH1>;
});

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

fn spawn<S>(spawner: embassy_executor::Spawner, name: &'static str, token: embassy_executor::SpawnToken<S>) {
    busy::name(token.id(), name);
    spawner.spawn(token);
}

const _: () = assert!(lcd::WIDTH == wrist_glass::WIDTH && lcd::HEIGHT == wrist_glass::HEIGHT);
static FRAMEBUFFER: ConstStaticCell<[u8; (lcd::WIDTH * lcd::HEIGHT) as usize]> =
    ConstStaticCell::new([0u8; (lcd::WIDTH * lcd::HEIGHT) as usize]);
/// The scene on the glass and the one being drawn, by turns.
static SCENES: ConstStaticCell<[Scene; 2]> = ConstStaticCell::new([Scene::new(), Scene::new()]);
/// Columns repainted between yields.
const STRIP: usize = 16;

/// The clocks every Stop exit restores (`HARDWARE.md`, "Stop modes"), with
/// the USB block's when `usb`.
pub(crate) fn rcc_config(usb: bool) -> rcc::Config {
    let mut c = rcc::Config::new();
    // OTG_HS needs AHB above 30 MHz (RM0456 73.3).
    c.msis = Some(rcc::MSIRange::Range48mhz);
    c.msik = None;
    c.hsi48 = None;
    c.ls = ls_config_as_found();
    c.sys = rcc::Sysclk::Msis;
    // As the time driver chooses LPTIM4's.
    c.mux.lptim1sel = if c.ls.lse.is_some() && unsafe { read32(RCC_BDCR) } & BDCR_LSERDY != 0 {
        rcc::mux::Lptimsel::Lse
    } else {
        rcc::mux::Lptimsel::Lsi
    };
    if usb {
        c.hse = Some(rcc::Hse {
            freq: Hertz::mhz(16),
            mode: rcc::HseMode::Oscillator,
        });
        c.mux.otghssel = rcc::Otghssel::Hse;
        // Its output is unused: PLL1 clocks the USB booster (`HARDWARE.md`, "USB device").
        c.pll1 = Some(rcc::Pll {
            source: rcc::PllSource::Hse,
            prediv: rcc::PllPreDiv::Div1,
            mul: rcc::PllMul::Mul10,
            divp: None,
            divq: None,
            divr: Some(rcc::PllDiv::Div2),
        });
    }
    c
}

const BDCR_LSEON: u32 = 1 << 0;
const BDCR_LSERDY: u32 = 1 << 1;
const BDCR_LSEBYP: u32 = 1 << 2;
const BDCR_LSESYSEN: u32 = 1 << 7;
const BDCR_RTCSEL_MASK: u32 = 0b11 << 8;
const BDCR_RTCSEL_LSE: u32 = 0b01 << 8;
const BDCR_RTCSEL_LSI: u32 = 0b10 << 8;
const BDCR_RTCEN: u32 = 1 << 15;

/// The LSE on, ready and in bypass, clocking the RTC (`HARDWARE.md`, "Stop modes").
fn backup_domain_as_measured(bdcr: u32) -> bool {
    let running = BDCR_LSEON | BDCR_LSERDY | BDCR_LSEBYP | BDCR_RTCSEL_LSE | BDCR_RTCEN;
    bdcr & (BDCR_LSEON | BDCR_LSERDY | BDCR_LSEBYP | BDCR_RTCSEL_MASK | BDCR_RTCEN) == running
}

/// The low-speed clocks as the backup domain holds them, so neither `init` nor a
/// Stop exit's restore resets the domain and the RTC; the LSI is on for the time
/// driver when the LSE is not ready.
fn ls_config_as_found() -> rcc::LsConfig {
    let bdcr = unsafe { read32(RCC_BDCR) };
    // `peripherals_clocked` is mirrored, so `init` sees no mismatch.
    let lse = (bdcr & (BDCR_LSEON | BDCR_LSEBYP) == BDCR_LSEON | BDCR_LSEBYP).then_some(rcc::LseConfig {
        frequency: Hertz(32_768),
        mode: rcc::LseMode::Bypass,
        peripherals_clocked: bdcr & BDCR_LSESYSEN != 0,
    });
    if backup_domain_as_measured(bdcr) {
        return rcc::LsConfig { rtc: rcc::RtcClockSource::Lse, lsi: false, lse };
    }
    let rtc = match (bdcr & BDCR_RTCEN != 0, bdcr & BDCR_RTCSEL_MASK) {
        (false, 0) => rcc::RtcClockSource::Disable,
        (true, BDCR_RTCSEL_LSE) if lse.is_some() => rcc::RtcClockSource::Lse,
        (true, BDCR_RTCSEL_LSI) => rcc::RtcClockSource::Lsi,
        // None the HAL can keep, so it resets the domain.
        _ => return rcc::LsConfig { lsi: true, ..rcc::LsConfig::off() },
    };
    let lse_ready = lse.is_some() && bdcr & BDCR_LSERDY != 0;
    rcc::LsConfig { rtc, lsi: rtc == rcc::RtcClockSource::Lsi || !lse_ready, lse }
}

/// `None`, with the reason on the UART, if the crystal or the USB supplies did
/// not come up.
fn usb_console(
    uart: Uart,
    hse_ok: bool,
    otg: Peri<'static, peripherals::USB_OTG_HS>,
    dp: Peri<'static, peripherals::PA12>,
    dm: Peri<'static, peripherals::PA11>,
    rcc: Peri<'static, peripherals::RCC>,
) -> Option<console::Pins> {
    usblog::install();
    let power = if hse_ok {
        unsafe { usb_power_ready() }
    } else {
        Err("no crystal, no USB")
    };
    let parts = match power {
        Ok(()) => {
            uart.write_line(
                "[11] USB supplies up (VDDUSBRDY, USBBOOSTRDY); CDC console service on OTG_HS (PA11/PA12)",
            );
            Some(console::Pins { otg, dp, dm, rcc })
        }
        Err(why) => {
            uart.write_str("[11] USB skipped: ");
            uart.write_line(why);
            None
        }
    };
    uart.flush();
    parts
}

/// The `OTG_HS` supplies up within bounded waits (RM0456 10.7.12); the error
/// names the flag that stayed clear.
pub(crate) unsafe fn usb_power_ready() -> Result<(), &'static str> {
    const BUDGET: u32 = 500_000;

    // RM0456 15.2.3 step 3.
    const SYSCFG_OTGHSPHYTUNER2: u32 = 0x4600_0400 + 0x7C;
    modify32(SYSCFG_OTGHSPHYTUNER2, 0b111 | (0b111 << 4), 0b010);

    modify32(PWR_SVMCR, 0, PWR_SVMCR_UVMEN);
    let mut ok = false;
    for _ in 0..BUDGET {
        if read32(PWR_SVMSR) & PWR_SVMSR_VDDUSBRDY != 0 {
            ok = true;
            break;
        }
    }
    if !ok {
        return Err("VDDUSB not ready (VDDUSBRDY stayed 0: is the 3V3 rail on VDDUSB?)");
    }
    modify32(PWR_SVMCR, 0, PWR_SVMCR_USV);
    modify32(PWR_VOSR, 0, PWR_VOSR_USBPWREN | PWR_VOSR_USBBOOSTEN);
    for _ in 0..BUDGET {
        if read32(PWR_VOSR) & PWR_VOSR_USBBOOSTRDY != 0 {
            return Ok(());
        }
    }
    Err("OTG_HS booster not ready (USBBOOSTRDY stayed 0: no EPOD clock from PLL1MBOOST?)")
}

pub(crate) unsafe fn usb_power_off() {
    modify32(PWR_VOSR, PWR_VOSR_USBBOOSTEN | PWR_VOSR_USBPWREN, 0);
    modify32(PWR_SVMCR, PWR_SVMCR_USV | PWR_SVMCR_UVMEN, 0);
}

/// [`start_hse`]'s 20 ms, in polls at each core clock.
pub(crate) const HSE_POLLS_4MHZ: u32 = 20_000;
pub(crate) const HSE_POLLS_48MHZ: u32 = 240_000;

/// False, with `HSEON` cleared again, if `HSERDY` did not set within `polls`
/// (RM0456 11.8.1).
pub(crate) unsafe fn start_hse(polls: u32) -> bool {
    const HSEON: u32 = 1 << 16;
    const HSERDY: u32 = 1 << 17;
    modify32(RCC_CR, 0, HSEON);
    for _ in 0..polls {
        if read32(RCC_CR) & HSERDY != 0 {
            return true;
        }
    }
    modify32(RCC_CR, HSEON, 0);
    false
}

bind_interrupts!(struct ExtiIrqs {
    EXTI4 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI4>;
    EXTI5 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI5>;
    EXTI7 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI7>;
    EXTI8 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI8>;
    EXTI15 => exti::InterruptHandler<embassy_stm32::interrupt::typelevel::EXTI15>;
});

struct ButtonLines {
    up: ExtiInput<'static, Async>,
    down: ExtiInput<'static, Async>,
    back: ExtiInput<'static, Async>,
    select: ExtiInput<'static, Async>,
}

impl ButtonLines {
    async fn any_press(&mut self) {
        select4(
            self.up.wait_for_falling_edge(),
            self.down.wait_for_falling_edge(),
            self.back.wait_for_falling_edge(),
            self.select.wait_for_falling_edge(),
        )
        .await;
    }
}

/// Short presses, and the button edges that woke the button task, for `power`.
static PRESSES: AtomicU32 = AtomicU32::new(0);
static PRESS_EDGES: AtomicU32 = AtomicU32::new(0);
/// Presses from the console's `press`, counted apart so a battery stretch's figures are real.
static INJECTED: AtomicU32 = AtomicU32::new(0);

const PRESS_QUEUE: usize = 4;
static PRESS_IN: Channel<CriticalSectionRawMutex, Press, PRESS_QUEUE> = Channel::new();

/// Hand the button task a press, as if released after debouncing; false while the queue is full.
pub fn inject_press(press: Press) -> bool {
    PRESS_IN.try_send(press).is_ok()
}

/// Where the wearer is among the pages.
static NAV: Mutex<CriticalSectionRawMutex, Cell<Nav>> = Mutex::new(Cell::new(Nav::new()));

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// The page to draw now, back on the face once a page's timeout has passed; and when that
/// timeout falls, if a page other than the face is up.
pub fn nav_now() -> (Page, Option<Instant>) {
    let (timed_out, now) = NAV.lock(|n| {
        let mut nav = n.get();
        let timed_out = nav.tick(now_ms());
        n.set(nav);
        (timed_out, (nav.page(), nav.deadline_ms().map(Instant::from_millis)))
    });
    if timed_out {
        Uart::best_effort().write_line("[page] face, timed out");
    }
    now
}

const _: () = assert!(BUTTONS.len() == Button::ALL.len());

/// The `[button]` line of `power`: a battery stretch's wake rate depends on how often the
/// face was lit.
pub fn write_buttons(uart: Uart) {
    uart.write_str("[button] short presses ");
    uart.write_u32(PRESSES.load(Ordering::Relaxed));
    uart.write_str(", edges that woke the button task ");
    uart.write_u32(PRESS_EDGES.load(Ordering::Relaxed));
    uart.write_str("; injected by `press` ");
    uart.write_u32(INJECTED.load(Ordering::Relaxed));
    uart.write_line(" (not counted in either)");
}

/// Feeds the watchdog, so a stalled executor resets into the bootloader.
#[embassy_executor::task]
async fn button_watch(mut lines: ButtonLines) -> ! {
    /// Must stay well under the watchdog's period.
    const FEED: Duration = Duration::from_secs(8);
    const SAMPLE_MS: u64 = 50;
    const HOLD_SAMPLES: u32 = 60;
    const SHORT_SAMPLES: u32 = 20;
    let uart = Uart::best_effort();
    let mut seen_up: u32 = 0;
    let mut held_for: u32 = 0;
    let mut was_down: u32 = 0;
    // Set only for a press's light; a press turns one switched on from the console off.
    let mut light_until: Option<Instant> = None;
    loop {
        watchdog::refresh();
        let down = buttons_down();
        // A button counts once seen released, so a stuck-low pin cannot reset the watch.
        seen_up |= !down & 0b1111;
        let deliberate = down & seen_up;
        let released = was_down & !down;
        if released != 0 {
            let long = held_for >= SHORT_SAMPLES;
            if !long {
                PRESSES.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(button) = Button::from_index(released.trailing_zeros() as usize) {
                on_press(uart, Press { button, long }, false, &mut light_until);
            }
        }
        // Injected presses never touch the hold count, so they cannot reach the reset.
        while let Ok(press) = PRESS_IN.try_receive() {
            INJECTED.fetch_add(1, Ordering::Relaxed);
            on_press(uart, press, true, &mut light_until);
        }
        if light_until.is_some_and(|off| Instant::now() >= off) {
            backlight(false);
            light_until = None;
        }
        was_down = deliberate;
        held_for = if deliberate == 0 { 0 } else { held_for + 1 };
        if held_for >= HOLD_SAMPLES {
            Uart::retry();
            uart.write_line("");
            uart.write_str("[step] button held: ");
            for button in Button::ALL {
                if down & (1 << button.index()) != 0 {
                    uart.write_str(button.label());
                    uart.write_str(" ");
                }
            }
            uart.write_line(
                "-- resetting into the bootloader (keep holding Select for its USB mode)",
            );
            uart.flush();
            watchdog::terminal_reset();
        }
        // Sampled only while a button is down, to time its hold and see its release; a lit
        // face waits for its light's end like any other timer.
        if deliberate == 0 {
            let feed = Instant::now() + FEED;
            let until = light_until.map_or(feed, |off| off.min(feed));
            match select3(lines.any_press(), Timer::at(until), PRESS_IN.ready_to_receive()).await {
                Either3::First(()) => {
                    PRESS_EDGES.fetch_add(1, Ordering::Relaxed);
                }
                Either3::Second(()) | Either3::Third(()) => {}
            }
        } else {
            Timer::after_millis(SAMPLE_MS).await;
        }
    }
}

/// What a debounced press does: a short one lights the face, or turns off a light the console
/// switched on, on every page; then the pages' model moves where it says.
fn on_press(uart: Uart, press: Press, injected: bool, light_until: &mut Option<Instant>) {
    const LIT_FOR: Duration = Duration::from_secs(3);
    uart.write_str(if injected { "[button] injected " } else { "[button] " });
    uart.write_str(if press.long { "long press " } else { "short press " });
    uart.write_str(press.button.label());
    if press.long {
        uart.write_line(": no light");
    } else if light_until.is_none() && lcd::light_on() {
        backlight(false);
        uart.write_line(": the console's light off");
    } else {
        backlight(true);
        *light_until = Some(Instant::now() + LIT_FOR);
        uart.write_line(": face lit for 3 s");
    }
    let (moved, page) = NAV.lock(|n| {
        let mut nav = n.get();
        let moved = nav.press(press, now_ms());
        n.set(nav);
        (moved, nav.page())
    });
    if moved {
        uart.write_str("[page] ");
        uart.write_line(page.name());
        wake_display();
    }
}

#[embassy_executor::task]
async fn display(mut bus: pmic::AsyncBus, mut pwr_int: ExtiInput<'static, Async>) -> ! {
    /// On USB; on battery the gauge is read with each new minute's face.
    const READING: Duration = Duration::from_secs(10);
    let uart = Uart::best_effort();
    let mut power = power::Sampler::new();
    // `None` while the display slot holds another module.
    let mut panel: Option<lcd::Lcd> = None;
    if !modules::allowed(modules::Slot::View) {
        uart.write_line("[mod] panel not started: view holds another module (`modules force view` starts it)");
    }
    let fb = FRAMEBUFFER.take();
    let scenes = SCENES.take();
    let mut n: u32 = 0;
    let mut next_reading = Instant::now();
    let mut soc_pct = None;
    let mut cell_mv = None;
    let mut last_minute = None;
    let mut shown_page = Page::Face;
    // `None` forces the next frame.
    let mut drawn: Option<Glass> = None;
    loop {
        // Reset before reading the state it announces, so a request during this pass is not lost.
        DISPLAY_WAKE.reset();
        if panel.is_none() && modules::allowed(modules::Slot::View) {
            let mut started = unsafe { lcd::Lcd::init() };
            started.start().await;
            uart.write_line("[lcd] initialization frame written; VCOM running from LPTIM1 and LPTIM3");
            panel = Some(started);
            drawn = None;
        }
        match rtc::apply_zone_change() {
            Ok(None) => {}
            Ok(Some(shift)) => {
                uart.write_str("[rtc] zone change applied: now ");
                rtc::write_zone(uart, rtc::zone().map_or(0, |z| z.offset));
                uart.write_line(if shift < 0 { ", calendar back" } else { ", calendar forward" });
            }
            Err(why) => {
                uart.write_str("[rtc] zone change not applied: ");
                uart.write_line(why);
            }
        }
        let minute = minute_now();
        let new_minute = last_minute != Some(minute);
        last_minute = Some(minute);
        // A still image is rewritten within two hours against sticking (LCP-2619063C).
        if new_minute && minute.is_multiple_of(60) {
            drawn = None;
        }
        if power.due(pwr_int.is_low(), new_minute) {
            power.service(&mut bus).await;
        }
        let page = nav_now().0;
        let reading_due = if power::on_battery() { new_minute } else { Instant::now() >= next_reading };
        // A page opened on battery would otherwise show a reading up to a minute old.
        if reading_due || (page == Page::Battery && shown_page != Page::Battery) {
            next_reading = Instant::now() + READING;
            n = n.wrapping_add(1);
            (soc_pct, cell_mv) = read_power(uart, &mut bus, n).await;
        }
        if let Some(panel) = panel.as_mut() {
            let t0 = Instant::now();
            let (composed, short) = compose_page(page, soc_pct, cell_mv);
            glass::note_max(&glass::MAX_COMPOSE_US, elapsed_us(t0));
            embassy_futures::yield_now().await;
            show(panel, fb, scenes, uart, &mut drawn, (&composed, short)).await;
        }
        shown_page = page;
        if panel.is_some() { panel_dma::between_frames(uart) }
        if glass::dump_wanted() {
            if panel.is_none() {
                uart.write_line("[frame] the panel is not started; the buffer holds no face");
            }
            glass::dump(fb).await;
        }
        // Asleep until the minute turns, unless USB, the input line or a request needs it sooner.
        let line_low = pwr_int.is_low();
        let mut due = Instant::now() + until_next_minute();
        if let (_, Some(back_to_face)) = nav_now() {
            due = due.min(back_to_face);
        }
        if !power::on_battery() {
            due = due.min(next_reading);
        }
        if let Some((_, _, Some(ms))) = glass::stress() {
            glass::stress_stepped();
            due = due.min(Instant::now() + Duration::from_millis(u64::from(ms)));
        }
        if line_low {
            due = due.min(power.retry_at());
        }
        let input_changed = async {
            if line_low {
                core::future::pending::<()>().await;
            } else {
                pwr_int.wait_for_falling_edge().await;
            }
        };
        select3(Timer::at(due), DISPLAY_WAKE.wait(), input_changed).await;
    }
}

/// The page, or the stress screen the console put in its place.
fn compose_page(page: Page, soc_pct: Option<u8>, cell_mv: Option<u32>) -> (Glass, pages::Short) {
    if let Some((kind, step, _)) = glass::stress() {
        return wrist_glass::stress::compose_stress(kind, step, pages::FACES);
    }
    let locale = &pages::EN_GB;
    match page {
        Page::Face => pages::compose(&glass::status(soc_pct), locale, pages::FACES),
        Page::Battery => pages::compose_battery(
            &pages::Battery {
                soc_pct,
                cell_mv,
                external: power::external_input(),
            },
            locale,
            pages::FACES,
        ),
        Page::About => pages::compose_about(
            &pages::About {
                version: version::VERSION,
                build: version::BUILD_ID,
            },
            locale,
            pages::FACES,
        ),
    }
}

/// `scenes[0]` lays out `drawn` while it is `Some`.
async fn show(
    panel: &mut lcd::Lcd,
    fb: &mut [u8],
    scenes: &mut [Scene; 2],
    uart: Uart,
    drawn: &mut Option<Glass>,
    (composed, short): (&Glass, pages::Short),
) {
    if drawn.as_ref() == Some(composed) {
        glass::UNCHANGED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if short != 0 {
        uart.write_str("[glass] lines cut short: ");
        uart.write_hex8(short);
        uart.write_line("");
    }
    let mut cost = glass::Cost::default();
    let [shown, scene] = scenes;
    for i in 0..composed.len().max(1) {
        let c = busy::cycles();
        scene.lay(composed, i);
        cost.layout.add(c);
        embassy_futures::yield_now().await;
    }
    let before = drawn.is_some().then_some(&*shown);
    let c = busy::cycles();
    let changed = scene.changed_columns(before);
    cost.changed.add(c);
    embassy_futures::yield_now().await;
    for columns in wrist_glass::text::strips(&changed, STRIP) {
        let t = Instant::now();
        let c = busy::cycles();
        pages::repaint(fb, scene, before, columns);
        cost.render.add(c);
        glass::note_max(&glass::MAX_RENDER_US, elapsed_us(t));
        embassy_futures::yield_now().await;
    }
    let start = glass::now_ms();
    let (columns, write) = panel.write_frame(fb, &changed).await;
    cost.write = glass::Step { total: write.busy, longest: write.longest };
    let ms = glass::now_ms().saturating_sub(start);
    scenes.swap(0, 1);
    glass::LAST_FRAME_COLUMNS.store(columns, Ordering::Relaxed);
    *drawn = Some(*composed);
    glass::shown(composed);
    glass::cost(cost);
    glass::FRAMES.fetch_add(1, Ordering::Relaxed);
    glass::LAST_FRAME_MS.store(ms, Ordering::Relaxed);
    glass::note_max(&glass::MAX_FRAME_MS, ms);
}

fn elapsed_us(t: Instant) -> u32 {
    u32::try_from(t.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// Until `minute_now` changes, and a little more so the wake lands after it.
fn until_next_minute() -> Duration {
    let into = rtc::ms_into_minute()
        .unwrap_or_else(|| u32::try_from(Instant::now().as_millis() % 60_000).unwrap_or(0));
    Duration::from_millis(u64::from(60_000 - into.min(59_999)) + 5)
}

/// The minute the face shows, or the uptime's while the RTC is unset.
fn minute_now() -> u32 {
    rtc::now_civil().map_or_else(
        || u32::try_from(Instant::now().as_secs() / 60).unwrap_or(0) | (1 << 31),
        |c| u32::from(c.hour) * 60 + u32::from(c.minute),
    )
}

/// The charge in percent and the cell's voltage, for the face and the battery page.
async fn read_power(uart: Uart, bus: &mut pmic::AsyncBus, n: u32) -> (Option<u8>, Option<u32>) {
    let s = pmic::snapshot(bus, n.is_multiple_of(6)).await;
    pmic::restore_gauge(bus, s.gauge_status).await;
    if !s.gauge_status.is_some_and(wrist_power::gauge_reset) {
        pmic::keep_hibernate_slow(&mut uart.lines(), bus, s.hib_cfg).await;
    }
    if s.full {
        pmic::write_report(&mut uart.lines(), &s);
        embassy_futures::yield_now().await;
        pmic::write_rails(&mut uart.lines(), s.top_cntl3);
        embassy_futures::yield_now().await;
    }
    pmic::write_summary(&mut uart.lines(), &s);
    power::note_reading(&s);
    (s.soc_raw.map(pmic::soc_percent), s.vcell_raw.map(pmic::vcell_mv))
}

static DISPLAY_WAKE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Call after setting the state the display task should act on.
pub fn wake_display() {
    DISPLAY_WAKE.signal(());
}

/// Keeps the image once it has run a while, and an update only once a way to send the next
/// one has worked: a host configured its USB, or (`ble` feature) an encrypted Bluetooth link
/// was up.
fn bluetooth_proven() -> bool {
    #[cfg(feature = "ble")]
    return ble::proven();
    #[cfg(not(feature = "ble"))]
    false
}

async fn bluetooth_proven_changed() {
    #[cfg(feature = "ble")]
    ble::proven_changed().await;
    #[cfg(not(feature = "ble"))]
    core::future::pending::<()>().await;
}

#[embassy_executor::task]
async fn heartbeat(flash: &'static update::FlashMutex) -> ! {
    const MARK_BOOTED_AT: u32 = 15;
    let uart = Uart::best_effort();
    let mut changes = power::changes();
    let mut n: u32 = 0;
    let mut kept = false;
    let mut said_waiting = false;
    loop {
        // On battery nothing hears the line, so no wake a second until a cable is in.
        if n >= MARK_BOOTED_AT {
            // An update not yet kept is also kept when a Bluetooth link proves itself.
            embassy_futures::select::select(power::wait_while_on_battery(&mut changes), async {
                if kept {
                    core::future::pending::<()>().await;
                }
                bluetooth_proven_changed().await;
            })
            .await;
        }
        n = n.wrapping_add(1);
        Uart::retry();
        if n >= MARK_BOOTED_AT && !kept {
            if update::just_updated(flash) && !console::host_configured() && !bluetooth_proven() {
                if !said_waiting {
                    uart.write_line("[boot] not kept yet: waiting for a host to configure USB, or an encrypted Bluetooth link");
                    said_waiting = true;
                }
            } else {
                kept = true;
                let via = if !update::just_updated(flash) {
                    ""
                } else if console::host_configured() {
                    "; kept: a USB host configured"
                } else {
                    "; kept: an encrypted Bluetooth link"
                };
                match update::mark_booted(flash) {
                    Ok(was) => {
                        uart.write_str("[boot] marked booted; state was ");
                        uart.write_str(was);
                        uart.write_line(via);
                    }
                    Err(()) => {
                        uart.write_line("[boot] could not mark booted: state partition write failed");
                    }
                }
            }
        }
        uart.write_str("[hb] ");
        uart.write_u32(n);
        uart.write_str("  uptime ");
        uart.write_u32(u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX));
        uart.write_str(" s  buttons ");
        uart.write_u32(buttons_down());
        uart.write_str("  dropped ");
        uart.write_u32(Uart::dropped());
        uart.write_line("");
        Timer::after_millis(1000).await;
    }
}

#[embassy_executor::task]
async fn report(handoff: RccSnapshot, measured_hz: u32, watchdog: watchdog::Started) -> ! {
    let uart = Uart::best_effort();
    full_report(uart, &handoff, measured_hz, watchdog, "boot").await;
    loop {
        match REPORT.wait().await {
            Report::Full => full_report(uart, &handoff, measured_hz, watchdog, "asked for").await,
            Report::UsbLog => usblog::dump(uart).await,
            Report::Staging(len) => update::staging_crcs(uart, len).await,
            Report::FlashSurvey => flash::survey(uart).await,
        }
    }
}

/// RM0456 11.8.50.
fn report_reset_cause(uart: Uart, csr: u32) {
    uart.write_str("reset flags : ");
    for (bit, name) in [
        (31, "LPWR "),
        (30, "WWDG "),
        (29, "IWDG "),
        (28, "SFT "),
        (27, "BOR "),
        (26, "PIN "),
        (25, "OBL "),
    ] {
        if csr & (1 << bit) != 0 {
            uart.write_str(name);
        }
    }
    uart.write_line("");
}

fn report_buttons(uart: Uart) {
    uart.write_str("    buttons : ");
    for ((base, pin, _), button) in BUTTONS.into_iter().zip(Button::ALL) {
        uart.write_str(button.label());
        uart.write_str(if unsafe { read32(base + GPIO_IDR) } & (1 << pin) == 0 {
            "=DOWN "
        } else {
            "=up "
        });
    }
    uart.write_line("(up is the released state; DOWN with nothing pressed means a wrong pin)");
}

async fn full_report(
    uart: Uart,
    handoff: &RccSnapshot,
    measured_hz: u32,
    watchdog: watchdog::Started,
    why: &str,
) {
    use console::make_room;
    uart.write_line("");
    uart.write_str("=== wrist-rs, report at ");
    uart.write_u32(u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX));
    uart.write_str(" s (");
    uart.write_str(why);
    uart.write_line(") ===");

    uart.write_str("[step] link  : ");
    uart.write_hex32(image_origin());
    uart.write_str("  USART3_TX PC4 AF7, ");
    uart.write_u32(Uart::baud());
    uart.write_str(" baud 8N1, BRR ");
    uart.write_u32(Uart::brr());
    uart.write_str(" from ");
    uart.write_u32(measured_hz);
    uart.write_line(" Hz");

    uart.write_str("[step] wdog  : ");
    uart.write_line(watchdog.as_str());
    uart.write_str("[step] build : ");
    uart.write_line(version::BUILD_ID);
    make_room().await;

    uart.write_line("[step] rtc   :");
    rtc::write_report(uart);
    make_room().await;
    uart.write_line("[step] usb   :");
    console::write_regs(uart);
    make_room().await;
    usblog::dump(uart).await;

    uart.write_line("[step] clocks:");
    report_handoff(uart, handoff);
    make_room().await;
    report_clocks(uart);
    report_enables(uart);
    make_room().await;
    uart.write_line("[step] park  :");
    park::write_report(uart);
    make_room().await;
    uart.write_line("[step] ident :");
    report_identity(uart);
    uart.write_line("[step] vtable:");
    report_vector_table(uart);
    make_room().await;

    let dropped = Uart::dropped();
    uart.write_str("[step] output: ");
    if dropped == 0 {
        uart.write_line("no bytes dropped");
    } else {
        uart.write_str("DROPPED ");
        uart.write_u32(dropped);
        uart.write_line(" bytes since boot -- lines above may be truncated");
    }
}

/// The banner at each of [`SWEEP_BAUDS`], so a terminal at any of them decodes
/// the rate to switch to.
fn sweep_banner(clock_hz: u32) -> Uart {
    for rate in SWEEP_BAUDS {
        let uart = unsafe { Uart::configure(clock_hz, rate) };
        uart.write_line("");
        uart.write_str("== wrist-rs == this line decoded at ");
        uart.write_u32(rate);
        uart.write_str(" baud; the report follows at ");
        uart.write_u32(BAUD);
        uart.write_line(" -- set the terminal to that");
        uart.flush();
    }
    unsafe { Uart::configure(clock_hz, BAUD) }
}

/// The USART3 kernel clock, and whether to trust it: not off MSIS, nor with a
/// bus prescaler possibly set.
fn measure_pclk1(uart: Uart) -> (u32, bool) {
    let cfgr1 = unsafe { read32(RCC_CFGR1) };
    let cfgr2 = unsafe { read32(RCC_CFGR2) };
    let icscr1 = unsafe { read32(RCC_ICSCR1) };
    let csr = unsafe { read32(RCC_CSR) };

    let on_msis = cfgr1 & RCC_CFGR1_SWS_MASK == 0;
    let prescalers_default = cfgr2 == RCC_CFGR2_RESET;

    if !on_msis {
        uart.write_line("!! SYSCLK is not MSIS; cannot derive the clock here");
        return (PCLK1_HZ, false);
    }
    if !prescalers_default {
        uart.write_str("!! RCC_CFGR2 is ");
        uart.write_hex32(cfgr2);
        uart.write_str(", not the documented reset ");
        uart.write_hex32(RCC_CFGR2_RESET);
        uart.write_line("; a bus prescaler may be set, so the clock is unknown");
        return (PCLK1_HZ, false);
    }

    let range = if icscr1 & RCC_ICSCR1_MSIRGSEL != 0 {
        icscr1 >> 28
    } else {
        (csr >> 12) & 0xF
    };
    (msis_range_khz(range) * 1000, true)
}

fn report_handoff(uart: Uart, h: &RccSnapshot) {
    uart.write_line("-- RCC at handoff, before this image touched it --");
    uart.write_reg("RCC_CR      = ", h.cr);
    uart.write_reg("RCC_ICSCR1  = ", h.icscr1);
    uart.write_reg("RCC_CFGR1   = ", h.cfgr1);
    uart.write_reg("RCC_CFGR2   = ", h.cfgr2);
    uart.write_reg("RCC_CSR     = ", h.csr);
    uart.write_reg("RCC_CCIPR1  = ", h.ccipr1);
    uart.write_reg("RCC_PLL1CFGR= ", h.pll1cfgr);
    uart.write_reg("FLASH_ACR   = ", h.flash_acr);
    uart.write_str("SYSCLK src  = ");
    uart.write_line(match (h.cfgr1 & RCC_CFGR1_SWS_MASK) >> 2 {
        0b00 => "MSIS",
        0b01 => "HSI16",
        0b10 => "HSE",
        _ => "PLL1 pll1_r_ck",
    });
    uart.write_str("PLL1ON      = ");
    uart.write_u32((h.cr >> 24) & 1);
    uart.write_str("  HSION = ");
    uart.write_u32((h.cr >> 8) & 1);
    uart.write_str("  HSEON = ");
    uart.write_u32((h.cr >> 16) & 1);
    uart.write_str("  MSISON = ");
    uart.write_u32(h.cr & 1);
    uart.write_line("");
}

fn report_clocks(uart: Uart) {
    uart.write_line("-- RCC now --");
    let cr = unsafe { read32(RCC_CR) };
    let icscr1 = unsafe { read32(RCC_ICSCR1) };
    let csr = unsafe { read32(RCC_CSR) };
    let cfgr1 = unsafe { read32(RCC_CFGR1) };

    uart.write_reg("RCC_CR      = ", cr);
    uart.write_reg("RCC_ICSCR1  = ", icscr1);
    uart.write_reg("RCC_ICSCR2  = ", unsafe { read32(RCC_ICSCR2) });
    uart.write_reg("RCC_ICSCR3  = ", unsafe { read32(RCC_ICSCR3) });
    uart.write_reg("RCC_CFGR1   = ", cfgr1);
    uart.write_reg("RCC_CFGR2   = ", unsafe { read32(RCC_CFGR2) });
    uart.write_reg("RCC_CSR     = ", csr);
    uart.write_reg("RCC_CCIPR1  = ", unsafe { read32(RCC_CCIPR1) });
    uart.write_reg("FLASH_ACR   = ", unsafe { read32(FLASH_ACR) });
    uart.write_reg("FLASH_OPTR  = ", unsafe { read32(FLASH_OPTR) });

    uart.write_str("SYSCLK src  = ");
    uart.write_line(match (cfgr1 & RCC_CFGR1_SWS_MASK) >> 2 {
        0b00 => "MSIS",
        0b01 => "HSI16",
        0b10 => "HSE",
        _ => "PLL1 pll1_r_ck",
    });

    let rgsel = u32::from(icscr1 & RCC_ICSCR1_MSIRGSEL != 0);
    let range = if rgsel == 1 {
        icscr1 >> 28
    } else {
        (csr >> 12) & 0xF
    };
    uart.write_str("MSIRGSEL    = ");
    uart.write_u32(rgsel);
    uart.write_str("  -> MSIS range ");
    uart.write_u32(range);
    uart.write_str(" (~");
    uart.write_u32(msis_range_khz(range));
    uart.write_line(" kHz nominal)");

    uart.write_str("PLL1ON      = ");
    uart.write_u32((cr >> 24) & 1);
    uart.write_str("  HSION = ");
    uart.write_u32((cr >> 8) & 1);
    uart.write_str("  HSEON = ");
    uart.write_u32((cr >> 16) & 1);
    uart.write_str("  MSISON = ");
    uart.write_u32(cr & 1);
    uart.write_line("");
}

/// RM0456 11.8.23 to 11.8.31.
fn report_enables(uart: Uart) {
    uart.write_line("-- clocks on --");
    for (name, addr) in [
        ("RCC_AHB1ENR = ", RCC_AHB1ENR),
        ("RCC_AHB2ENR1= ", RCC_AHB2ENR1),
        ("RCC_AHB2ENR2= ", RCC_AHB2ENR2),
        ("RCC_AHB3ENR = ", RCC_AHB3ENR),
        ("RCC_APB1ENR1= ", RCC_APB1ENR1),
        ("RCC_APB1ENR2= ", RCC_APB1ENR2),
        ("RCC_APB2ENR = ", RCC_APB2ENR),
        ("RCC_APB3ENR = ", RCC_APB3ENR),
        ("ICACHE_CR   = ", ICACHE_CR),
    ] {
        uart.write_reg(name, unsafe { read32(addr) });
    }
}

/// RM0456 11.8.2.
fn msis_range_khz(range: u32) -> u32 {
    match range {
        0 => 48_000,
        1 => 24_000,
        2 => 16_000,
        3 => 12_000,
        4 => 4_000,
        5 => 2_000,
        6 => 1_330,
        7 => 1_000,
        8 => 3_072,
        9 => 1_536,
        10 => 1_024,
        11 => 768,
        12 => 400,
        13 => 200,
        14 => 133,
        _ => 100,
    }
}

fn report_identity(uart: Uart) {
    uart.write_line("-- identity --");
    let idcode = unsafe { read32(DBGMCU_IDCODE) };
    uart.write_reg("DBGMCU_IDCODE = ", idcode);
    uart.write_str("  DEV_ID = ");
    uart.write_hex32(idcode & 0xFFF);
    uart.write_str("  REV_ID = ");
    uart.write_hex32(idcode >> 16);
    uart.write_line("");
}

fn report_vector_table(uart: Uart) {
    uart.write_line("-- vector table at image origin --");
    let origin = image_origin();
    let sp = unsafe { read32(origin) };
    let reset = unsafe { read32(origin + 4) };

    uart.write_reg("word[0] initial SP = ", sp);
    uart.write_reg("word[1] reset vec  = ", reset);

    uart.write_str("SP in [");
    uart.write_hex32(*SRAM_WINDOW.start());
    uart.write_str(", ");
    uart.write_hex32(*SRAM_WINDOW.end());
    uart.write_str("] : ");
    uart.write_line(if SRAM_WINDOW.contains(&sp) {
        "yes (bootloader's only check passes)"
    } else {
        "NO -- this image would never have been entered"
    });

    uart.write_str("SP word + 16 still in SRAM: ");
    uart.write_line(if SRAM_WINDOW.contains(&sp.saturating_add(16)) {
        "yes"
    } else {
        "NO -- the bootloader's own pop reads past the end of SRAM"
    });

    uart.write_str("reset vec Thumb bit: ");
    uart.write_line(if reset & 1 == 1 {
        "set"
    } else {
        "CLEAR -- wrong"
    });
}

/// A busy wait counted in iterations, for when no clock can be trusted.
pub(crate) fn crude_delay() {
    for _ in 0..400_000u32 {
        core::hint::spin_loop();
    }
}

fn short_delay() {
    for _ in 0..100_000u32 {
        core::hint::spin_loop();
    }
}

/// Every change to the light goes through here, so `VCOM` can follow it.
pub(crate) fn backlight(on: bool) {
    light_pin(on);
    lcd::note_light(on);
}

/// The light alone, for the boot beacon and the fault flasher, which leave `VCOM` be.
fn light_pin(on: bool) {
    unsafe {
        write32(
            GPIOF_BASE + GPIO_BSRR,
            if on { 1 << (3 + 16) } else { 1 << 3 },
        );
    }
}

/// (port, pin, `PUPDR`), in [`Button::ALL`]'s order; Select's pin is driven, so it has no
/// pull (`HARDWARE.md`, "Power and reset domain").
const BUTTONS: [(u32, u32, u32); 4] = [
    (GPIOD_BASE, 4, 0b01),
    (GPIOA_BASE, 5, 0b01),
    (GPIOD_BASE, 8, 0b01),
    (GPIOD_BASE, 15, 0b00),
];

unsafe fn init_buttons() {
    enable_clock(RCC_AHB2ENR1, RCC_AHB2ENR1_GPIOAEN | RCC_AHB2ENR1_GPIODEN);
    for (base, pin, pull) in BUTTONS {
        modify32(base + GPIO_MODER, 0b11 << (pin * 2), 0);
        modify32(base + GPIO_PUPDR, 0b11 << (pin * 2), pull << (pin * 2));
    }
}

/// Bit `i` set when button `i` of [`BUTTONS`] is down.
fn buttons_down() -> u32 {
    let mut mask = 0;
    for (i, (base, pin, _)) in BUTTONS.iter().enumerate() {
        if unsafe { read32(base + GPIO_IDR) } & (1 << pin) == 0 {
            mask |= 1 << i;
        }
    }
    mask
}

/// `pulses` flashes then a pause: progress readable by eye before the UART is up.
pub(crate) fn beacon(pulses: u32) {
    light_pin(false);
    short_delay();
    short_delay();
    for _ in 0..pulses {
        light_pin(true);
        short_delay();
        light_pin(false);
        short_delay();
        short_delay();
    }
    for _ in 0..4 {
        short_delay();
    }
}

pub(crate) fn flicker() {
    for _ in 0..10 {
        light_pin(true);
        for _ in 0..40_000u32 {
            core::hint::spin_loop();
        }
        light_pin(false);
        for _ in 0..40_000u32 {
            core::hint::spin_loop();
        }
    }
}

fn report_fault_registers(uart: Uart, name: &str) {
    uart.write_line("");
    uart.write_str("### FAULT: ");
    uart.write_line(name);
    uart.write_reg("SCB_CFSR  = ", unsafe { read32(SCB_CFSR) });
    uart.write_reg("SCB_HFSR  = ", unsafe { read32(SCB_HFSR) });
    uart.write_reg("SCB_MMFAR = ", unsafe { read32(SCB_MMFAR) });
    uart.write_reg("SCB_BFAR  = ", unsafe { read32(SCB_BFAR) });
    uart.write_reg("SCB_SHCSR = ", unsafe { read32(SCB_SHCSR) });
}

/// Repeated so a capture attached mid-fault catches one, and finite so the
/// fault path still resets.
const FAULT_REPORTS: u32 = 10;

fn fault_then_reset(name: &str, frame: Option<&ExceptionFrame>) -> ! {
    // The retained record first, in case reporting fails.
    let pc = frame.map_or(0, ExceptionFrame::pc);
    crash::fault(unsafe { read32(SCB_CFSR) }, pc);
    let uart = Uart::direct();
    for _ in 0..FAULT_REPORTS {
        flicker();
        Uart::retry();
        report_fault_registers(uart, name);
        if let Some(frame) = frame {
            uart.write_reg("stacked PC   = ", frame.pc());
            uart.write_reg("stacked LR   = ", frame.lr());
            uart.write_reg("stacked xPSR = ", frame.xpsr());
            uart.write_reg("stacked R0   = ", frame.r0());
            uart.write_reg("stacked R1   = ", frame.r1());
            uart.write_reg("stacked R2   = ", frame.r2());
            uart.write_reg("stacked R3   = ", frame.r3());
            uart.write_reg("stacked R12  = ", frame.r12());
        } else {
            uart.write_line("(no stacked frame recovered for this handler)");
        }
        uart.flush();
        crude_delay();
    }
    uart.write_line("### resetting after fault");
    uart.flush();
    watchdog::terminal_reset()
}

#[exception]
unsafe fn HardFault(frame: &ExceptionFrame) -> ! {
    fault_then_reset("HardFault", Some(frame))
}

#[exception]
unsafe fn MemoryManagement() -> ! {
    fault_then_reset("MemoryManagement (MemManage)", None)
}

#[exception]
unsafe fn BusFault() -> ! {
    fault_then_reset("BusFault", None)
}

#[exception]
unsafe fn UsageFault() -> ! {
    fault_then_reset("UsageFault", None)
}

/// A stray tick is harmless, so not a fault.
#[exception]
fn SysTick() {}

#[exception]
unsafe fn NonMaskableInt() {
    if !flash::nmi_caught() {
        fault_then_reset("NonMaskableInt (NMI)", None)
    }
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) -> ! {
    let uart = Uart::direct();
    for _ in 0..FAULT_REPORTS {
        Uart::retry();
        uart.write_line("");
        uart.write_str("### unexpected vector, IRQn = ");
        if irqn < 0 {
            uart.write_str("-");
        }
        uart.write_u32(u32::from(irqn.unsigned_abs()));
        uart.write_line("");
        report_fault_registers(uart, "DefaultHandler");
        uart.flush();
        crude_delay();
    }
    watchdog::terminal_reset()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if let Some(loc) = info.location() { crash::panic(loc.file(), loc.line()); }
    let uart = Uart::direct();
    for _ in 0..FAULT_REPORTS {
        Uart::retry();
        uart.write_line("");
        uart.write_line("### PANIC");
        if let Some(location) = info.location() {
            uart.write_str("at ");
            uart.write_str(location.file());
            uart.write_str(":");
            uart.write_u32(location.line());
            uart.write_line("");
        }
        uart.flush();
        crude_delay();
    }
    watchdog::terminal_reset()
}
