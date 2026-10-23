//! The IWDG (RM0456 61) and the deliberate reset.

use crate::mmio::{
    read32, write32, AIRCR_SYSRESETREQ, IWDG_KEY_REFRESH, IWDG_KEY_START, IWDG_KEY_UNLOCK, IWDG_KR,
    IWDG_PR, IWDG_PR_DIV256, IWDG_RLR, IWDG_RLR_MAX, IWDG_SR, IWDG_SR_UPDATING, RCC_BDCR,
    RCC_BDCR_LSIRDY, SCB_AIRCR,
};

const POLL_BUDGET: u32 = 200_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Armed,
    ArmedDefaultTimeout,
    NoClock,
}

impl Started {
    pub fn as_str(self) -> &'static str {
        match self {
            Started::Armed => "armed, ~32s",
            Started::ArmedDefaultTimeout => "ARMED BUT DEFAULT TIMEOUT (SR never cleared)",
            Started::NoClock => "NOT RUNNING -- LSI never came ready",
        }
    }
}

fn poll_clear(addr: u32, mask: u32) -> bool {
    for _ in 0..POLL_BUDGET {
        if unsafe { read32(addr) } & mask == 0 {
            return true;
        }
    }
    false
}

fn poll_set(addr: u32, mask: u32) -> bool {
    for _ in 0..POLL_BUDGET {
        if unsafe { read32(addr) } & mask != 0 {
            return true;
        }
    }
    false
}

/// Starts the IWDG before widening its timeout, because the start key turns the LSI on (RM0456 61).
///
/// # Safety
/// The watchdog cannot be stopped without a reset; call once.
pub unsafe fn start() -> Started {
    write32(IWDG_KR, IWDG_KEY_START);

    // The LSI check below is a report, not a gate: widening never depends on it.
    write32(IWDG_KR, IWDG_KEY_UNLOCK);
    let mut widened = poll_clear(IWDG_SR, IWDG_SR_UPDATING);
    write32(IWDG_PR, IWDG_PR_DIV256);
    write32(IWDG_RLR, IWDG_RLR_MAX);
    widened &= poll_clear(IWDG_SR, IWDG_SR_UPDATING);
    write32(IWDG_KR, IWDG_KEY_REFRESH);

    if !poll_set(RCC_BDCR, RCC_BDCR_LSIRDY) {
        return Started::NoClock;
    }
    if !widened {
        return Started::ArmedDefaultTimeout;
    }
    Started::Armed
}

pub fn refresh() {
    unsafe { write32(IWDG_KR, IWDG_KEY_REFRESH) };
}

pub fn terminal_reset() -> ! {
    loop {
        unsafe {
            write32(SCB_AIRCR, AIRCR_SYSRESETREQ);
            core::arch::asm!("dsb sy", "isb sy", options(nomem, nostack));
        }
        crate::crude_delay();
    }
}
