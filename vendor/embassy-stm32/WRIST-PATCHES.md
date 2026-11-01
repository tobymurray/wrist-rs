# Local patches to embassy-stm32

Upstream: https://github.com/embassy-rs/embassy/tree/cf9beb7173159ce1b60dadb9be11583c20c7949a/embassy-stm32
License: MIT OR Apache-2.0 (included).

Sibling path dependencies are removed; the app pins Embassy's other crates to the same
revision. Remove this copy once upstream has all of these, or equivalents:

- **LPTIM4 time driver (U5).** Upstream's `time-driver-lptim4` does not build on U5, whose
  LPTIM4 has the basic register block, which `SealedInstance::regs` does not cover; the
  driver uses `pac::LPTIM4` directly. It selects the LSE for LPTIM3/4 itself
  (`CCIPR3.LPTIM34SEL`), which the app's RCC configuration must name too, or the LSI when the
  LSE is not ready, counting its 32 kHz as 32768 Hz (2.3 % slow), and enables LPTIM4 in
  Stop (`SRDAMR`, `APB3SMENR`). `rcc::init_rcc` leaves its clock alone. The late-alarm
  threshold (5 ms) and `time_until_next_alarm`'s lead (upstream's 32 ticks, 1 ms) are in
  time, not ticks, so they hold at the image's 8,192 Hz tick.
- **LPTIM compare (U5).** As upstream, `CMPOK` is cleared before `CCR` is written; here the
  wait for the write's own `CMPOK` is also bounded, and a `CCR` that already holds the value
  is not written again, since no write is left unacknowledged.
- **LPTIM `DIER` writes (U5, WBA, U3).** As upstream
  ([#7417](https://github.com/embassy-rs/embassy/pull/7417)), each change waits for
  `DIEROK` and an unchanged value is not written; here the wait is bounded in time rather
  than in polls, and counted. Entering Stop 2 with a `CCIE` enable unacknowledged loses the
  compare wakeup, and the alarm runs one counter period late, so `set_alarm` writes `CCIE` once,
  to its final value, where upstream clears it and sets it again on a retry. A `DIEROK` or
  `CMPOK` not seen within 2 ms, against at most 215 us measured undivided, does not stop the driver:
  the write is made once more and, if still unacknowledged, left to the overflow, whose
  `next_period` (upstream) handles an alarm already due, including one whose flag enabling
  `CCIE` cleared (ES0553 2.12.3).
- **Backup SRAM clock (U5).** `BKPSRAMEN` is set by `BackupMemory::new`, not by every
  `LsConfig::init`, which runs at each Stop exit too, so an image that does not use the
  backup SRAM can leave its clock off.
- **Time driver counters.** `low_power::time_driver_stats::snapshot()` returns the `DIER`
  and compare waits, the compare writes skipped, Stop entries with a write unacknowledged,
  the waits that timed out, with the registers at the last, and the last late alarms.
- **Stop 2 on U5.** Peripheral clock enables do not add the blanket Stop 2 lock, since U5
  retains peripheral registers in Stop 2
  ([AN5371](https://www.st.com/resource/en/application_note/an5371-migration-from-stm32l5-series-to-stm32u5-series-microcontrollers-stmicroelectronics.pdf),
  [AN5652](https://www.st.com/resource/en/application_note/an5652-how-to-optimize-power-consumption-on-stm32u5-mcus-stmicroelectronics.pdf)).
  Operation and DMA guards are unchanged, so a driver whose operation spans an await
  must hold its guard.
- **Stop observer.** `low_power::set_stop_observer(fn(StopEvent))` reports each Stop entry
  and each exit `PWR.SR.STOPF` confirms, with `PWR.CR1.LPMS`; the clocks' restore after it;
  and each wait in Sleep when no Stop mode is allowed, with the Stop refcounts, and its end.
- **`rcc::reconfigure(config, now, &mut rcc)`** makes `config` what every Stop exit
  restores, without `rcc::reinit`'s reset of the time driver.

## Merged upstream

Local patches that upstream now has, in the revision above:

- **Interrupt flags cleared as read**
  ([#7409](https://github.com/embassy-rs/embassy/pull/7409)): an overflow arriving during
  the handler is no longer cleared unseen.
- **LPTIM counter snapshot** ([#7408](https://github.com/embassy-rs/embassy/pull/7408)):
  `now()` samples the pending overflow on both sides of the counter read and retries if it
  changed, so an overflow between the reads no longer puts the time a period ahead. It
  replaces `wrist_time::counter::extend`.
- **`DIEROK` between `DIER` writes**
  ([#7417](https://github.com/embassy-rs/embassy/pull/7417)), refined above.
- **`embassy-usb-synopsys-otg` reinitialization**
  ([#7381](https://github.com/embassy-rs/embassy/pull/7381)): `Driver::new` clears the
  endpoint allocations, enabled flags and pending setup a previous driver left in the shared
  `State`, so that crate is no longer vendored. Upstream clears the OUT receive state on each
  bus reset, and a dropped read clears its transfer.
