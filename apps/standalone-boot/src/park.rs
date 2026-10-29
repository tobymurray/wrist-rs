//! Everything the image does not use, turned off at boot (`HARDWARE.md`, "What the image turns off").

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_stm32::gpio::{Level, Output, Pin, Speed};
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Blocking;
use embassy_stm32::{peripherals, Peri};
use wrist_board::{pins as net, Pin as Net, Port};

use crate::mmio::{
    enable_clock, modify32, read32, write32, GPIOA_BASE, GPIOB_BASE, GPIOC_BASE, GPIOD_BASE,
    GPIOE_BASE, GPIOF_BASE, GPIOG_BASE, GPIO_MODER, GPIO_PUPDR, PWR_CR2, PWR_CR2_DC1RAM,
    PWR_CR2_DMA2DRAM, PWR_CR2_PKARAM, PWR_CR2_SRAM2, PWR_CR2_SRAM3_1_TO_8, PWR_CR2_SRAM4, PWR_CR3,
    PWR_CR3_REGSEL, PWR_CR4, PWR_CR4_SRAM3_9_TO_13, PWR_CR4_SRAM5_1_TO_12, PWR_SVMSR,
    PWR_SVMSR_REGS, RCC_AHB2ENR1, RCC_AHB2ENR1_GPIOAEN, RCC_AHB2ENR1_GPIOBEN, RCC_AHB2ENR1_GPIOCEN,
    RCC_AHB2ENR1_GPIODEN, RCC_AHB2ENR1_GPIOFEN, RCC_AHB2ENR1_GPIOGEN, RCC_AHB2ENR1_SDMMC1EN,
    SDMMC1_BASE, SDMMC_POWER, SRAM1_BASE, SRAM_PAGE,
};

extern "C" {
    static __euninit: u8;
}
use crate::uart::Uart;

/// The lines held low.
pub struct Pins {
    pub emmc_en: Peri<'static, peripherals::PD10>,
    pub gnss_en: Peri<'static, peripherals::PG5>,
    pub gnss_wake: Peri<'static, peripherals::PD1>,
    /// With the `ble` feature, `ble.rs` holds it instead.
    #[cfg(not(feature = "ble"))]
    pub ble_rst: Peri<'static, peripherals::PA8>,
    pub body_rst: Peri<'static, peripherals::PF13>,
    pub body_cs: Peri<'static, peripherals::PG12>,
    pub altimeter_en: Peri<'static, peripherals::PG0>,
    pub haptic_rst: Peri<'static, peripherals::PF2>,
    pub buzzer_en1: Peri<'static, peripherals::PB9>,
    pub buzzer_en2: Peri<'static, peripherals::PD5>,
}

const ANALOG: [Net; 35] = [
    net::SDIO_D0, net::SDIO_D1, net::SDIO_D2, net::SDIO_D3, net::SDIO_CK, net::SDIO_CMD,
    net::SENSOR1_INT2, net::SENSOR1_TX_INT0, net::SENSOR1_RX_INT1, net::SENSOR1_CS,
    net::SENSOR1_MOSI, net::SENSOR1_SCK, net::SENSOR1_MISO, net::SENSOR1_SCL, net::SENSOR1_SDA,
    net::SENSOR2_INT2, net::SENSOR2_TX_INT0, net::SENSOR2_RX_INT1, net::SENSOR2_CS,
    net::SENSOR2_MISO, net::SENSOR2_MOSI,
    net::SENSORBODY_INT0, net::SENSORBODY_INT1, net::SENSORBODY_INT2, net::SENSORBODY_INT3,
    net::SENSORBODY_SCL, net::SENSORBODY_SDA, net::SENSORBODY_MOSI, net::SENSORBODY_SCK,
    net::SENSORBODY_MISO,
    net::BUZZER_PWM, net::TOUCH_INT_L, net::TOUCH_RST_L,
    net::FUEL_GAUGE_ALRT, net::USB_FAULT_L,
];

const SENSORS: [(u8, &str); 2] = [(0x68, "IMU"), (0x14, "compass")];
const REG_CMD: u8 = 0x7E;
const CMD_SOFT_RESET: u8 = 0xB6;

const SMPS_POLLS: u32 = 10_000;

const SENSOR_LEFT_ALONE: u8 = 0;
const SENSOR_RESET: u8 = 1;
const SENSOR_SILENT: u8 = 2;

static SDMMC_WAS_ON: AtomicBool = AtomicBool::new(false);
static SENSOR_OUTCOME: [AtomicU8; 2] = [AtomicU8::new(SENSOR_LEFT_ALONE), AtomicU8::new(SENSOR_LEFT_ALONE)];

/// `sensor_bus` is `None` when the GNSS slot holds another module.
pub fn park(pins: Pins, sensor_bus: Option<&mut I2c<'static, Blocking, Master>>, uart: Uart) {
    SDMMC_WAS_ON.store(unsafe { sdmmc_off() }, Ordering::Relaxed);
    unsafe {
        enable_clock(
            RCC_AHB2ENR1,
            RCC_AHB2ENR1_GPIOAEN
                | RCC_AHB2ENR1_GPIOBEN
                | RCC_AHB2ENR1_GPIOCEN
                | RCC_AHB2ENR1_GPIODEN
                | RCC_AHB2ENR1_GPIOFEN
                | RCC_AHB2ENR1_GPIOGEN,
        );
        for line in ANALOG {
            let (base, pin) = (gpio_base(line.port), u32::from(line.number));
            modify32(base + GPIO_PUPDR, 0b11 << (pin * 2), 0);
            modify32(base + GPIO_MODER, 0, 0b11 << (pin * 2));
        }
    }
    hold_low(pins.emmc_en);
    hold_low(pins.gnss_en);
    hold_low(pins.gnss_wake);
    #[cfg(not(feature = "ble"))]
    hold_low(pins.ble_rst);
    hold_low(pins.body_rst);
    hold_low(pins.body_cs);
    hold_low(pins.altimeter_en);
    hold_low(pins.haptic_rst);
    hold_low(pins.buzzer_en1);
    hold_low(pins.buzzer_en2);
    crate::rtc::disarm_alarm_a();
    if unsafe { select_smps() } {
        unsafe { sram_off() };
    }
    if let Some(bus) = sensor_bus {
        for ((addr, _), outcome) in SENSORS.into_iter().zip(&SENSOR_OUTCOME) {
            let reset = bus.blocking_write(addr, &[REG_CMD, CMD_SOFT_RESET]).is_ok();
            outcome.store(if reset { SENSOR_RESET } else { SENSOR_SILENT }, Ordering::Relaxed);
        }
    }
    uart.write_line("[13] parked:");
    write_report(uart);
}

pub fn write_report(uart: Uart) {
    #[cfg(not(feature = "ble"))]
    uart.write_str("     off: eMMC, GNSS, Bluetooth, heart rate, altimeter, haptics, buzzer; SDMMC1 ");
    #[cfg(feature = "ble")]
    uart.write_str("     off: eMMC, GNSS, Bluetooth until `ble on`, heart rate, altimeter, haptics, buzzer; SDMMC1 ");
    uart.write_line(if SDMMC_WAS_ON.load(Ordering::Relaxed) { "was on, now off" } else { "off" });
    uart.write_str("     ");
    uart.write_u32(u32::try_from(ANALOG.len()).unwrap_or(0));
    uart.write_line(" unused lines analog; RTC alarm A off");
    uart.write_line(if on_smps() {
        "     off in Stop: SRAM1 above .bss, SRAM2, SRAM3 outside an update, SRAM4, SRAM5 below its top 64 KB, DCACHE1/DMA2D/PKA RAM"
    } else {
        "     SMPS NOT selected: every SRAM kept on in Stop (ES0553 2.2.10)"
    });
    uart.write_reg("     PWR_CR2 = ", unsafe { read32(PWR_CR2) });
    uart.write_reg("     PWR_CR4 = ", unsafe { read32(PWR_CR4) });
    for ((_, name), outcome) in SENSORS.into_iter().zip(&SENSOR_OUTCOME) {
        uart.write_str("     ");
        uart.write_str(name);
        uart.write_line(match outcome.load(Ordering::Relaxed) {
            SENSOR_RESET => " reset to suspend",
            SENSOR_SILENT => " did not answer; left alone",
            _ => " left alone: the GNSS slot holds another module",
        });
    }
}

/// SRAM may be powered down in Stop 2 only on the SMPS (ES0553 2.2.10).
pub fn on_smps() -> bool {
    unsafe { read32(PWR_SVMSR) & PWR_SVMSR_REGS != 0 }
}

unsafe fn select_smps() -> bool {
    modify32(PWR_CR3, 0, PWR_CR3_REGSEL);
    (0..SMPS_POLLS).any(|_| on_smps())
}

/// Keeps only SRAM1's pages up to the end of `.bss` and SRAM5's top page (the stack, the
/// loader's mailbox and the failure record); `update` keeps its staging pages for a transfer.
///
/// # Safety
/// Nothing may live in the pages it powers down.
unsafe fn sram_off() {
    let used = (core::ptr::addr_of!(__euninit) as u32 - SRAM1_BASE).div_ceil(SRAM_PAGE);
    let sram1 = 0xFFF & !((1u32 << used) - 1);
    modify32(
        PWR_CR2,
        0,
        (sram1 & 0b111)
            | PWR_CR2_SRAM2
            | PWR_CR2_SRAM3_1_TO_8
            | PWR_CR2_SRAM4
            | PWR_CR2_DC1RAM
            | PWR_CR2_DMA2DRAM
            | PWR_CR2_PKARAM,
    );
    modify32(PWR_CR4, 0, (sram1 >> 3) | PWR_CR4_SRAM3_9_TO_13 | PWR_CR4_SRAM5_1_TO_12);
}

/// True if SDMMC1 was clocked, the only case in which its registers are written.
///
/// # Safety
/// Nothing else may own SDMMC1.
unsafe fn sdmmc_off() -> bool {
    if read32(RCC_AHB2ENR1) & RCC_AHB2ENR1_SDMMC1EN == 0 {
        return false;
    }
    write32(SDMMC1_BASE + SDMMC_POWER, 0);
    modify32(RCC_AHB2ENR1, RCC_AHB2ENR1_SDMMC1EN, 0);
    true
}

fn gpio_base(port: Port) -> u32 {
    match port {
        Port::A => GPIOA_BASE,
        Port::B => GPIOB_BASE,
        Port::C => GPIOC_BASE,
        Port::D => GPIOD_BASE,
        Port::E => GPIOE_BASE,
        Port::F => GPIOF_BASE,
        Port::G => GPIOG_BASE,
    }
}

fn hold_low(pin: Peri<'static, impl Pin>) {
    core::mem::forget(Output::new(pin, Level::Low, Speed::Low));
}
