# Local patches to embassy-stm32

Upstream: https://github.com/embassy-rs/embassy/tree/592a8c2e1f727f483d238dd01c0862e1e0c51c73/embassy-stm32
License: MIT OR Apache-2.0 (included).

Sibling path dependencies are removed; the app pins Embassy's other crates to the same
revision. Remove this copy once upstream has all of these, or equivalents:

- **LPTIM4 time driver (U5).** Upstream's `time-driver-lptim4` does not build on U5, whose
  LPTIM4 has the basic register block, which `SealedInstance::regs` does not cover; the
  driver uses `pac::LPTIM4` directly. It selects the LSE for LPTIM3/4 itself
  (`CCIPR3.LPTIM34SEL`), which the RCC configuration has no field for, or the LSI when the
  LSE is not ready, counting its 32 kHz as 32768 Hz (2.3 % slow), and enables LPTIM4 in
  Stop (`SRDAMR`, `APB3SMENR`). `rcc::init_rcc` leaves its clock alone.
- **LPTIM compare (U5).** As upstream, `CMPOK` is cleared before `CCR` is written; here the
  wait for the write's own `CMPOK` is also bounded, and a `CCR` that already holds the value
  is not written again, since no write is left unacknowledged.
- **LPTIM `DIER` writes (U5, WBA, U3).** Each change waits for `DIEROK` (bounded), and an
  unchanged value is not written: entering Stop 2 with a `CCIE` enable unacknowledged loses
  the compare wakeup, and the alarm runs one 2 s period late. `set_alarm` writes `CCIE` once,
  to its final value. A `DIEROK` or `CMPOK` not seen within 2 ms, against at most 215 us
  measured, does not stop the driver: the write is made once more and, if still
  unacknowledged, left to the overflow, whose `next_period` (upstream) handles an alarm
  already due, including one whose flag enabling `CCIE` cleared (ES0553 2.12.3).
- **LPTIM counter snapshot.** `now()` reads the counter between two reads of the pending
  overflow and of `period`, through `wrist_time::counter::extend` (host-tested in
  [`crates/wrist-time`](../../crates/wrist-time)). Upstream reads the counter and then the
  pending overflow, so an overflow between the two reads puts the time a period ahead.
- **Interrupt flags cleared as read** (upstream
  [#7409](https://github.com/embassy-rs/embassy/pull/7409)): the handler clears only the
  flags it read. It also cleared the enable mask, and `UEIE` and `UECF` are the same bit,
  so an overflow arriving during the handler was cleared unseen: a counter period lost for
  good, and time 2 s behind.
- **Time driver counters.** `low_power::time_driver_stats::snapshot()` returns the `DIER`
  and compare waits, the compare writes skipped, Stop entries with a write unacknowledged,
  the waits that timed out, with the registers at the last, and the last late alarms.
- **Stop 2 on U5.** Peripheral clock enables do not add the blanket Stop 2 lock, since U5
  retains peripheral registers in Stop 2
  ([AN5371](https://www.st.com/resource/en/application_note/an5371-migration-from-stm32l5-series-to-stm32u5-series-microcontrollers-stmicroelectronics.pdf),
  [AN5652](https://www.st.com/resource/en/application_note/an5652-how-to-optimize-power-consumption-on-stm32u5-mcus-stmicroelectronics.pdf)).
  Upstream no longer resets the Stop refcounts after `init`, so without this the time
  driver's own enable would keep the core out of Stop 2. Operation and DMA guards are unchanged, so a driver
  whose operation spans an await must hold its guard.
- **Stop observer.** `low_power::set_stop_observer(fn(StopEvent))` reports each Stop entry
  and each exit `PWR.SR.STOPF` confirms, with `PWR.CR1.LPMS`; the clocks' restore after it;
  and each wait in Sleep when no Stop mode is allowed, with the Stop refcounts, and its end.
- **`rcc::reconfigure(config, now, &mut rcc)`** makes `config` what every Stop exit
  restores, without `rcc::reinit`'s reset of the time driver.
