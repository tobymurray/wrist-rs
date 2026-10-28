# Local patches to embassy-stm32

Upstream: https://github.com/embassy-rs/embassy/tree/81cabe7e5307c702d27e98577ac5e063c7796910/embassy-stm32
License: MIT OR Apache-2.0 (included).

Sibling path dependencies are removed; the app pins Embassy's other crates to the same
revision. Remove this copy once upstream has all of these, or equivalents:

- **Log formatting.** Three placeholders in `low_power.rs` and `time_driver/lptim.rs` use
  `Debug`, not `Display`, which upstream fails to compile with `log`.
- **LPTIM compare (U5).** `CMPOK` is cleared before `CCR` is written, and the wait for the
  write's own `CMPOK` is bounded: a stale `CMPOK` makes compare wakeups miss.
- **LPTIM counter snapshot.** An overflow pending but not yet serviced is accounted for,
  through `wrist_time::counter::extend` (host-tested in [`crates/wrist-time`](../../crates/wrist-time)); without it
  the time can go backwards at rollover.
- **DMA interrupt.** Channels are indexed through `DmaInfo::ch`: casting GPDMA to the
  four-channel LPDMA type panics for channels 4 and 5.
- **Stop 2 on U5.** Idle peripheral clock enables do not add the blanket Stop 2 lock,
  since U5 retains peripheral registers in Stop 2
  ([AN5371](https://www.st.com/resource/en/application_note/an5371-migration-from-stm32l5-series-to-stm32u5-series-microcontrollers-stmicroelectronics.pdf),
  [AN5652](https://www.st.com/resource/en/application_note/an5652-how-to-optimize-power-consumption-on-stm32u5-mcus-stmicroelectronics.pdf)).
  Operation and DMA guards are unchanged, so a driver whose operation spans an await
  must hold its guard.
- **Stop observer.** `low_power::set_stop_observer(fn(StopEvent))` reports each Stop entry
  and each exit `PWR.SR.STOPF` confirms, with `PWR.CR1.LPMS`; the clocks' restore after it;
  and each wait in Sleep when no Stop mode is allowed, with the Stop refcounts, and its end.
- **`rcc::reconfigure(config, now, &mut rcc)`** makes `config` what every Stop exit
  restores, without `rcc::reinit`'s reset of the time driver.
- **LPTIM4 time driver (U5).** `time-driver-lptim4` runs the time driver on LPTIM4's basic
  register block, leaving LPTIM1 and LPTIM3 to the panel's `VCOM`; it selects the LSE for
  LPTIM3/4 itself (`CCIPR3.LPTIM34SEL`), which the RCC configuration has no field for, or
  the LSI when the LSE is not ready, counting its 32 kHz as 32768 Hz (2.3 % slow).
- **LPTIM `DIER` writes (U5, WBA, U3).** Each change waits for `DIEROK` (bounded), and an
  unchanged value is not written: entering Stop 2 with a `CCIE` enable unacknowledged loses
  the compare wakeup, and the alarm runs one 2 s period late. `set_alarm` writes `CCIE` once, to its final value,
  and an alarm already due after `next_period` runs at the overflow (ES0553 2.12.3).
- **Alarms armed for the current counter cycle** (upstream
  [`fb82051b5`](https://github.com/embassy-rs/embassy/commit/fb82051b5), #7241): `CCIE` is
  set for any alarm before the next overflow; setting it only under `0xC000` ticks away
  leaves an alarm in a cycle's last quarter up to 500 ms late.
- **Time driver counters.** `low_power::time_driver_stats::snapshot()` returns the `DIER`
  and compare waits, Stop entries with a write unacknowledged, and the last late alarms.
