//! Register addresses (RM0456 Rev 7 Table 6, non-secure aliases) and volatile
//! access to them.

#![allow(clippy::inline_always)]

/// # Safety
/// `addr` must be a word-aligned address of a readable 32-bit register.
#[inline(always)]
pub unsafe fn read32(addr: u32) -> u32 {
    (addr as *const u32).read_volatile()
}

/// # Safety
/// `addr` must be a word-aligned address of a writable 32-bit register.
#[inline(always)]
pub unsafe fn write32(addr: u32, value: u32) {
    (addr as *mut u32).write_volatile(value);
}

/// # Safety
/// `addr` must be a word-aligned address of a readable and writable register.
#[inline(always)]
pub unsafe fn modify32(addr: u32, clear: u32, set: u32) {
    let value = read32(addr);
    write32(addr, (value & !clear) | set);
}

/// The read-back gives a newly clocked peripheral the cycles it needs before
/// its registers take writes.
///
/// # Safety
/// `addr` must be an RCC clock-enable register.
#[inline(always)]
pub unsafe fn enable_clock(addr: u32, bits: u32) {
    modify32(addr, 0, bits);
    let _ = read32(addr);
}

// --- RCC (RM0456 11.8) -------------------------------------------------------

pub const RCC_BASE: u32 = 0x4602_0C00;

pub const RCC_CR: u32 = RCC_BASE;
pub const RCC_ICSCR1: u32 = RCC_BASE + 0x008;
pub const RCC_ICSCR2: u32 = RCC_BASE + 0x00C;
pub const RCC_ICSCR3: u32 = RCC_BASE + 0x010;
pub const RCC_CFGR1: u32 = RCC_BASE + 0x01C;
pub const RCC_CFGR2: u32 = RCC_BASE + 0x020;
pub const RCC_PLL1CFGR: u32 = RCC_BASE + 0x028;
pub const RCC_AHB1ENR: u32 = RCC_BASE + 0x088;
pub const RCC_AHB2ENR1: u32 = RCC_BASE + 0x08C;
pub const RCC_AHB2ENR2: u32 = RCC_BASE + 0x090;
pub const RCC_AHB3ENR: u32 = RCC_BASE + 0x094;
pub const RCC_APB1ENR1: u32 = RCC_BASE + 0x09C;
pub const RCC_APB1ENR2: u32 = RCC_BASE + 0x0A0;
pub const RCC_APB2ENR: u32 = RCC_BASE + 0x0A4;
pub const RCC_APB3ENR: u32 = RCC_BASE + 0x0A8;
pub const RCC_APB3SMENR: u32 = RCC_BASE + 0x0D0;
pub const RCC_SRDAMR: u32 = RCC_BASE + 0x0D8;
pub const RCC_CCIPR1: u32 = RCC_BASE + 0x0E0;
pub const RCC_BDCR: u32 = RCC_BASE + 0x0F0;
pub const RCC_CSR: u32 = RCC_BASE + 0x0F4;

pub const RCC_AHB2ENR1_GPIOAEN: u32 = 1 << 0;
pub const RCC_AHB2ENR1_GPIOBEN: u32 = 1 << 1;
pub const RCC_AHB2ENR1_GPIODEN: u32 = 1 << 3;
pub const RCC_AHB2ENR1_GPIOEEN: u32 = 1 << 4;
pub const RCC_AHB2ENR1_GPIOCEN: u32 = 1 << 2;
pub const RCC_AHB2ENR1_GPIOFEN: u32 = 1 << 5;
pub const RCC_AHB2ENR1_GPIOGEN: u32 = 1 << 6;
pub const RCC_AHB2ENR1_SDMMC1EN: u32 = 1 << 27;
pub const RCC_APB1ENR1_USART3EN: u32 = 1 << 18;
pub const RCC_APB3ENR_RTCAPBEN: u32 = 1 << 21;
/// The same bit in `APB3ENR`, `APB3SMENR` and `SRDAMR`.
pub const RCC_APB3_LPTIM1: u32 = 1 << 11;
pub const RCC_APB3_LPTIM3: u32 = 1 << 12;
pub const RCC_CCIPR1_USART3SEL: u32 = 0b11 << 4;
pub const RCC_BDCR_LSIRDY: u32 = 1 << 27;
pub const RCC_CSR_RMVF: u32 = 1 << 23;
pub const RCC_AHB3ENR_PWREN: u32 = 1 << 2;

pub const RCC_ICSCR1_MSIRGSEL: u32 = 1 << 23;
pub const RCC_ICSCR1_MSISRANGE_MASK: u32 = 0xF << 28;
pub const RCC_ICSCR1_MSISRANGE_4: u32 = 4 << 28;
pub const RCC_CR_MSISON: u32 = 1 << 0;
pub const RCC_CR_MSISRDY: u32 = 1 << 2;
pub const RCC_CFGR1_SW_MASK: u32 = 0b11;
pub const RCC_CFGR1_SWS_MASK: u32 = 0b11 << 2;

// --- GPIO (RM0456 13.4) ------------------------------------------------------

pub const GPIOA_BASE: u32 = 0x4202_0000;
pub const GPIOB_BASE: u32 = 0x4202_0400;
pub const GPIOC_BASE: u32 = 0x4202_0800;
pub const GPIOD_BASE: u32 = 0x4202_0C00;
pub const GPIOE_BASE: u32 = 0x4202_1000;
pub const GPIOF_BASE: u32 = 0x4202_1400;
pub const GPIOG_BASE: u32 = 0x4202_1800;

pub const GPIO_MODER: u32 = 0x00;
pub const GPIO_OTYPER: u32 = 0x04;
pub const GPIO_OSPEEDR: u32 = 0x08;
pub const GPIO_PUPDR: u32 = 0x0C;
pub const GPIO_IDR: u32 = 0x10;
pub const GPIO_BSRR: u32 = 0x18;
pub const GPIO_AFRL: u32 = 0x20;
pub const GPIO_AFRH: u32 = 0x24;

// --- LPTIM (RM0456 58) -------------------------------------------------------

pub const LPTIM1_BASE: u32 = 0x4600_4400;
pub const LPTIM3_BASE: u32 = 0x4600_4800;
pub const LPTIM_ISR: u32 = 0x00;
pub const LPTIM_ICR: u32 = 0x04;
pub const LPTIM_CFGR: u32 = 0x0C;
pub const LPTIM_CR: u32 = 0x10;
pub const LPTIM_CCR1: u32 = 0x14;
pub const LPTIM_ARR: u32 = 0x18;
pub const LPTIM_CNT: u32 = 0x1C;
pub const LPTIM_CCMR1: u32 = 0x2C;
pub const LPTIM_CR_ENABLE: u32 = 1 << 0;
pub const LPTIM_CR_CNTSTRT: u32 = 1 << 2;
pub const LPTIM_CR_COUNTRST: u32 = 1 << 3;
/// The same bits in `ISR` and `ICR`.
pub const LPTIM_CMP1OK: u32 = 1 << 3;
pub const LPTIM_ARROK: u32 = 1 << 4;
pub const LPTIM_CCMR1_CC1E: u32 = 1 << 1;
pub const LPTIM_CCMR1_CC1P_INVERTED: u32 = 0b01 << 2;

// --- PWR (RM0456 10.10) ------------------------------------------------------

pub const PWR_CR1: u32 = 0x4602_0800;
pub const PWR_CR1_ULPMEN: u32 = 1 << 7;
pub const PWR_VOSR: u32 = 0x4602_0800 + 0x0C;
pub const PWR_SVMCR: u32 = 0x4602_0800 + 0x10;
pub const PWR_SVMSR: u32 = 0x4602_0800 + 0x3C;
pub const PWR_VOSR_USBBOOSTRDY: u32 = 1 << 13;
pub const PWR_VOSR_USBPWREN: u32 = 1 << 19;
pub const PWR_VOSR_USBBOOSTEN: u32 = 1 << 20;
pub const PWR_SVMCR_UVMEN: u32 = 1 << 24;
pub const PWR_SVMCR_USV: u32 = 1 << 28;
pub const PWR_SVMSR_VDDUSBRDY: u32 = 1 << 24;

pub const PWR_CR2: u32 = 0x4602_0800 + 0x04;
pub const PWR_CR4: u32 = 0x4602_0800 + 0xA8;
pub const PWR_CR2_SRAM2: u32 = 0b11 << 4;
pub const PWR_CR2_SRAM4: u32 = 1 << 6;
pub const PWR_CR2_DC1RAM: u32 = 1 << 9;
pub const PWR_CR2_DMA2DRAM: u32 = 1 << 10;
pub const PWR_CR2_PKARAM: u32 = 1 << 12;
pub const PWR_CR2_SRAM3_1_TO_8: u32 = 0xFF << 16;
pub const PWR_CR4_SRAM3_9_TO_13: u32 = 0b1_1111 << 10;
pub const PWR_CR4_SRAM5_1_TO_12: u32 = 0xFFF << 16;
pub const SRAM1_BASE: u32 = 0x2000_0000;
pub const SRAM_PAGE: u32 = 64 * 1024;

// --- ICACHE (RM0456 8.7) -----------------------------------------------------

pub const ICACHE_CR: u32 = 0x4003_0400;
pub const ICACHE_CR_EN: u32 = 1 << 0;
pub const ICACHE_CR_CACHEINV: u32 = 1 << 1;
pub const ICACHE_SR: u32 = 0x4003_0404;
pub const ICACHE_SR_BUSYF: u32 = 1 << 0;

// --- SDMMC1 ------------------------------------------------------------------

pub const SDMMC1_BASE: u32 = 0x420C_8000;
pub const SDMMC_POWER: u32 = 0x00;

// --- RTC ---------------------------------------------------------------------

pub const RTC_BASE: u32 = 0x4600_7800;

// --- USART3 (RM0456 66.8) ----------------------------------------------------

pub const USART3_BASE: u32 = 0x4000_4800;

pub const USART_CR1: u32 = 0x00;
pub const USART_CR2: u32 = 0x04;
pub const USART_CR3: u32 = 0x08;
pub const USART_BRR: u32 = 0x0C;
pub const USART_ISR: u32 = 0x1C;
pub const USART_TDR: u32 = 0x28;
pub const USART_PRESC: u32 = 0x2C;

pub const USART_CR1_UE: u32 = 1 << 0;
pub const USART_CR1_TE: u32 = 1 << 3;
pub const USART_CR1_TXFNFIE: u32 = 1 << 7;
/// RM0456 Table 164.
pub const USART3_IRQN: u32 = 63;
pub const USART_ISR_TXFNF: u32 = 1 << 7;
pub const USART_ISR_TC: u32 = 1 << 6;

// --- Cortex-M33 system control block (Armv8-M ARM) ---------------------------

pub const SCB_VTOR: u32 = 0xE000_ED08;
pub const SCB_SHCSR: u32 = 0xE000_ED24;
pub const SCB_CFSR: u32 = 0xE000_ED28;
pub const SCB_HFSR: u32 = 0xE000_ED2C;
pub const SCB_MMFAR: u32 = 0xE000_ED34;
pub const SCB_BFAR: u32 = 0xE000_ED38;

pub const SHCSR_MEMFAULTENA: u32 = 1 << 16;
pub const SHCSR_BUSFAULTENA: u32 = 1 << 17;
pub const SHCSR_USGFAULTENA: u32 = 1 << 18;

pub const SCB_AIRCR: u32 = 0xE000_ED0C;
pub const AIRCR_SYSRESETREQ: u32 = (0x5FA << 16) | (1 << 2);

pub const SYST_CSR: u32 = 0xE000_E010;

pub const SCB_ICSR: u32 = 0xE000_ED04;
pub const ICSR_PENDSTCLR: u32 = 1 << 25;
pub const ICSR_PENDSVCLR: u32 = 1 << 27;

pub const NVIC_ISER0: u32 = 0xE000_E100;
pub const NVIC_ICER0: u32 = 0xE000_E180;
pub const NVIC_ICPR0: u32 = 0xE000_E280;
pub const NVIC_REGS: u32 = 8;

pub const DBGMCU_IDCODE: u32 = 0xE004_4000;

// --- IWDG (RM0456 61.7) ------------------------------------------------------

pub const IWDG_BASE: u32 = 0x4000_3000;

pub const IWDG_KR: u32 = IWDG_BASE;
pub const IWDG_PR: u32 = IWDG_BASE + 0x04;
pub const IWDG_RLR: u32 = IWDG_BASE + 0x08;
pub const IWDG_SR: u32 = IWDG_BASE + 0x0C;

pub const IWDG_KEY_REFRESH: u32 = 0xAAAA;
pub const IWDG_KEY_UNLOCK: u32 = 0x5555;
pub const IWDG_KEY_START: u32 = 0xCCCC;

pub const IWDG_PR_DIV256: u32 = 0b0110;
pub const IWDG_RLR_MAX: u32 = 0xFFF;
pub const IWDG_SR_UPDATING: u32 = (1 << 0) | (1 << 1);

// --- FLASH (RM0456 7.9) ------------------------------------------------------

pub const FLASH_BASE: u32 = 0x4002_2000;

pub const FLASH_ACR: u32 = FLASH_BASE;
pub const FLASH_OPTR: u32 = FLASH_BASE + 0x40;
