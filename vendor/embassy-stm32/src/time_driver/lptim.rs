#![allow(non_snake_case)]

#[cfg(feature = "low-power")]
use core::cell::Cell;
use core::cell::RefCell;
#[cfg(feature = "low-power")]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU32, Ordering, compiler_fence};

use critical_section::CriticalSection;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time_driver::{Driver, TICK_HZ};
use embassy_time_queue_utils::Queue;
#[cfg(not(time_driver_lptim4))]
use stm32_metapac::lptim::Lptim;

use super::AlarmState;
use crate::interrupt::typelevel::Interrupt;
use crate::lptim::vals;
#[cfg(not(time_driver_lptim4))]
use crate::lptim::SealedInstance;
#[cfg(not(time_driver_lptim4))]
use crate::rcc::SealedRccPeripheral;
use crate::{peripherals, rcc};

#[cfg(time_driver_lptim1)]
type T = peripherals::LPTIM1;
#[cfg(time_driver_lptim2)]
type T = peripherals::LPTIM2;
#[cfg(time_driver_lptim3)]
type T = peripherals::LPTIM3;
#[cfg(time_driver_lptim4)]
type T = peripherals::LPTIM4;

#[cfg(not(time_driver_lptim4))]
type Regs = Lptim;
// wrist-rs: LPTIM4 has the basic register block, which `SealedInstance::regs` does not cover.
#[cfg(time_driver_lptim4)]
type Regs = stm32_metapac::lptim::LptimBasic;

fn regs_lptim() -> Regs {
    #[cfg(not(time_driver_lptim4))]
    {
        T::regs()
    }
    #[cfg(time_driver_lptim4)]
    {
        crate::pac::LPTIM4
    }
}

// LPTIM v2a/v2b (STM32WBA, STM32U5, STM32U3, etc.) renames the interrupt
// enable register from IER to DIER and uses IcrAdv/DierAdv register types
// instead of Icr/Ier. The compare register also changes from a single `cmp()`
// to an indexed `ccr(n)`.

/// Read the interrupt enable register value as a raw u32.
fn ier_read(r: Regs) -> u32 {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    {
        r.ier().read().0
    }
    #[cfg(any(stm32wba, stm32u5, stm32u3))]
    {
        r.dier().read().0
    }
}

/// Check if the compare-capture interrupt is enabled for channel `n`.
fn ier_ccie(r: Regs, n: usize) -> bool {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    {
        r.ier().read().ccie(n)
    }
    #[cfg(any(stm32wba, stm32u5, stm32u3))]
    {
        r.dier().read().ccie(n)
    }
}

/// Modify the interrupt enable register.
fn ier_set_ueie(r: Regs, val: bool) {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    r.ier().modify(|w| w.set_ueie(val));
    #[cfg(any(stm32wba, stm32u5, stm32u3))]
    {
        let mut w = r.dier().read();
        w.set_ueie(val);
        dier_write(r, w.0);
    }
}

fn ier_set_ccie(r: Regs, n: usize, val: bool) {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    r.ier().modify(|w| w.set_ccie(n, val));
    #[cfg(any(stm32wba, stm32u5, stm32u3))]
    {
        let mut w = r.dier().read();
        w.set_ccie(n, val);
        dier_write(r, w.0);
    }
}

/// wrist-rs: the one way DIER changes. A write is waited out to its `DIEROK`, as RM0456
/// 58.7.7 asks before the next; one still unacknowledged at Stop 2 entry loses its compare wakeup.
/// A write that changes nothing is skipped: changing an enable clears its flag (ES0553 2.12.3).
#[cfg(any(stm32wba, stm32u5, stm32u3))]
fn dier_write(r: Regs, val: u32) {
    let old = ier_read(r);
    if old == val {
        stats::skipped();
        return;
    }
    r.icr().write(|w| w.set_dierokcf(true));
    r.dier().write(|w| w.0 = val);
    stats::written(old, val);
    let from = stats::cycles();
    for _ in 0..20_000 {
        if r.isr().read().dierok() {
            stats::dierok_wait(from);
            stats::acknowledged(ier_read(r));
            return;
        }
    }
    panic!("LPTIM interrupt enable write acknowledgement timed out");
}

/// wrist-rs: what the driver saw, for diagnosing late wakes; see WRIST-PATCHES.md.
/// The console's `power` report names each field.
#[cfg(any(stm32wba, stm32u5, stm32u3))]
#[allow(missing_docs)]
pub mod stats {
    use core::cell::RefCell;
    use core::sync::atomic::Ordering::Relaxed;
    use core::sync::atomic::{AtomicBool, AtomicU32};

    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

    /// Armed with `CCIE` clear, for `next_period` to enable.
    pub const FAR: u16 = 1 << 0;
    /// Its `CCIE` write came before the previous `DIER` write's `DIEROK`.
    pub const EARLY: u16 = 1 << 1;
    /// Stop was entered before its `CCIE` write's `DIEROK`.
    pub const STOP_PENDING: u16 = 1 << 2;
    /// Its `CMPOK` was already set at the first poll.
    pub const CMPOK_FIRST_POLL: u16 = 1 << 3;
    /// Found in an overflow interrupt.
    pub const AT_OVERFLOW: u16 = 1 << 4;
    /// That interrupt read `CCIE` clear before `next_period`.
    pub const CCIE_CLEAR: u16 = 1 << 5;
    /// `CCR` did not hold the alarm's low 16 bits.
    pub const CCR_WRONG: u16 = 1 << 6;
    /// Found overdue by another timer's `schedule_wake`.
    pub const BY_SCHEDULE: u16 = 1 << 7;
    /// Its arming turned `CCIE` on from off, which clears `CC1IF` (ES0553 2.12.3).
    pub const CCIE_ON: u16 = 1 << 8;

    /// An alarm handled more than 5 ms after its time.
    #[derive(Clone, Copy, Default)]
    pub struct Late {
        pub at_ticks: u32,
        pub late_ticks: u32,
        pub alarm_lo: u16,
        pub flags: u16,
    }

    /// Cycles are the core's, at the clock in force.
    #[derive(Clone, Copy, Default)]
    pub struct Stats {
        pub dier_writes: u32,
        pub dier_skipped: u32,
        pub dier_early: u32,
        pub dier_mismatch: u32,
        pub stop_dier_pending: u32,
        pub overdue_at_overflow: u32,
        pub overdue_ccie_clear: u32,
        pub cmpok_waits: u32,
        pub cmpok_first_poll: u32,
        pub cmpok_cycles: u32,
        pub cmpok_max_cycles: u32,
        pub dierok_waits: u32,
        pub dierok_cycles: u32,
        pub dierok_max_cycles: u32,
        pub late_alarms: u32,
        pub late_max_ticks: u32,
        /// Oldest first.
        pub late: [Late; 8],
    }

    const LATE_TICKS: u64 = 164;
    const NO_LATE: Late = Late { at_ticks: 0, late_ticks: 0, alarm_lo: 0, flags: 0 };

    static DIER_WRITES: AtomicU32 = AtomicU32::new(0);
    static DIER_SKIPPED: AtomicU32 = AtomicU32::new(0);
    static DIER_EARLY: AtomicU32 = AtomicU32::new(0);
    static DIER_MISMATCH: AtomicU32 = AtomicU32::new(0);
    static STOP_DIER_PENDING: AtomicU32 = AtomicU32::new(0);
    static OVERDUE_AT_OVERFLOW: AtomicU32 = AtomicU32::new(0);
    static OVERDUE_CCIE_CLEAR: AtomicU32 = AtomicU32::new(0);
    static CMPOK_WAITS: AtomicU32 = AtomicU32::new(0);
    static CMPOK_FIRST: AtomicU32 = AtomicU32::new(0);
    static CMPOK_CYCLES: AtomicU32 = AtomicU32::new(0);
    static CMPOK_MAX: AtomicU32 = AtomicU32::new(0);
    static DIEROK_WAITS: AtomicU32 = AtomicU32::new(0);
    static DIEROK_CYCLES: AtomicU32 = AtomicU32::new(0);
    static DIEROK_MAX: AtomicU32 = AtomicU32::new(0);
    static LATE_ALARMS: AtomicU32 = AtomicU32::new(0);
    static LATE_MAX: AtomicU32 = AtomicU32::new(0);
    static LATE: Mutex<CriticalSectionRawMutex, RefCell<[Late; 8]>> = Mutex::new(RefCell::new([NO_LATE; 8]));

    /// A `DIER` write not yet acknowledged, and the value it wrote.
    static PENDING: AtomicBool = AtomicBool::new(false);
    static WRITTEN: AtomicU32 = AtomicU32::new(0);
    static LAST_EARLY: AtomicBool = AtomicBool::new(false);
    static LAST_CCIE_ON: AtomicBool = AtomicBool::new(false);
    static LAST_CMPOK_FIRST: AtomicBool = AtomicBool::new(false);
    /// The armed alarm's flags.
    static ALARM_FLAGS: AtomicU32 = AtomicU32::new(0);

    pub fn snapshot() -> Stats {
        Stats {
            dier_writes: DIER_WRITES.load(Relaxed),
            dier_skipped: DIER_SKIPPED.load(Relaxed),
            dier_early: DIER_EARLY.load(Relaxed),
            dier_mismatch: DIER_MISMATCH.load(Relaxed),
            stop_dier_pending: STOP_DIER_PENDING.load(Relaxed),
            overdue_at_overflow: OVERDUE_AT_OVERFLOW.load(Relaxed),
            overdue_ccie_clear: OVERDUE_CCIE_CLEAR.load(Relaxed),
            cmpok_waits: CMPOK_WAITS.load(Relaxed),
            cmpok_first_poll: CMPOK_FIRST.load(Relaxed),
            cmpok_cycles: CMPOK_CYCLES.load(Relaxed),
            cmpok_max_cycles: CMPOK_MAX.load(Relaxed),
            dierok_waits: DIEROK_WAITS.load(Relaxed),
            dierok_cycles: DIEROK_CYCLES.load(Relaxed),
            dierok_max_cycles: DIEROK_MAX.load(Relaxed),
            late_alarms: LATE_ALARMS.load(Relaxed),
            late_max_ticks: LATE_MAX.load(Relaxed),
            late: LATE.lock(|l| *l.borrow()),
        }
    }

    /// The core's cycle counter, enabled again if Stop cleared it.
    pub(super) fn cycles() -> u32 {
        use cortex_m::peripheral::{DCB, DWT};
        if !DWT::cycle_counter_enabled() {
            // SAFETY: sets only the trace enable and the counter enable.
            unsafe {
                (*DCB::PTR).demcr.modify(|w| w | 1 << 24);
                (*DWT::PTR).ctrl.modify(|w| w | 1);
            }
        }
        DWT::cycle_count()
    }

    fn add_cycles(sum: &AtomicU32, max: &AtomicU32, from: u32) {
        let took = cycles().wrapping_sub(from);
        sum.fetch_add(took, Relaxed);
        max.fetch_max(took, Relaxed);
    }

    pub(super) fn cmpok_wait(from: u32, first_poll: bool) {
        add_cycles(&CMPOK_CYCLES, &CMPOK_MAX, from);
        CMPOK_WAITS.fetch_add(1, Relaxed);
        if first_poll {
            CMPOK_FIRST.fetch_add(1, Relaxed);
        }
        LAST_CMPOK_FIRST.store(first_poll, Relaxed);
    }

    pub(super) fn dierok_wait(from: u32) {
        add_cycles(&DIEROK_CYCLES, &DIEROK_MAX, from);
        DIEROK_WAITS.fetch_add(1, Relaxed);
    }

    pub(super) fn skipped() {
        DIER_SKIPPED.fetch_add(1, Relaxed);
    }

    pub(super) fn written(old: u32, val: u32) {
        DIER_WRITES.fetch_add(1, Relaxed);
        let on = old & 1 == 0 && val & 1 != 0;
        LAST_CCIE_ON.store(on, Relaxed);
        if on {
            ALARM_FLAGS.fetch_or(u32::from(CCIE_ON), Relaxed);
        }
        WRITTEN.store(val, Relaxed);
        let early = PENDING.swap(true, Relaxed);
        if early {
            DIER_EARLY.fetch_add(1, Relaxed);
        }
        LAST_EARLY.store(early, Relaxed);
    }

    /// `DIEROK` seen: whether `DIER` reads back what was written.
    pub(super) fn acknowledged(read_back: u32) {
        if PENDING.swap(false, Relaxed) && read_back != WRITTEN.load(Relaxed) {
            DIER_MISMATCH.fetch_add(1, Relaxed);
        }
    }

    pub(super) fn pending() -> bool {
        PENDING.load(Relaxed)
    }

    pub(super) fn stop_with_pending() {
        STOP_DIER_PENDING.fetch_add(1, Relaxed);
        ALARM_FLAGS.fetch_or(u32::from(STOP_PENDING), Relaxed);
    }

    pub(super) fn armed(far: bool) {
        let mut f = if far { FAR } else { 0 };
        if LAST_EARLY.load(Relaxed) {
            f |= EARLY;
        }
        if LAST_CMPOK_FIRST.load(Relaxed) {
            f |= CMPOK_FIRST_POLL;
        }
        if LAST_CCIE_ON.load(Relaxed) {
            f |= CCIE_ON;
        }
        ALARM_FLAGS.store(u32::from(f), Relaxed);
    }

    pub(super) fn overdue_at_overflow(ccie_clear: bool) {
        OVERDUE_AT_OVERFLOW.fetch_add(1, Relaxed);
        if ccie_clear {
            OVERDUE_CCIE_CLEAR.fetch_add(1, Relaxed);
        }
    }

    /// The alarm due at `alarm` is being handled at `now`.
    #[allow(clippy::cast_possible_truncation)]
    pub(super) fn handled(alarm: u64, now: u64, ccr: u16, context: u16) {
        if alarm == u64::MAX || now < alarm + LATE_TICKS {
            return;
        }
        let late_ticks = (now - alarm).min(u64::from(u32::MAX)) as u32;
        let mut flags = ALARM_FLAGS.load(Relaxed) as u16 | context;
        if ccr != alarm as u16 {
            flags |= CCR_WRONG;
        }
        LATE_ALARMS.fetch_add(1, Relaxed);
        LATE_MAX.fetch_max(late_ticks, Relaxed);
        LATE.lock(|l| {
            let mut l = l.borrow_mut();
            l.copy_within(1.., 0);
            l[7] = Late { at_ticks: now as u32, late_ticks, alarm_lo: alarm as u16, flags };
        });
    }
}

/// Write a raw value to the ICR (interrupt clear register).
fn icr_write_raw(r: Regs, val: u32) {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    r.icr().write_value(stm32_metapac::lptim::regs::Icr(val));
    #[cfg(all(any(stm32wba, stm32u5, stm32u3), not(time_driver_lptim4)))]
    r.icr().write_value(stm32_metapac::lptim::regs::IcrAdv(val));
    #[cfg(time_driver_lptim4)]
    r.icr().write_value(stm32_metapac::lptim::regs::IcrBasic(val));
}

/// Write the compare value and wait for the write to be acknowledged.
fn write_compare(r: Regs, val: u16) {
    #[cfg(not(any(stm32wba, stm32u5, stm32u3)))]
    {
        // On STM32WL, CMPOK remains set after a compare update. Clear the stale
        // acknowledgement so the loop waits for the current CMP write.
        #[cfg(any(stm32wlex, stm32wl5x))]
        r.icr().write(|w| w.set_cmpokcf(0, true));

        r.cmp().write(|w| w.set_cmp(val));
        while !r.isr().read().cmpok(0) {}
    }
    #[cfg(any(stm32wba, stm32u5, stm32u3))]
    {
        // wrist-rs: a stale CMPOK does not acknowledge this write; see WRIST-PATCHES.md.
        r.icr().write(|w| w.set_cmpokcf(0, true));
        r.ccr(0).write(|w| w.set_ccr(val));
        let from = stats::cycles();
        for i in 0..20_000 {
            if r.isr().read().cmpok(0) {
                stats::cmpok_wait(from, i == 0);
                return;
            }
        }
        panic!("LPTIM compare write acknowledgement timed out");
    }
}

pub(crate) struct RtcDriver {
    /// Number of 2^15 periods elapsed since boot.
    period: AtomicU32,
    alarm: Mutex<CriticalSectionRawMutex, AlarmState>,
    #[cfg(feature = "low-power")]
    is_stopped: AtomicBool,
    #[cfg(feature = "low-power")]
    /// The minimum pause time beyond which the executor will enter a low-power state.
    min_stop_pause: Mutex<CriticalSectionRawMutex, Cell<embassy_time::Duration>>,
    queue: Mutex<CriticalSectionRawMutex, RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: RtcDriver = RtcDriver {
    period: AtomicU32::new(0),
    alarm: Mutex::const_new(CriticalSectionRawMutex::new(), AlarmState::new()),
    #[cfg(feature = "low-power")]
    is_stopped: AtomicBool::new(false),
    #[cfg(feature = "low-power")]
    min_stop_pause: Mutex::const_new(CriticalSectionRawMutex::new(), Cell::new(embassy_time::Duration::from_millis(0))),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

impl RtcDriver {
    /// initialize the timer, but don't start it.  Used for chips like stm32wle5
    /// for low power where the timer config is lost in STOP2.
    pub(crate) fn init_timer(&'static self, _cs: critical_section::CriticalSection) {
        let r = regs_lptim();

        // we want this to increment the stop mode counter (some lp timer can't do STOP2)
        rcc::enable_and_reset::<T>();

        #[cfg(not(time_driver_lptim4))]
        let timer_freq = T::frequency();
        // wrist-rs: the chip data does not model LPTIM34SEL, so LPTIM4 is put on the LSE here,
        // or on the LSI when the LSE is not running, its 32 kHz counted as 32768 Hz.
        #[cfg(time_driver_lptim4)]
        let timer_freq = {
            use crate::pac::rcc::vals::Lptimsel;
            let bdcr = crate::pac::RCC.bdcr().read();
            assert!(bdcr.lserdy() || bdcr.lsirdy(), "LPTIM4 time driver: neither the LSE nor the LSI is running");
            let sel = if bdcr.lserdy() { Lptimsel::Lse } else { Lptimsel::Lsi };
            crate::pac::RCC.ccipr3().modify(|w| w.set_lptim34sel(sel));
            crate::time::Hertz(32_768)
        };

        r.cnt().write(|w| w.set_cnt(0));

        // let psc = timer_freq.0 / TICK_HZ as u32 - 1;
        let psc = timer_freq.0 / TICK_HZ as u32;
        let psc = match psc {
            128 => vals::Presc::Div128,
            64 => vals::Presc::Div64,
            32 => vals::Presc::Div32,
            16 => vals::Presc::Div16,
            8 => vals::Presc::Div8,
            4 => vals::Presc::Div4,
            2 => vals::Presc::Div2,
            1 => vals::Presc::Div1,
            // TODO: we could compute the valid TICK_HZ for the valid prescalers to include in the panic message
            _ => panic!("Invalid prescaler: {} for timer frequency: {}Hz", psc, timer_freq.0),
        };

        trace!(
            "init: setting presc: {:?} timer_freq: {}Hz TICK_HZ: {}",
            psc, timer_freq, TICK_HZ
        );
        r.cfgr().modify(|w| w.set_presc(psc));

        // RM says timer must be enabled before setting arr or cmp
        r.cr().modify(|w| w.set_enable(true));
        trace!("init: arr: {:?}", r.arr().read());
        // TRM says this is updated immediately if the timer is not started so no need to check for arrok! (stm32wl5 & stm32wle)
        r.arr().write(|w| w.set_arr(u16::MAX));

        // Enable overflow interrupts
        ier_set_ueie(regs_lptim(), true);

        <T as crate::lptim::SealedBasicInstance>::GlobalInterrupt::unpend();
        unsafe {
            <T as crate::lptim::SealedBasicInstance>::GlobalInterrupt::enable();
        }
        #[cfg(feature = "low-power")]
        {
            // Configure EXTI wakeup lines for LPTIM on chips that require
            // explicit EXTI routing to wake from STOP mode.
            //
            // STM32WL: LPTIM1/2/3 use EXTI lines 29/30/31 respectively.
            // STM32WBA: LPTIM interrupts wake from STOP directly — no EXTI
            //           routing needed.
            #[cfg(any(stm32wlex, stm32wl5x))]
            {
                #[cfg(time_driver_lptim1)]
                const EXTI_WAKEUP_LINE: usize = 29;
                #[cfg(time_driver_lptim2)]
                const EXTI_WAKEUP_LINE: usize = 30;
                #[cfg(time_driver_lptim3)]
                const EXTI_WAKEUP_LINE: usize = 31;

                #[cfg(stm32wl5x)]
                {
                    #[cfg(feature = "_core-cm4")]
                    const CPU: usize = 0;
                    #[cfg(feature = "_core-cm0p")]
                    const CPU: usize = 1;

                    crate::pac::EXTI
                        .cpu(CPU)
                        .imr(0)
                        .modify(|w| w.set_line(EXTI_WAKEUP_LINE, true));
                    crate::pac::EXTI
                        .cpu(CPU)
                        .emr(0)
                        .modify(|w| w.set_line(EXTI_WAKEUP_LINE, true));
                }
                #[cfg(stm32wlex)]
                {
                    crate::pac::EXTI.imr(0).modify(|w| w.set_line(EXTI_WAKEUP_LINE, true));
                }
            }

            // U5/U3: the LP timer used as time driver must remain functional in
            // STOP mode so it can wake the CPU. Two RCC bits are required:
            //  - SRDAMR.*amen: autonomous mode enable, so the peripheral can
            //    request its kernel clock (LSE/LSI) while in STOP.
            //  - APB3SMENR.*smen: keep the APB bus clock available in STOP so
            //    the NVIC interrupt reaches the CPU.
            // Without these, the LPTIM keeps its registers but its interrupt
            // cannot wake the core from STOP, hanging the executor.
            #[cfg(any(stm32u5, stm32u3))]
            {
                #[cfg(time_driver_lptim1)]
                {
                    crate::pac::RCC.srdamr().modify(|w| w.set_lptim1amen(true));
                    crate::pac::RCC.apb3smenr().modify(|w| w.set_lptim1smen(true));
                }
                #[cfg(time_driver_lptim3)]
                {
                    crate::pac::RCC.srdamr().modify(|w| w.set_lptim3amen(true));
                    crate::pac::RCC.apb3smenr().modify(|w| w.set_lptim3smen(true));
                }
                #[cfg(time_driver_lptim4)]
                {
                    crate::pac::RCC.srdamr().modify(|w| w.set_lptim4amen(true));
                    crate::pac::RCC.apb3smenr().modify(|w| w.set_lptim4smen(true));
                }
            }
        }
    }

    fn init(&'static self, cs: CriticalSection) {
        self.init_timer(cs);
        regs_lptim().cr().modify(|w| w.set_cntstrt(true));
    }

    pub(crate) fn on_interrupt(&self) {
        let r = regs_lptim();

        critical_section::with(|cs| {
            let sr = r.isr().read();
            let ier = ier_read(r);
            #[cfg(any(stm32wba, stm32u5, stm32u3))]
            if sr.dierok() {
                stats::acknowledged(ier);
            }

            // Clear all interrupt flags. Bits in ICR are "write 1 to clear"
            icr_write_raw(r, ier);
            icr_write_raw(r, sr.0);

            // Overflow
            // wrist-rs: an alarm already due once `next_period` has enabled it, whose flag
            // the enable may have cleared (ES0553 2.12.3), is handled now.
            let mut overdue = false;
            if sr.ue() {
                self.next_period();
                overdue = self.alarm.borrow(cs).timestamp.get() <= self.now();
                #[cfg(any(stm32wba, stm32u5, stm32u3))]
                if overdue {
                    stats::overdue_at_overflow(ier & 1 == 0);
                }
            }

            if (sr.ccif(0) && ier_ccie(r, 0)) || overdue {
                #[cfg(any(stm32wba, stm32u5, stm32u3))]
                {
                    let mut context = 0;
                    if sr.ue() {
                        context |= stats::AT_OVERFLOW;
                    }
                    if ier & 1 == 0 {
                        context |= stats::CCIE_CLEAR;
                    }
                    let at = self.alarm.borrow(cs).timestamp.get();
                    stats::handled(at, self.now(), r.ccr(0).read().ccr(), context);
                }
                self.trigger_alarm(cs);
            }
        })
    }

    fn next_period(&self) {
        let r = regs_lptim();

        // We only modify the period from the timer interrupt, so we know this can't race.
        let period = self.period.load(Ordering::Relaxed) + 1;
        self.period.store(period, Ordering::Relaxed);
        let t = (period as u64) << 16;

        critical_section::with(move |cs| {
            let alarm = self.alarm.borrow(cs);
            let at = alarm.timestamp.get();

            // Arm now if it falls in this counter cycle (upstream fb82051b5). One a few ticks
            // into the cycle may already have passed; `on_interrupt` handles it.
            if at < t + 0x1_0000 {
                // just enable it. `set_alarm` has already set the correct CCR val.
                ier_set_ccie(r, 0, true);
            }
        })
    }

    fn trigger_alarm(&self, cs: CriticalSection) {
        trace!("trigger_alarm");
        let mut next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now());
        while !self.set_alarm(cs, next) {
            next = self.queue.borrow(cs).borrow_mut().next_expiration(self.now());
        }
    }

    fn set_alarm(&self, cs: CriticalSection, timestamp: u64) -> bool {
        trace!("set_alarm: timestamp: {}", timestamp);
        let r = regs_lptim();

        self.alarm.borrow(cs).timestamp.set(timestamp);

        let t = self.now();
        if timestamp <= t {
            trace!("set_alarm: timestamp <= t");
            // If alarm timestamp has passed the alarm will not fire.
            // Return `false` to indicate that. wrist-rs: `CCIE` is left for the caller's retry,
            // which always follows, to set once.
            self.alarm.borrow(cs).timestamp.set(u64::MAX);

            return false;
        }

        // Write the CCR value regardless of whether we're going to enable it now or not.
        // This way, when we enable it later, the right value is already set.
        write_compare(r, timestamp as u16);

        // Enable it if it'll happen this counter cycle. Otherwise, `next_period` will enable it.
        let this_cycle = timestamp < ((t >> 16) + 1) << 16;
        ier_set_ccie(r, 0, this_cycle);
        #[cfg(any(stm32wba, stm32u5, stm32u3))]
        stats::armed(!this_cycle);

        // Reevaluate if the alarm timestamp is still in the future
        let t = self.now();
        if timestamp <= t {
            trace!("set_alarm: timestamp <= t (after set)");
            // If alarm timestamp has passed since we set it, we have a race condition and
            // the alarm may or may not have fired.
            // Return `false` to indicate that.
            // It is the caller's responsibility to handle this ambiguity.
            // wrist-rs: as above, `CCIE` is left for the retry.
            self.alarm.borrow(cs).timestamp.set(u64::MAX);

            return false;
        }

        trace!("set_alarm: true");
        // We're confident the alarm will ring in the future.
        true
    }
}

#[cfg(feature = "low-power")]
impl super::LPTimeDriver for RtcDriver {
    /// Compute the approximate amount of time until the next alarm
    fn time_until_next_alarm(&self, cs: CriticalSection) -> embassy_time::Duration {
        let now = self.now() + 32;

        embassy_time::Duration::from_ticks(self.alarm.borrow(cs).timestamp.get().saturating_sub(now))
    }

    /// Set the stopped flag or return an error if the time until the next alarm is less than the minimum stop pause
    fn pause_time(&self, cs: CriticalSection) -> Result<(), ()> {
        trace!("pause_time");
        let time_until_next_alarm = self.time_until_next_alarm(cs);
        if time_until_next_alarm < self.min_stop_pause.borrow(cs).get() {
            trace!(
                "time_until_next_alarm < self.min_stop_pause ({})",
                time_until_next_alarm
            );
            Err(())
        } else {
            #[cfg(any(stm32wba, stm32u5, stm32u3))]
            if stats::pending() && !regs_lptim().isr().read().dierok() {
                stats::stop_with_pending();
            }
            self.is_stopped.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    /// Reset the stopped flag
    fn resume_time(&self, _cs: CriticalSection) {
        trace!("resume_time");
        self.is_stopped.store(false, Ordering::Relaxed);
    }

    fn set_min_stop_pause(&self, cs: CriticalSection, min_stop_pause: embassy_time::Duration) {
        trace!("set_min_stop_pause: {} ticks", min_stop_pause.as_ticks());
        self.min_stop_pause.borrow(cs).replace(min_stop_pause);
    }
}

impl Driver for RtcDriver {
    fn now(&self) -> u64 {
        let r = regs_lptim();
        loop {
            let period = self.period.load(Ordering::Relaxed);
            compiler_fence(Ordering::Acquire);
            let pending = r.isr().read().ue();
            let counter = r.cnt().read().cnt();
            let counter2 = r.cnt().read().cnt();
            let pending2 = r.isr().read().ue();
            let period2 = self.period.load(Ordering::Relaxed);
            if let Some(now) = wrist_time::counter::extend((period, period2), (pending, pending2), (counter, counter2)) {
                break now;
            }
        }
    }

    fn schedule_wake(&self, at: u64, waker: &core::task::Waker) {
        critical_section::with(|cs| {
            let mut queue = self.queue.borrow(cs).borrow_mut();

            if queue.schedule_wake(at, waker) {
                #[cfg(any(stm32wba, stm32u5, stm32u3))]
                {
                    let alarm = self.alarm.borrow(cs).timestamp.get();
                    let ccr = regs_lptim().ccr(0).read().ccr();
                    stats::handled(alarm, self.now(), ccr, stats::BY_SCHEDULE);
                }
                let mut next = queue.next_expiration(self.now());
                while !self.set_alarm(cs, next) {
                    next = queue.next_expiration(self.now());
                }
            }
        })
    }
}

pub(crate) const fn get_driver() -> &'static RtcDriver {
    &DRIVER
}

pub(crate) fn init(cs: CriticalSection) {
    DRIVER.init(cs)
}
