# Hardware notes

What the watch-face image relies on, derived from
[UNAWatch/una-hardware](https://github.com/UNAWatch/una-hardware) (CC BY 4.0, Una Watch
Ltd), UNAcore Rev3.2 unless stated, and from measurements on a watch. The schematics are
PDFs with no netlist, so every claim is tagged:

- **CONFIRMED**: printed as text in the schematic, or matched by a measurement on a watch.
- **MEASURED**: seen on a watch running this project's firmware.
- **LIKELY**: read off a rendered image, not cross-checked.
- **UNKNOWN**: not answered by the schematics.

Primary sources: the UNAcore Rev3.2 schematic and BOM, ST's `STM32U5A5.svd` and
`stm32u5a5xx.h`, ST's CubeMX pin database (`STM32_open_pin_data`), RM0456 Rev 7 (the
STM32U5 reference manual), DS13543 Rev 3 (the STM32U5Axxx datasheet), the PCA9420
datasheet Rev 4.2 and the NX20P0407 datasheet Rev 1.3. The ordering key decodes
`STM32U5A5QJI6Q` as 5A5 line, Q = 132 balls, J = 4 Mbytes, I = UFBGA, 6 = industrial,
Q = SMPS pinout.

## Debug and boot

| Claim | Status |
|---|---|
| Test points for SWDIO, SWCLK, BOOT0, RST_L, SWO and DEBUG_TX on the core board (sheet 2) | CONFIRMED |
| SWD can also route through the USB-C connector, only with `R27`–`R30` fitted (marked NOPOP) | CONFIRMED |
| `DEBUG_TX` is on PC4 | CONFIRMED on the schematic itself, not only the ECN 4 note: sheet 2 draws `DEBUG_TX` on `PC4` at ball **M3** (ST's package database puts PC4 at M3), with test point **TP35** directly on the net and an off-sheet port to sheet 6. Nothing sits between the ball and TP35. |
| Backup registers across a system reset: `TAMP_BKP3R` is **zeroed on every reset** before this image runs (the vendor bootloader, presumably), `TAMP_BKP5R` holds `0x00A60602`, which this project never writes, and `BKP0R`, `BKP1R`, `BKP2R`, `BKP4R`, `BKP6R` to `BKP10R` and `BKP31R` survive. The image's allocation: `BKP1R` the zone, tag `TZ` and minutes east of UTC; `BKP2R` who last set the calendar, tag `SR`; `BKP11R` to `BKP14R` the zone's next two changes, tags `T1` and `T2` with the new offset, then the UTC second each takes effect; `BKP31R` the local time last set, seconds since 1970 ([`crates/wrist-time/src/zone.rs`](crates/wrist-time/src/zone.rs), [`apps/standalone-boot/src/rtc.rs`](apps/standalone-boot/src/rtc.rs)). `BKP16R` whether the radio is on, tag `BL` (`ble` builds, [`apps/standalone-boot/src/rtc.rs`](apps/standalone-boot/src/rtc.rs)). `BKP4R` and `BKP6R` to `BKP10R` are reserved for an alarm and compass calibration; `BKP15R` and `BKP17R` to `BKP30R` are free. All are lost with the backup domain on a PMIC hard reset | **MEASURED**: markers written, reset, read back. `BKP16R` survives a reset and an update's reset (the radio came back on after both, 2026-10-24). `BKP15R` and `BKP17R` to `BKP30R` untested |
| I2C1 is embassy's async DMA driver: **a blocking call on it after async transfers have run put every later read a byte out** (PMIC and gauge garbage) until a reset | **MEASURED**. Blocking calls are used only at boot, before the first async transfer. The mechanism is not pinned down |
| **`DEBUG_TX` reaches the UNA Dev Tool 2.0 only while the MCU drives `DAM_DETECT` (PB15) high** — the tap is *not* firmware-independent | CONFIRMED (sheet 6). The only route from `DEBUG_TX` to the outside is `R40` (200 Ω) → `U22` NX20P0407 SBU switch → USB-C `SBU1` (`J6`). `U22`'s `SBUEN` is the net `DEBUG_EN`, which header `J11` (the "debug-enable jumper") connects to `DAM_DETECT`; `DAM_DETECT` is MCU pin **PB15** (ball K12, ECN 7: "Connected DAM_DETECT to U1.K12") and nothing else. NX20P0407 datasheet Rev 1.3, pin table and Table 4: `SBUEN` is active high with a **460 kΩ internal pull-down**, and the SBU switch is OFF while it is low. The vendor kernel detects Debug Accessory Mode and drives PB15; a standalone image that leaves PB15 at its reset state (input, floating) has its UART physically disconnected from the connector, at every baud. **A standalone image must set PB15 high before its first byte.** `U22` is also powered from `3V3_USB` (`U23`, enabled by `5V0_USB`), so the path only exists with VBUS present — which is the real reason the watch must be powered through the tool. The tool is a T-shaped PCB the watch clips onto at J1; `U1` is an **FTDI FT231X** (`0403:6015`) that enumerates as `/dev/ttyUSB0` and takes the watch's `DEBUG_TX` at **1.8 V logic** into its RX. It taps the pin rather than riding a kernel-side USB channel, so a standalone image's output *can* reach it with no kernel running — **but only through `U22`, and only if the image itself drives PB15 high** (above). Four conditions all have to hold: PB15 high; the **debug-enable jumper** is fitted (pads by the battery, T3 screws to get in); the watch is powered **through the dev tool, not a data cable** (a data cable forces charge mode, where apps do not run); and the host baud matches. |
| **The dev tool is monitor-only — no SWD, and no RX path to the watch** | CONFIRMED for the data path; **one caveat from sheet 6**: `U22`'s second channel joins USB-C `SBU2` to `RST_L` through `R39`. ECN 9 (Rev 3.2, BOM only) made `R39` NOPOP, but the Rev 3.1 core module these notes were taken on has **`R39` fitted (200 Ω)** — so whenever `DAM_DETECT` is high and the tool is attached, the tool's SBU2 line sits on the MCU's `NRST`. Whether the tool ever drives it is UNKNOWN (no tool schematic). Same source otherwise. It cannot program or dump flash, so it does not substitute for a debug probe; recovery stays the bootloader long-press rollback. No RX also means an image that only transmits, and never waits on input, loses nothing. |
| The vendor kernel logs at **921600 baud**, which a standalone image on the post-reset clock **cannot match** | CONFIRMED by arithmetic on the confirmed post-reset clock: with `OVER8` = 0 `USARTDIV` must be ≥ 16, so at 4 MHz the ceiling is 250000 baud and 921600 needs `USARTDIV` 4.34. Reachable cleanly at 4 MHz: 115200 (−0.79%), 57600 (+0.64%), 38400/19200 (+0.16%), 9600 (−0.08%); 230400 is +2.12% and marginal. **So the host has to be re-set to the image's rate after the install resets** — capturing at 921600 out of habit shows nothing and looks exactly like an image that never ran. (921600 would need the clock raised to MSIS range 0, where `BRR` 52 gives +0.16%.) |
| The bootloader at `0x08000000` does not verify the image at `0x08060000` | CONFIRMED (see "Boot handoff") |

## MCU pin map

All 76 pins that carry a net are in [`crates/wrist-board/src/pins.rs`](crates/wrist-board/src/pins.rs), generated by
[`tools/extract_pinmap.py`](tools/extract_pinmap.py) from the schematic PDF's text layer. Net labels sit a fixed
distance above their pin row; the script applies that offset and refuses to run unless
every pin in the schematic's own "Interrupts" table comes out as the table says (17
pins, all agree).

Checked independently against the STM32U5A5 datasheet's alternate-function table
(DS13543), none contradicted:

| Net group | Pins | Peripheral | Status |
|---|---|---|---|
| `MAIN_SCL/SDA` | PB6, PB7 | I2C1 (AF4 per ST's GPIO database), which an I2C scan found hosting 0x36, 0x5A, 0x61; sheet 5 wires it to the PCA9420 `SCL`/`SDA` and the MAX17262 | CONFIRMED |
| `SENSOR1_SCL/SDA` | PC0, PC1 | I2C3 | CONFIRMED; MEASURED: the Bluetooth module's EEPROM answers at 0x50 at 100 kHz |
| `SENSOR2_SCL/SDA` | PD12, PD13 | I2C4, which hosts the BMI270 (0x68) and the BMM350 (0x14) | CONFIRMED; MEASURED: BMI270 chip id 0x24, BMM350 chip id 0x33. The image only resets both into suspend |
| `SENSORBODY_SCL/SDA` | PB13, PB14 | I2C2 | CONFIRMED |
| `SENSOR1_SCK/MISO/MOSI` | PG2, PG3, PB5 | SPI1 (found configured on the watch) | CONFIRMED |
| `SENSOR2_SCK/MISO/MOSI` | PD1, PC2, PC3 | SPI2 | CONFIRMED |
| `SENSORBODY_SCK/MISO/MOSI` | PG9, PG10, PD6 | SPI3 (found unconfigured on the watch) | CONFIRMED |
| `SDIO_D0..D3/CK/CMD` | PC8–PC11, PC12, PD2 | SDMMC1 | CONFIRMED |
| `SW1`, `SW3`, `SW4` | PD4, PA5, PD8 | buttons, labelled **Up, Down, Back** on the case (MEASURED, pressed one at a time); SW2 is the PMIC power button, seen at PD15 as `PWR_ON_1V8_L` | CONFIRMED |
| `DEBUG_TX` | PC4 | USART3_TX, **AF7** | CONFIRMED — derived from ST's CubeMX GPIO database (`GPIO-STM32U59x_gpio_v1_0_Modes.xml`): PC4 offers `USART3_TX` as `AF7_USART3`, `I2C6_SMBA` as AF2, `OCTOSPIM_P1_IO7` as AF10; PC5 offers `USART3_RX` as AF7. Re-read in DS13543 Rev 3 Table 28: the PC4 row is `-, -, I2C6_SMBA, -, -, -, -, USART3_TX` across AF0–AF7. |
| `DAM_DETECT` | PB15 | plain GPIO output; **enables the SBU switch that carries `DEBUG_TX` (and, on Rev 3.1, `RST_L`) to the USB-C connector** | CONFIRMED, sheet 6 (see "Debug and boot") |
| `USB_D_N/P` | PA11, PA12 | USB | CONFIRMED |
| `SWDIO/SWCLK/SWO` | PA13, PA14, PB3 | debug | CONFIRMED |
| `BUZZER_PWM` | PB8 | TIM4_CH3, `DIN` of `U7` PAM8904E; `BUZZER_EN1` PB9 and `BUZZER_EN2` PD5 are its `EN1`/`EN2`, 0/0 shutdown (DS41667 Rev. 4-2) | CONFIRMED; MEASURED |
| `LCD_FRP`, `LCD_XFRP` | PB2, PD14 | LPTIM1_CH1 (AF1), LPTIM3_CH1 (AF14): both timers are in the SmartRun domain and run in Stop2 from the LSE. They generate `VCOM`, and the time driver is on LPTIM4 | CONFIRMED (pins); AF numbers from stm32-data / CubeMX for STM32U5A5QJ |
| `LCD_QSPI_IO3_HCK`, `LCD_QSPI_IO2_ENB` | PA6, PA7 | TIM16_CH1, TIM3_CH2 | CONFIRMED |
| `LCD_R1/G1/B1/R2/G2/B2` | PE0, PE2, PE4, PE8, PE10, PE12 | plain GPIO (LTDC is absent on this die) | CONFIRMED |

## Modules and slots

The core board has four module connectors: Sensor 1 (I2C3, SPI1, USART1), Sensor 2
(I2C4, SPI2, USART2), Body (I2C2, SPI3) and the display. The two sensor slots carry the
same ten signals (`SCL`, `SDA`, `SCK`, `MISO`, `MOSI`, `CS`, `RST_L`, `TX_INT0`,
`RX_INT1`, `INT2`), and `SENSOR*_TX_INT0/RX_INT1` are PA9/PA10 and PA2/PA3, which are
USART1 and USART2 pins. **Electrically** a module can therefore use either slot as I2C,
SPI or UART. **LIKELY**, from the schematics alone.

What each module carries, from its own schematic:

| Module | Parts | Slot signals used |
|---|---|---|
| UNAlink_BLUENRG2 | BlueNRG-2 (BlueNRG-234N), N24S64B EEPROM | SPI (`BLE_SPI_*`), reset, IRQ, I2C for the EEPROM |
| UNAlink_AG3335M | AG3335M GNSS, **BMI270 (0x68), BMM350 (0x14, `MAG_INT` to the slot's MISO, PC2)**, N24S64B EEPROM | UART (`TXD_GNSS_TO_HOST`, `RXD_HOST_TO_GNSS`), `GNSS_EN`, wake lines, I2C for IMU, magnetometer and EEPROM |
| UNAbody_PAH8316 | PAH8316LS, N24S64B EEPROM | Body slot |
| UNAview_LS012 | LS012B7DD06A panel, N24S64B EEPROM | display connector, `MAIN_SCL/SDA` |
| UNAaltimeter_MS5837 | MS5837 | `SENSORBODY` I2C, powered by the core's `ALTIMETER_EN` LDO |

**Module connector pinout, from the two module schematics** (`UNAlink_BLUENRG2` Rev 1.4
and `UNAlink_AG3335M` Rev 1.4, sheet 1; the BLE one names the slot nets
on its pins 9 and 10, which fixes the numbering, and the GNSS one is laid over it). Pin 1
slot `MOSI`, 2 `MISO`, 3 `CS`, 4 `SCK`, 5 `RST_L`, 6 `SCL`, 7 `TX_INT0`, 8 `SDA`, 9
`RX_INT1`, 10 `INT2`; ground and power on 11–24. So on the BLE module: `BLE_SPI_MOSI/MISO/CS/SCK` are the slot's SPI (Sensor 1: PB5, PG3, PA15, PG2), `BLE_RST_L` is `RST_L` (PA8),
and **`BLE_BOOT_SPI_IRQ` is `TX_INT0` (PA9)**. On the GNSS module: **`GNSS_EN` is `RST_L`
(Sensor 2: PG5)**, `RXD_HOST_TO_GNSS` is `TX_INT0` (**PA2**, `USART2_TX`), `TXD_GNSS_TO_HOST`
is `RX_INT1` (**PA3**, `USART2_RX`), `WAKE_HOST_TO_GNSS` is `SCK` (PD1), `WAKE_GNSS_TO_HOST`
is `MOSI` (PC3), `MAG_INT` is `MISO` (PC2), `RTC_EINT` is `CS` (PB12), `IMU_INT` is `INT2`
(PA1). `GNSS_EN` goes to the AG3335M's enable and, via the chip's `EXT_PWR_EN`, switches
the module's own 1.8 V LDO.

The IMU and magnetometer are on the GNSS module, so a watch without that module has no
IMU. GNSS talks UART, not I2C; the I2C pair on that module reaches the IMU,
magnetometer and EEPROM.

The image drives none of them but the display. It holds each slot's reset or enable
low and sets the slot's other lines to analog ("What the image turns off", below).

## The module EEPROM

Part: onsemi N24S64B, 8 KB array. Checked against its datasheet (rev. 3, June 2025):

- The array answers at `0x50 | A2A1A0`. The same address with `0x58 | A2A1A0` reaches
  three other things, chosen by the word address sent first: the 32-byte Secure Data
  Page (`0x0000`), a **16-byte read-only unique ID** (`0x0200`) and the Device
  Configuration Register (`0x0600`). A2:A0 default to 0 and can be reprogrammed.
- The same `0x58` header also writes the page lock, the write-protect bit and A2:A0, so
  [`crates/wrist-eeprom`](crates/wrist-eeprom) only ever sends the two address bytes of a read.
- So 0x50 and 0x58 answer on all four module buses because a module with an EEPROM sits
  on each. CONFIRMED against the datasheet.

### Module records, read from one watch under the stock kernel

All four buses answered at 0x50, all with A2:A0 = 0 and the configuration register at
`0x1D`, the datasheet's delivery state. Every Secure Data Page is blank (`FF`), so nothing
is locked or stored there. The unique IDs are 16 bytes as the datasheet says; they are
per-unit and are not recorded here.

| Bus | Slot | Record | Name | Revision |
|---|---|---|---|---|
| I2C1 | view | **not an `MI` record**: the ASCII text `view_LS012-1_3`, NUL, then `FF` | | |
| I2C3 | link1 (Sensor 1) | `MI` v1 | `BLUENRG2` | `1.3` |
| I2C4 | link2 (Sensor 2) | `MI` v1 | `AG3335M` | `1.2` |
| I2C2 | body | `MI` v1 | `PAH8316` | `1.0` |

- The layout read from the kernel is right: names at byte 4, revisions at byte 36, serials
  (byte 68) empty on all three.
- **The display module carries an older format.** The kernel image has no parser for it
  (no `view_` string), so its `MODULE_INFO` presumably reports `view` as a read error.
  A firmware that identifies modules has to accept both. [`crates/wrist-eeprom`](crates/wrist-eeprom) reads it as
  `<slot>_<name>-<revision>` (`view`, `LS012`, `1_3`), a shape inferred from this one label
  and parsed strictly: anything looser is "unrecognised", which refuses nothing.
- **The fitted revisions are older than the published schematics** for the radios: BLE
  1.3 against schematic Rev1.4, GNSS 1.2 against Rev1.4. Display (1.3) and body (1.0)
  match. Pin-level claims from those two schematics are about a later board.
- The slot assignment on this watch is the one the vendor firmware assumes.

**Read by this project's firmware: the same four records**, each
read twice at boot in four 25-byte pieces and identical both times. Two reads take 22.0 to
22.2 ms at 100 kHz (I2C1, I2C2, I2C3) and 6.2 ms at 400 kHz (I2C4). The body EEPROM
answered with `ALTIMETER_EN` (PG0) high and the body slot's `RST_L`/`CS` (PF13/PG12) not
yet configured, so it needs neither `PPG_EN` candidate; whether it needs PG0 was not tried.
The image reads the display's, link1's and the GNSS module's records at boot, and a slot
whose record names another module is not driven ([`apps/standalone-boot/src/modules.rs`](apps/standalone-boot/src/modules.rs)).

So:

- **The modules are not interchangeable under the vendor firmware.** Its kernel builds a
  driver by slot, from the EEPROM's presence, never reading the record. Our own firmware
  can read the record and choose the driver from what is plugged in.
- **UNA's `architecture-deep-dive.md` has the structure right and two details wrong.** It
  describes an 8-byte unique ID and a step that checks whether the ID "matches a known
  LCD/BLE/GPS". The ID is 16 bytes, and nothing is compared.
- The unique ID cannot say what a module is, but the module record can. That is the
  thing to read from the four buses.

Not GPIO and so not in the table (sheet 2): `BOOT0` (PH3, ball B5) pulled
to GND by `R9` 10 kΩ, so with `nSWBOOT0` = 1 in `FLASH_OPTR` the part always boots from
flash; `NRST` (ball G2) is the net `RST_L`, which is the **PCA9420's `SYSRST` output** (sheet 5)
with `C23` 100 nF to GND, no external pull-up on the core board and test point TP4;
`HSE_CLK_IN/OUT` (PH0/PH1, balls F1/G1) carry **`X1`, a fitted 16 MHz crystal** (BOM item 51,
ECS-160-8-37-CWY-TR3, `C69`/`C70` 8 pF); and `CLK_32K` into `PC14-OSC32_IN` (ball C1) comes from
`U2`, a **SiT1532 MEMS oscillator** (BOM item 36), not a crystal. The pin→ball pairs on sheet 2
were checked against ST's package database for the STM32U5A5QJI6Q: 65 pairs, 65 agree.

**The backup domain at handoff (MEASURED):** the vendor bootloader
configures it on every boot and leaves `RCC_BDCR` = `0x0C00A987`: LSE on, ready and in
bypass (the SiT1532), `LSESYSEN` and `LSESYSRDY` set, `RTCSEL` = LSE with `RTCEN`, and the
LSI on. It does not reset the domain: a word in `TAMP_BKP0R` and a running calendar
survive a reset through it. Only a PMIC hard reset (Select held 16 s), which cycles
`1V8_RTC`, clears it (MEASURED 2026-10-28: the PCA9420's reset monitor `0x71` went from
`0x88` to `0x89`, `LONG_ON_DONE`; `TAMP_BKP0R` read 0, the zone record was gone and the
calendar uninitialised; `RCC_CSR` at handoff stayed `0x14004400`, so it does not show the
hard reset). From wrist-rs, Select held 3 s reaches the bootloader's USB mode without it,
and the domain survives: a zone record set the day before came through that, stock 1.6.2
installed over it, two installer dry runs and two installs from stock. Any embassy-based image must give `embassy_stm32::init` a low-speed
config that matches those bits, `LSESYSEN` included, or embassy resets the domain and the
RTC calendar with it; both `wrist-loader` and `standalone-boot` mirror what they find.

**`VCK` and `VST` are fixed by the display module's drawing** (UNAview_LS012
Rev 1.3 sheet 2): its 30-pin connector carries `LCD_VST` on pin 17 and
`LCD_VCK` on pin 27, matching the core's `LCD_QSPI_SCK_VST` (PB10) and `LCD_QSPI_CS_VCK`
(PA4), and its 21-pin FPC maps those, through the TXB0108 shifters, onto the panel pin for
pin: `VST` → `GSP` (gate start, FPC 3), `VCK` → `GCK` (gate clock, 4), `ENB` → `GEN` (5),
`XRST` → `INTB` (6), `FRP` → both `VB` (7) and `VCOM` (20), `XFRP` → `VA` (8), `HST` → `BSP`
(11), `HCK` → `BCK` (12), `R1/G1/B1` → `R[0]/G[0]/B[0]` (odd pixels, 13/15/17) and
`R2/G2/B2` → `R[1]/G[1]/B[1]` (even, 14/16/18); `VDD1` is `3V3`, `VDD2` is `5V0`. So the
core table's TIM2_CH3 label is simply wrong about which pin; `VCK` is PA4. The panel is a
Sharp LS012B7DD06A (datasheet LCP-2619063C): 240 × 240, 2 bits per colour sent as two
planes per row, 483 `GCK` edges per frame, 123 `BCK` per line, `VCOM`/`VB`/`VA` 50 % square
waves (duty 48 to 52 %) with `VA` inverted, at 0.5 to 45 Hz in reflective mode but 30 to
45 Hz in transmissive mode (light on) (Table 6-3-3, split by mode in Rev A; Note 6-3-2:
flicker may be seen at slower rates), no maximum pulse widths and no minimum frame rate,
an all-black initialization frame required before `VCOM` starts, and a still image to be
rewritten against sticking (the precautions say both "less than two days" and "longer than
two hour"; the image rewrites every column each hour).
**CONFIRMED.** The image runs `VCOM` at 0.5 Hz with the light off and about
31 Hz while `BACKLIGHT_ON` is driven. LPTIM1 and LPTIM3 generate it on
`FRP` and `XFRP` without the core: `page vcom` read 0.5 Hz, and 31.0 Hz lit, from both.
**Measured on the panel:** the bit-banged driver in
[`apps/standalone-boot/src/lcd.rs`](apps/standalone-boot/src/lcd.rs) drew colour bands and a four-level grey ramp correctly, so
the colour channels, both bit planes (MSB plane first, then LSB, per line) and the odd/even
pixel pairing are as the datasheet says. **Orientation as worn:** gate line 0 is the right
edge of the screen and pixel pair 0 of each line is the top, i.e. the panel is mounted
rotated 90° relative to the datasheet's drawing. **Reading the datasheet's horizontal chart**:
the numbers 1 to 123 on `BCK` label half-periods, exactly as 1 to 483 on
`GCK` do, and pixel data is latched on both edges; one `BCK` edge per label with a new pixel
pair on each of edges 2 to 121 gives the right picture, one full cycle per label stretches
the pixel axis 2×. With that, the eight-colour test pattern rendered in screen orientation:
white, red, green,
blue, yellow, cyan, magenta, black top to bottom, red bar left, blue bar right. A frame takes
about 1.2 s at a 4 MHz core with 2 µs `BCK` half-periods, and about 500 ms at 48 MHz. The
image writes only the gate lines under glyphs that changed and fast-forwards the rest with
`GCK` half-periods of at least 2 µs and no `GEN`: the datasheet's partial update (Figure
6-5-4, Note 6-3-1: a reference, not guaranteed), with `INTB` still high for `thwINTB`'s
30.3 ms. `page` counted 35 of 240 columns for a minute, and by eye a 7 turned to an 8 with
no leftover pixels or garbled columns. **MEASURED**: `cortex_m::asm::delay` takes three
cycles an iteration on this core, so the driver's nominal 1 µs `BCK` half-periods were about
3.2 µs and a full frame took 395 ms. The driver now paces every wait on the cycle counter
from the edge before it, never shorter than [`wrist-panel`](crates/wrist-panel)'s minimums:
data held 300 ns after a `BCK` edge and set up 300 ns before the next (Table 6-3-3: 200 ns),
which comes to about 1.1 µs a half-period with the loop's own cycles, 216 of the 240 lines in
87 ms of which 81 ms is the CPU's, and a minute's 40 lines in 16 ms of CPU inside the 31 ms
`INTB` holds. By eye at this rate the pages draw cleanly.
**Labels 122 and 123 latch too, into pixels past the 240 the datasheet counts, and this
watch shows them** (MEASURED under a 200X microscope with a
single-row sweep: a lit row 238 or 239, the last pair, drew a second, shorter line beyond
the edge; rows 237 and 236 did not). Whatever the bus holds at those labels is what those
pixels show, so the driver puts a black word there after the last pair. The datasheet's
active area is φ30.24 mm at a 0.126 mm pitch, exactly 240 dots, so those pixels are outside
the specification; the glass shows roughly two to four unlit rows beyond row 239 at the
bottom of the screen and one beyond row 0 at the top. **Do not shift the picture to
even that out**: the bezel's printed ticks at 12, 3 and 9 o'clock, measured against a
ruler pattern drawn on the panel, put the case's centre within
0.4 px (rows) and 0.8 px (columns) of the centre of the addressed 240 × 240 array, inside
the photo's own uncertainty. The circle drawn by `wrist-glass` is centred on the case; the
extra pixels are simply more panel under the glass on one side, and they stay black.

## The Bluetooth module (BlueNRG-2)

What a `ble` build ([`apps/standalone-boot/src/ble.rs`](apps/standalone-boot/src/ble.rs)) relies on, all **MEASURED** on one watch with
the link1 module `BLUENRG2` rev 1.3, from Linux (BlueZ 5.87) unless a row says otherwise.

| Fact | Evidence |
|---|---|
| ST's network-coprocessor stack answers: HCI version 11, LMP 11, manufacturer `0x0030` (ST), firmware build 2353 | `ble version` after `ble on` |
| SPI1 in mode 1 at 1 MHz from PCLK2 (48 MHz) reads clean headers; reset is 5 ms low, then `ACI_BLUE_INITIALIZED` within 3 s | every `ble on`; mode 0 bit-slips headers (ST's X-CUBE-BLE2 uses mode 1 too) |
| The chip keeps its bonds through MCU resets and image updates: a bond made under an earlier image was still listed | `ble bonds` |
| **A characteristic that offers write alone gets no write from a bonded central**: BlueZ's write requests were neither answered nor reported (`ACI_GATT_ATTRIBUTE_MODIFIED`), with or without security permissions. Offering write without response as well, they arrive | four builds with write alone, the same bond and host; not tried unbonded |
| With `ATTR_PERMISSION_AUTHEN_WRITE \| ENCRY_WRITE` on RX, an unbonded central's write request is refused and its write without response reaches no handler; a bonded central's, over the encrypted link, arrive | `wrist-ble.py` and a probe after `bluetoothctl remove` |
| Numeric comparison between a `DisplayYesNo` watch and a `DisplayYesNo` BlueZ agent pairs and bonds, the watch's half answered with `ble yes`. The watch asks for neither security nor an MTU at connect: BlueZ sends the Pairing Request (LE Secure Connections, key size 16) and, after encryption, Exchange MTU 517, answered 220 | `wrist-ble.py --pair`; btmon |
| A BlueZ host with Secure Connections off is refused: Pairing Failed, reason 0x03 (Authentication Requirements) | `btmgmt sc off`, `wrist-ble.py --pair`; btmon |
| Pairing failed at the `DHKey` check until an address had been written to the public slot (`ACI_HAL_WRITE_CONFIG_DATA` offset 0), as ST's samples do first | pairing before and after the write |
| A bonded BlueZ central starts encryption 45 to 65 ms after the connection and is encrypted within 0.7 s; an unbonded one sends its Pairing Request within 0.1 s, or nothing at all if it only writes without response | btmon |
| An ATT MTU request or an SMP security request from the watch at connect ends a bonded macOS central's link with a MIC failure: reconnects with both 0 of 6, the security request alone 1 of 4, the MTU request alone 0 of 3, neither 3 of 3 | one Mac and one bond, an image per case |
| BlueZ keeps a bonded watch's GATT table and does not rediscover it: a later build's characteristic properties were not seen until the host removed the watch and paired again. BlueZ's per-characteristic `MTU` property stays 23 for the bonded watch; `AcquireWrite` on RX reports the 220 negotiated | bleak 3.0.2 |
| BlueZ grants the battery timing's request: 195 ms, latency 4 | `ble profile battery` on a live link |
| An application image of 167,644 bytes arrives in 24 to 25 s at a 15 ms interval and MTU 220; the executor is never more than 5 ms late while it arrives. The flash phase then holds it for 0.2 and 1.6 s, as over USB | `page` after `wrist-ble.py --update --no-reset` |

## What the schematics settle

| Question | Answer | Status |
|---|---|---|
| Where is the MS5837? | On `SENSORBODY_SCL/SDA` (I2C2, PB13/PB14), behind a TXS0102 level translator, powered by a 3.0 V LDO gated by `ALTIMETER_EN` (**PG0**, sheet 4) | **MEASURED**: with PG0 high it answers at 0x76 (an MS5837-02BA). The image holds PG0 low |
| Display interface | `LCD_R1/R2/G1/G2/B1/B2` plus `LCD_QSPI_*` named signals (`VST`, `HST`, `XRST`, `ENB`, `HCK`, `VCK`) and `FRP`/`XFRP`, through a TXB0108 level shifter on the display module. A memory-LCD parallel interface, not plain SPI. | LIKELY |
| Touch controller | `TOUCH_INT_L` (PC13) and `TOUCH_RST_L` reach the display connector, so a touch-capable module is wired for. | CONFIRMED wiring, UNKNOWN fitted |
| I2C addresses | PCA9420 0x61, MAX17262 0x36, DRV2625 0x5A, STUSB4500 0x29 | CONFIRMED; DRV2625 MEASURED: it answers only once `HAPTIC_RST_N` (PF2) is high, and the image holds PF2 low |
| STUSB4500 (I2C1 0x29) | USB-PD sink controller on the USB-C port | MEASURED: `DEVICE_ID` 0x25 (STUSB4500, not the B cut), answering with VBUS present. Its NVM offers **one sink PDO, 5 V 1 A** (`DPM_SNK_PDO_NUMB` 1; PDO2/3 hold 15 V and 20 V but are not offered). The laptop's USB-C port and a USB-C PD charger both read Rp 3.0 A with **no PD contract** (RDO 0). Our images never write to it. **MEASURED**: on battery, STUSB4500 read failed while PCA9420 CHG_STATUS_1=0x00 (VIN below UVLO); after reconnection, STUSB4500 PORT_STATUS_1=0x41 and PCA9420 CHG_STATUS_1=0xC0. Use PMIC input status as well as PD attachment for power-source detection |
| eMMC power | Behind two NCP335 load switches on `EMMC_EN` (sheet 3: `3V3_EMMC` for VCC, `1V8_EMMC` for VCCQ, 10 kOhm on `EN`); firmware must enable them before the card appears. | CONFIRMED; MEASURED: **the bootloader hands over with the card on** (PD10 output high, SDMMC1 clocked, `POWER` 0x3, `CLKCR` 0x00025000: 4-bit bus, divider bypassed). Card on and idle costs about 3 mA. |
| MCU part | **`STM32U5A5QJI6Q`**, CONFIRMED three ways: BOM Rev 3.2 item 35 (U1, "4MB Flash, 2.5MB RAM, UFBGA132"), the sheet 2 symbol title, and the watch's own `DBGMCU_IDCODE` `DEV_ID` = `0x481`, which OpenOCD's `stm32l4x.c` names `STM32U59/U5Axx`. The ECN 4 note "U1 changed to STM32U595QII6Q" belongs to Rev 2.0 (10/04/25) and was superseded. The watch measured here has a **Rev 3.1** core module; Rev 3.2 differs only by ECN 9, a BOM-only change (`R39` → NOPOP), so every Rev 3.2 wiring claim applies, and `R39` is fitted on Rev 3.1. | CONFIRMED |

## MCU facts a standalone image at `0x08060000` depends on

Pulled from the primary documents, not from the vendor firmware: **RM0456 Rev 7** (the
STM32U5 reference manual) and **DS13543 Rev 3** (the STM32U5Axxx datasheet). Section and
table numbers are given so each line can be re-checked. The part is `STM32U5A5QJI6Q`
(sheet 2's U1 marking), and DS13543 Rev 3's ordering-information key reads that as
132 balls, **4 Mbytes** of flash, UFBGA, industrial, SMPS pinout.

Derived from ST's `STM32U5A5.svd` and `stm32u5a5xx.h` (non-secure aliases,
right because `TZEN` = 0), and re-read in RM0456 Rev 7 Table 6 (bases), Table 79
and 7.9.7/7.9.9/7.9.13 (FLASH), 8.7.1 (ICACHE), 61.7.2 (IWDG), 66.8.6/66.8.15 (USART),
75.12.4 (`DEV_ID` `0x481` = STM32U59x/5Ax), Table 45 and 2.3.2 (SRAM); all CONFIRMED as the code uses them: `RCC` `0x46020C00`; `GPIOB`
`0x42020400`, `GPIOC` `0x42020800`; `USART3` `0x40004800` on APB1 with `USART3EN` =
`RCC_APB1ENR1` bit 18 (`APB1ENR1` at offset `0x9C`), `GPIOBEN`/`GPIOCEN` = `RCC_AHB2ENR1`
bits 1/2 (offset `0x8C`); USART offsets `CR1` 0, `CR2` 4, `CR3` 8, `BRR` 0xC, `ISR` 0x1C,
`TDR` 0x28, `PRESC` 0x2C, `TXFNF` bit 7, `TC` bit 6, `OVER8` bit 15; `FLASH` `0x40022000`
with `NSKEYR` 0x08, `NSSR` 0x20, `NSCR` 0x28, `OPTR` 0x40, `SECHDPCR` 0xC0, `NSCR` reset
`0xC0000000` and bits `PG` 0 / `PER` 1 / `PNB` 10:3 / `BKER` 11 / `BWR` 14 / `STRT` 16 /
`EOPIE` 24 / `LOCK` 31, `NSSR` `BSY` 16 / `WDW` 17 / error mask `0x20FA`; `ICACHE` `0x40030400`
with `EN` bit 0 and `CACHEINV` bit 1; `IWDG` `0x40003000` with `KR` 0 / `PR` 4 / `RLR` 8 /
`SR` 0xC; `DBGMCU_IDCODE` `0xE0044000`, `DEV_ID` bits 11:0; SRAM1 768 K at `0x20000000`,
SRAM2 64 K at `0x200C0000`, SRAM3 832 K at `0x200D0000`, SRAM5 832 K at `0x201A0000`, so the
contiguous block ends at `0x20270000` as the bootloader's range check assumes; the I2C bases
`I2C1` `0x40005400`, `I2C3` `0x46002800`, `I2C4` `0x40008400` and `I2C2` `0x40005800`. The
PCA9420's 7-bit address is `1100001` = `0x61` (datasheet Table 7) on `MAIN_SCL/SDA` =
PB6/PB7 = I2C1.

| Claim | Status |
|---|---|
| `USART3_TX` on PC4 is **AF7** | CONFIRMED — DS13543 Rev 3, Section 4.3, Table 28 ("Alternate function AF0 to AF7"). The PC4 row is empty at AF0/AF1/AF3/AF4/AF5/AF6, carries `I2C6_SMBA` at AF2, and `USART3_TX` at AF7; the AF7 column group header is "LCD/USART1/2/3/6". |
| PC5 is `USART3_RX` at AF7 but carries no net on this board | CONFIRMED both halves: AF7 per ST's GPIO database; sheet 2 draws PC5 at ball **J3** with no net label, and `pins.rs` has no net on it. |
| Post-reset system clock is **MSIS at range 4, nominally 4 MHz** | CONFIRMED — RM0456 Rev 7: `RCC_CR` reset `0x00000035` has `MSISON` = 1 (11.8.1); `RCC_CFGR1` reset `0x00000000` has `SW[1:0]` = 00 = MSIS (11.8.6); `MSIRGSEL` is 0 after reset so the live range is `MSISSRANGE[3:0]` in `RCC_CSR`, whose reset `0x0C004400` gives 0b0100 = range 4 ≈ 4 MHz, and 11.8.50 adds "after a NRST pin or a power-on reset [...] the range is always 4 MHz". |
| Post-reset `HCLK` = `PCLK1` = `SYSCLK` | CONFIRMED — `RCC_CFGR2` reset is `0x00006000` on STM32U59x/5Ax (RM0456 11.8.7); only `DPRE[2:0]` is non-zero, `HPRE[3:0]` and `PPRE1[2:0]` are both 0. |
| USART3's kernel clock post-reset is `PCLK1` | CONFIRMED — `RCC_CCIPR1` reset `0x00000000`, `USART3SEL[1:0]` = 00 = PCLK1 (RM0456 11.8.46); `USART_PRESC` reset 0 does not divide it (66.8.15). So a divisor computed against 4 MHz is right, and `BRR` = `USARTDIV` with `OVER8` = 0 (66.8.6). |
| Whether the bootloader leaves HSI/HSE/PLL1 touched before handoff | **MEASURED**, by the image printing its RCC registers before touching anything, on the dev tool and on battery alike: `RCC_CR` = `0x00003005` (MSIS on and ready, HSI/HSE/PLL1 all off), `RCC_ICSCR1` = `0x048945ED` (**`MSIRGSEL` = 1, `MSISRANGE` = 0, i.e. MSIS at ~48 MHz**; `MSIKRANGE` = 4), `RCC_CFGR1` = `0` (SYSCLK = MSIS), **`RCC_CFGR2` = `0x00000000`** (not the documented reset `0x6000`; no prescalers), `RCC_CSR` = `0x14004400` (`SFTRSTF` and `PINRSTF` set), `RCC_CCIPR1` = `0`, `RCC_PLL1CFGR` = `0`, `FLASH_ACR` = `0x101` (one wait state, prefetch on). So the bootloader hands over on **MSIS range 0, 48 MHz** whatever the power source, and an image assuming 4 MHz transmits at 12x its intended rate: `BRR` 417 at 48 MHz is 115 kbaud. The reset values elsewhere in this table are right as reset values (`STM32U5A5.svd`, RM0456 Rev 7 11.8.1/11.8.2/11.8.6/11.8.7/11.8.46/11.8.50); `RCC_CFGR2` resets to `0x00006000` on STM32U59x/5Ax and `0` on U575/585. |
| Flash is **dual-bank, 8 KiB pages, 256 pages per 2 MB bank**; bank 1 is `0x08000000`–`0x081FFFFF`, bank 2 `0x08200000`–`0x083FFFFF` | CONFIRMED — RM0456 Rev 7, Section 7.3.1 and Table 53 ("Flash module 4-Mbyte dual-bank organization for STM32U59x/5Ax/5Fx/5Gx"). On a 4 Mbyte part dual-bank is the fixed organization; the `DUALBANK` option bit (7.9.13) only applies to the 2 Mbyte and smaller variants. |
| `0x08060000` is **bank 1, page 48**; the kernel region is bank 1 pages 48–255 then bank 2 pages 0–255 | CONFIRMED by arithmetic on Table 53: page N of bank 1 begins at `0x08000000 + N * 0x2000`, and `0x60000 / 0x2000` = 48. |
| `FLASH_OPTR` = `0x1FEFF8AA` decodes as `TZEN` = 0, `DUALBANK` = 1, `SWAP_BANK` = 0, `RDP` = `0xAA` (Level 0) | CONFIRMED — bit positions from RM0456 Rev 7, 7.9.13. `SWAP_BANK` = 0 matters: bank addresses are *not* swapped, so page 48 really is at `0x08060000`. |
| FLASH register offsets from base `0x40022000` (Table 6 names it; non-secure alias): `FLASH_ACR` 0x00, **`FLASH_NSKEYR` 0x08**, `FLASH_SECKEYR` 0x0C, `FLASH_OPTKEYR` 0x10, `FLASH_PDKEY1R` 0x18, `FLASH_PDKEY2R` 0x1C, **`FLASH_NSSR` 0x20**, `FLASH_SECSR` 0x24, **`FLASH_NSCR` 0x28**, `FLASH_SECCR` 0x2C, `FLASH_ECCR` 0x30, `FLASH_OPSR` 0x34, `FLASH_OPTR` 0x40 | CONFIRMED — RM0456 Rev 7, Section 7.9.35, Table 79 ("FLASH register map and reset values"), cross-checked against each register's own section (7.9.2 for `NSKEYR`, 7.9.7 for `NSSR`, 7.9.9 for `NSCR`). **There is no register called `FLASH_CR` on this part**; the nonsecure control register is `FLASH_NSCR` at 0x28, and with `TZEN` = 0 that is the one to use. |
| Which pages hold anything under wrist-rs, and the flash controller's state | **MEASURED** with `flash survey`, on the pages build after a USB update, with the image installed over stock 1.6.1 by the Install glance: bank 1 pages 48–49 (loader), 52–69 (the active image), 116–133 (the DFU slot's previous image) and 181 (embassy-boot state) hold data; **bank 1 182–255 all hold data**, which nothing of ours writes, so the stock kernel's tail is still there. **Bank 2 pages 0–18 hold data** (the stock kernel's spill, also stale), and 128–129 and 132–148 hold the installer's staged copy. Bank 2 19–127 and **196–255 are blank**. `FLASH_ACR` = `0x101` (one wait state, prefetch on), `NSSR` = 0, `NSCR` = `0xC0000000` (`LOCK` and `OPTLOCK`), `OPTR` = `0x1FEFF8AA`, `WRP1AR`/`WRP1BR`/`WRP2AR`/`WRP2BR` = `0xFF00FFFF` each (start page 255 past end page 0: no area protected), `OPSR` = 0, `ECCR` = 0 before and after reading every page. Bank 1 pages 0–47, the vendor bootloader's, were not read. With one wait state, ES0553 2.2.13's flash condition for a hang at low-power entry (prefetch with four or more) does not hold at this clock. |
| A double ECC error raises an NMI the image can return from, and what `FLASH_ECCR` reports for system flash | **MEASURED** with `flash ecc-test`, twice, on the pages build after a USB update. Reading ST's 1-bit test word `0x0BFA1F00` (RM0456 7.3.2) sets `ECCR` = `0x40411F00`: `ECCC`, `SYSF_ECC`, `BK_ECC` = 0, and `ADDR_ECC` = `0x11F00`, the offset from system flash's base `0x0BF90000`; no interrupt, since `ECCIE` = 0. Reading the 2-bit word `0x0BFA1F80` raises the NMI with `ECCR` = `0x80411F80` (`ECCD`, `SYSF_ECC`, offset `0x11F80`). Writing `ECCD` back clears it (`ECCR` then `0x00411F80`: the address stays, the flags clear), and **returning from the NMI resumes the read with no fault, no reset and no crash record**; the console and the face carried on. Both errors were reported again on the second run after `ECCC`/`ECCD` were cleared and the ICACHE invalidated. The image's NMI returns only for a double error in the one page a read has armed: this word during `flash ecc-test`, or the page `flash survey` is reading. Any other NMI still resets. |
| Unlock keys and the page-erase sequence | CONFIRMED — `KEY1` = `0x45670123` then `KEY2` = `0xCDEF89AB` into `FLASH_NSKEYR` clears `FLASH_NSCR.LOCK` (bit 31) (RM0456 7.9.2, 7.9.9). Page erase (7.3.6): wait `NSSR.BSY` (bit 16) clear → clear the error flags in `FLASH_NSSR` → set `NSCR.PER` (bit 1) with `PNB[7:0]` (bits 10:3) and `BKER` (bit 11) → set `NSCR.STRT` (bit 16) → wait `BSY` clear. Programming is **quad-word only**, four consecutive 32-bit writes to one 128-bit-aligned address, with `NSCR.PG` (bit 0) set (7.3.7). |
| The SRAM window the bootloader range-checks the image's initial SP against, `0x20000000`–`0x20270000`, is exactly the contiguous SRAM1+SRAM2+SRAM3+SRAM5 block | CONFIRMED — RM0456 Rev 7, Table 45 ("SRAM structure"): on STM32U59x/5Ax that is 768 + 64 + 832 + 832 KiB = 2496 KiB = `0x270000`, and the Section 2.3.2 memory map shows the block running up to `0x20270000`. Note `0x270000` is **2496 KiB, not 2528 KiB**. |

<a id="boot-handoff"></a>
### Boot handoff: the core state the bootloader leaves behind

The bootloader reaches the image by a branch, not a reset, so the image inherits core state
rather than reset defaults. Rows marked CONFIRMED without a test were read from a stock
firmware image.

| Claim | Status |
|---|---|
| The bootloader accepts an initial SP word from `0x20000000` to `0x20270000` inclusive, and after loading it into MSP pops 16 bytes at and above it, so the SP word must sit at least 16 bytes below the top of SRAM (`SP_MAX` in [`crates/wrist-flash/src/vectors.rs`](crates/wrist-flash/src/vectors.rs), `_stack_start` in `memory.x`) | CONFIRMED |
| The bootloader does not verify the image: the SP word is all it checks, which is why a replacement kernel boots | CONFIRMED |
| The image starts with interrupts masked (`PRIMASK` = 1), possibly on PSP, with `VTOR` already set to `0x08060000`; the loader's `__pre_init` loads MSP from its own vector table, clears `CONTROL` and writes `VTOR` itself | CONFIRMED; LIKELY for PSP |
| NVIC enables, pending bits and SysTick are inherited, and a SysTick is already pending, so the loader sets `PENDSTCLR` and `PENDSVCLR` before enabling interrupts | CONFIRMED by test |
| Firmware update and factory reset live in the bootloader, which draws its own menu, so recovery needs no debugger, no UART and no working kernel | CONFIRMED by test with the kernel erased |
| Factory reset returns to the firmware the watch shipped with, not the previous version, so every update since has to be installed again | CONFIRMED by test |
| A long-press combination in the bootloader rolls the watch back to an earlier firmware version, with no host; `UNA BOOT` carries `UnaWatch-Kernel_1.0.2.gld`, the shipped image it restores | CONFIRMED by test with a working kernel; the file seen on the drive |
| The bootloader's recovery erases from `0x08060000` to the end of flash and nothing below, so [`crates/wrist-flash/src/hw.rs`](crates/wrist-flash/src/hw.rs) keeps its status in a blank page of the bootloader's partition | CONFIRMED |
| An app on the stock kernel can write the FLASH control registers. A key write while the controller was mid-operation faulted, so `hw.rs` writes the keys only while `LOCK` is set | MEASURED (the fault); LIKELY that the busy controller, not a second unlock, caused it |
| A write to `FLASH_NSCR` while `LOCK` is set is discarded with no flag and no fault, so `hw.rs` reads `NSCR` back after each command | CONFIRMED by test |
| `PNB` is `NSCR[10:3]` and `BKER` is bit 11, so a page number of 256 or more silently selects the other bank; `hw.rs` sets `BKER` itself and checks it | CONFIRMED |
| With the instruction cache on, data reads of flash are cached too (RM0456 8.7.1), and embassy-stm32's U5 flash driver does not invalidate it after an erase or a program, so the image invalidates it before reading flash it has written | MEASURED |
| The stock kernel starts every glance service at once when the carousel opens, so nothing destructive can sit behind a glance being shown | CONFIRMED by test |
| The installer is a utility app: its service starts when it is opened from the launcher, before its GUI. On kernel 1.4.0 the glances screen could also start an app of another type, depending on a flag its header carried; the pinned SDK's `app_merging.py` leaves that bit (0x20) clear, and its `Docs/service-lifecycle.md` (sections 3 and 10) says only images packed by an earlier SDK are affected. The service acts only once its GUI is up, and the recipe refuses a header with that bit or autostart | UNKNOWN on the watch: from the SDK's scripts and documentation, not yet tested |
| A utility app's service runs privileged with the MPU and TrustZone off, as a glance's does, so it can write flash | UNKNOWN. Measured only for a glance (`module-probe`, stock 1.5.0: `CONTROL` = `0x6`, `MPU_CTRL` = 0, `TZEN` = 0); that a utility app's service is the same rests on the SDK linking and packing every type's service alike. No utility app has written flash here. The installer's dry run runs the isolation gate and a scratch-page self-test and offers the confirm only if both pass |
| The stock 1.0.2 launcher shows two utility apps at most, while every glance is reachable from the carousel. How many it shows on 1.6.1 is not known, nor whether the Wrist-rs app appears beside others | CONFIRMED on 1.0.2 (2026-10-05: `FW Dump` registered by `App.Manager` but unreachable); UNKNOWN on 1.6.1, not in UNA's documentation or release notes |

## DMA channels the image uses

GPDMA1 channels 0 and 1 carry I2C1's transmit and receive: embassy-stm32 0.6's async I2C
needs a channel each way, and the driver sets the request lines. Nothing else in the image
uses DMA. The vendored HAL indexes GPDMA channels through its controller-aware accessor:
casting GPDMA to the four-channel LPDMA type panics for channels 4 and 5
([`vendor/embassy-stm32/WRIST-PATCHES.md`](vendor/embassy-stm32/WRIST-PATCHES.md)).

The firmware also takes channels 12 to 15, through registers and with
no interrupt, with TIM1 to TIM4, to write the panel's bus
([`apps/standalone-boot/src/panel_dma.rs`](apps/standalone-boot/src/panel_dma.rs)).

| Claim | Status |
|---|---|
| A GPDMA burst of two to four beats to TIM3 never completes: the channel stays enabled with its FIFO full (`FIFOL` 3), on either port, with or without a hardware request on the destination, block repeat or a list, on `tim3_cc1/cc4/upd_dma`. Single beats reach TIM3's registers | MEASURED, `page bus test` on a diagnostic build |
| TIM3's own DMA burst works: `DCR` = `(5 << 16) \| (2 << 8) \| 13` (on compare 4, three transfers from `CCR1`) with single beats to `DMAR` writes `CCR1` to `CCR3` and leaves `CCR4` | MEASURED, same build |
| That three-write burst takes about 29 timer cycles at 48 MHz from compare 4: 999 of 999 repeated blocks completed at a period of 29 cycles or more, 876 at 28, 669 at 25 | MEASURED, same build, no other channel running |
| A single-beat GPDMA write lands 11 to 13 timer cycles after its TIM3 request, on either port, a list load adding about one; the same at 64-, 28- and 24-cycle ticks with both channels running | MEASURED, `page bus probe` |
| `E`'s half-word writes to `GPIOE_ODR` reach the pins 3 to 9 timer cycles after TIM3's update: `R1` (`PE0`) and `BCK` stamped through EXTI 0 and 6 and two GPDMA channels, `BCK`'s known compare taking out the path (9 cycles, constant) | MEASURED, `page bus pins` |
| At a 26-cycle tick, `D` reading each compare's source on its request failed 11 of 113 frames with its list and sources in SRAM5, 4 of 192 in SRAM2, and 1 of 266 in SRAM5 with interrupts masked during frames; `D` reading its sources ahead failed 0 of 259 in SRAM5 and 0 of 261 in SRAM2 | MEASURED, `page bus` on a diagnostic build |
| `tim3_cc1_dma` fires every tick, not only on toggles: with `CCR1` above `ARR`, `CC1IF` is set at the overflow (RM0456 55.5.6). A channel counting it counted ticks | MEASURED, a diagnostic build |

## USB device (OTG_HS)

| Claim | Status |
|---|---|
| A standalone image can enumerate on `USB_D_N/P` (PA11/PA12) with no vendor code | **MEASURED**: `standalone-boot` with `embassy-stm32` 0.6.0's OTG driver and `embassy-usb` CDC-ACM enumerates as 1209:D1A1 (`/dev/ttyACM…` on Linux, `/dev/cu.usbmodem…` on macOS), **Device Speed 2 = high speed** (`DSTS` = `0x0030CC00`, `ENUMSPD` = 00), address 1 (`DCFG` = `0x10`). Commands sent from the host are answered. |
| The OTG_HS block needs AHB above 30 MHz | CONFIRMED, RM0456 Rev 7 Section 73.3 (caution under the block description): "the AHB frequency must be higher than 30 MHz". The image runs the core at MSIS range 0 (48 MHz) from `embassy_stm32::init` on; the panel driver's cycle constant and the UART divisor follow. |
| The OTG_HS PHY reference is the 16 MHz crystal, selected by `OTGHSSEL`; no 48 MHz USB kernel clock exists on this part | CONFIRMED, RM0456 11.4.19: the kernel clock is generated by the PHY from HSE, HSE/2, PLL1_P or PLL1_P/2, at 16/19.2/20/24/26/32 MHz. `SYSCFG_OTGHSPHYCR` reads `0x0D` running: `EN` = 1, `CLKSEL` = 0b0011 = 16 MHz, `PDCTRL` = 0. `ICLKSEL` (RM0456 11.8.46) feeds OTG_FS/USB/SDMMC on other U5 parts and is irrelevant here. |
| **The OTG_HS EPOD booster is clocked from the PLL1 input through `PLL1MBOOST`, so PLL1 must be configured even when the system clock is MSIS** | **MEASURED**: with `RCC_PLL1CFGR` = 0 the booster never reports ready, and embassy's unbounded wait for `USBBOOSTRDY` starves the executor until the IWDG resets the part, every 32 s. RM0456 10.7.12 step 3b: "Make sure the EPOD booster clock is enabled (using `PLL1MBOOST[3:0]`)". The image runs PLL1 from HSE (16 MHz, ×10, R/2 = 80 MHz unused); embassy programs `PLL1MBOOST` only inside a PLL1 configuration whose R output is at least 55 MHz. `PWR_VOSR` reads `0x001FE000` running: `USBBOOSTEN`, `USBPWREN`, `BOOSTEN`, `VOS` = range 1, `VOSRDY`, `BOOSTRDY`, `USBBOOSTRDY` all set. |
| `PWR_SVMSR` is at offset `0x3C`; `VDDUSBRDY` is its bit 24 | CONFIRMED, RM0456 10.10.16 and `stm32-metapac` 21.0.0; offset `0x14` is `PWR_WUCR1`. Running value `0x05038002`: `VDDUSBRDY` set, and `REGS` (bit 1) set, so the core runs from the SMPS, not the LDO. `PWR_CR1` reads `0`: `ULPMEN` is clear, and nothing sets it on U5. |
| The supply sequence of RM0456 10.7.12 (UVM, `VDDUSBRDY`, `USV`, `USBPWREN` + `USBBOOSTEN`, `USBBOOSTRDY`) completes within a few ms on this board | MEASURED: the image's bounded pre-check passes before embassy's own unbounded waits run. |
| The PHY tuner step of RM0456 15.2.3 (`COMPDISTUNE` = 0b010, `SQRXTUNE` = 0b000 in `SYSCFG_OTGHSPHYTUNER2`) | APPLIED by the image; `0x81CD0682` running. embassy does not write it. Not shown to be necessary. |
| `vbus_detection` off is right for this board | CONFIRMED by use: nothing tells the MCU about VBUS directly (no net on `OTG_HS_VBUS`), so the device enumerates whenever a host is present; `GCCFG` = `0x01800000` running (`VBVALOVEN`, `VBVALOVAL`). On battery the image powers the block, its supplies, the crystal and PLL1 down, and brings them back when external power returns ("What the image turns off"). |
| **The UNA Dev Tool 2.0 does not pass USB data through**; the data cable is the USB path for the image | MEASURED: on the tool the Mac enumerates only the tool's FT231X; on a data cable the watch enumerates. The stock kernel's "charge mode on a data cable" is kernel behaviour, not hardware: the standalone image runs normally on the data cable. |
| macOS asks "Allow accessory to connect?" for a new USB device | OBSERVED; until it is allowed the console does not appear. |
| The stock kernel also exposes a CDC console on the data cable, prompt `una> ` with ANSI colour | OBSERVED by the host logger opening `/dev/cu.usbmodem2061317256412` while stock was running. Not used; noted so a stray `una>` in a log is not mistaken for this image. |
| **The device task must never be starved for more than a few milliseconds**: a host's `SET_ADDRESS` status stage and its retries are over in tens of ms, and a reset it then sends while `embassy-usb` 0.6 is inside a control transfer is never serviced, since the control path has no bus poll beside it | **MEASURED** (the watch's own log on the dev tool): with synchronous UART output at 9600 baud (1 ms/byte; three 2.5 KiB reports in the first 10 s), `SET_ADDRESS` goes unanswered, then `GINTSTS` holds `USBRST` + `ENUMDNE` pending for hours with `DCFG.DAD` = 1, and no re-plug clears it. So UART output is interrupt-driven, the panel frame yields per line, and the console service rebuilds the link if a reset sits unserviced for 500 ms. |
| `embassy-usb-synopsys-otg` 0.3.3 (what `embassy-stm32` 0.6.0 pins) reports a stale `USBSUSP` after a reset as a Suspend event, which parks `embassy-usb` in `wait_resume` where SETUP packets are ignored until the next reset or resume | CONFIRMED in source; the driver on `main` (0.4.0) clears `USBSUSP` when it services `USBRST`/`ENUMDNE`. Seen in the watch's log ("reset" then "suspend" at the same ms). With the executor responsive the flag is not stale, and the service task's rebuild covers the rest. |

## Power and reset domain (sheets 5 and 6, PCA9420 datasheet Rev 4.2)

| Claim | Status |
|---|---|
| **The MCU's `NRST` is driven by the PCA9420's `SYSRST` pin** (`U8` D3 → net `RST_L` → `U1` G2), with `C23` 100 nF and no external pull-up on the core board | CONFIRMED, sheets 5 and 2. PCA9420 §8.9: `SYSRSTn` is open-drain, asserted low whenever **any enabled rail is below 90 % or above 110 % of target**, during power-up/down sequencing, on **watchdog expiry** (§8.11), and on an **ON-pin long press** (§8.5, `ON_GLT_LONG` default 01 = 8 s). It does not assert for commanded voltage changes or mode switches. |
| Rails: `SW1_OUT` → `1V0` (modules), `SW2_OUT` → **`1V8` = MCU `VDD`, `VDDIO2`, `VDDSMPS`**, `LDO1_OUT` → `1V8_RTC` = MCU `VBAT`, `LDO2_OUT` → **`3V3`** = `VDDA`, `VDDUSB`, `VREF+`; `VIN` = `5V0_USB`; `ASYS` = `VSYS`; `VBAT_BKUP` tied to `VBAT` | CONFIRMED, sheets 5 and 2; the part is `PCA9420UKZ` (UNAcore BOM, `U9`). The datasheet's defaults are 1.0 V / 1.8 V / 1.8 V / 3.3 V (Tables 45 to 51), each "MTP programmable", but **this watch's mode 0 reads 1.2 V / 1.8 V / 1.8 V / 3.2 V** (`MODECFG_0_0..3` `1C 0C 4F 34`, below) when this project's image starts, after the vendor bootloader. Whether the part's programmed defaults or the bootloader set them is not known; either way `1V0` and `3V3` are names, not voltages. The MCU's own supplies (`1V8`) match; a standalone image need not reprogram any rail to keep the MCU up. The MCU core (`VDD11`) comes from its own SMPS (`L1`, 1V1). |
| **PC4 is in the 1.8 V I/O domain** | CONFIRMED: `VDD` and `VDDIO2` are both `1V8`; only `VDDUSB` (PA11/PA12) and `VDDA` are 3.3 V. Matches the dev tool's 1.8 V RX. |
| `MODESEL0` and `MODESEL1` are tied together and pulled to GND by `R13` 100 kΩ | CONFIRMED, sheet 5 — pin mode selection can only ever choose mode 0; mode switching is by I2C (`TOP_CNTL3` bits [4:3]). |
| The power button `SW2` is the PMIC `ON` pin (`PWR_ON_L`), level-shifted by `U10` 74LVC1T45 to `PWR_ON_1V8_L` on PD15; `INTB` is `PWR_INT_L` on PG7 | CONFIRMED, sheets 5 and 2. **`SW2` is the button the vendor calls Select**: an image polling only PD4/PA5/PD8 does not see it, and a long hold reboots the watch through the PCA9420's `ON_GLT_LONG` = 16 s hard reset, after which the bootloader, still seeing the button, enters its USB mode. So **the bootloader is always reachable from any image: hold Select 16 s, keep holding**. The image also watches PD15: a 3 s hold resets into the bootloader, a short press lights the face. What the PCA9420 itself does with a short press of `ON` while the rails are up (an interrupt on `PWR_INT_L`, or nothing) is not known. |
| **PCA9420 register `0x0A` is `TOP_CNTL1` and contains no watchdog bit** | CONFIRMED (datasheet Table 18): bits [7:6] `ASYS_PREWARNING`, [5:4] `ASYS_INPUT_SEL`, bit 3 reserved (default 1), bit 2 `VIN_OVP_SEL`, **bits [1:0] `VIN_UVLO_SEL`** (00 = 2.9 V, 01 = 3.1 V, 10 = 3.3 V, 11 = 3.5 V). Reset value `0x89`; the kernel leaves it at `0x82` (VIN UVLO 3.3 V, reserved bit cleared). The kernel's `setI2cWatchdog` RMW of bits [1:0] therefore programs the VIN under-voltage lockout, whatever the vendor calls it. |
| **The real PCA9420 watchdog**: `WD_TIMER_x[1:0]` = bits [7:6] of `MODECFG_x_3` (`0x25`/`0x29`/`0x2D`/`0x31`), 00 disabled (reset default), 01 = 16 s, 10 = 32 s, 11 = 64 s (±15 %); fed by writing `001` to `WD_TIMER_CLR[2:0]` in `TOP_CNTL4` (`0x0D`, write-only) or by any mode change; on expiry `SYSRSTn` asserts and all rails revert to mode-0 settings, charging continuing (`TOP_CNTL0` bit 2 `nEN_CHG_IN_WATCHDOG` = 0) or suspended (= 1); `0x71` bit 1 `WD_RST_DONE` records it | CONFIRMED from the datasheet (§8.11, Tables 17, 21, 50, 76). Its shortest period is 16 s. The kernel leaves it disabled in every mode (Power-on state, below). |
| Power-source dependence inside the PMIC | What changes with VIN present: `ASYS` follows VIN instead of VBAT, the charger state machine runs, the ON-pin pull-up target changes, and `VIN_ILIM` (default 425 mA) can clamp `VSYS`. None of these produce a periodic reset by themselves; a rail droop below 90 % under VIN current limiting would, but only under load. |
| `3V3_USB` exists only with VBUS (`U23` TPS7A0233, enabled by `5V0_USB`); it powers `U21`/`U22`, the NX20P0407 CC/SBU protection switches | CONFIRMED, sheet 6 |
| **`BACKLIGHT_ON` (PF3) is active low**: it drives the gate of `Q1`, a P-channel FET on the display board, through nothing; `R1` 10 kΩ pulls the gate to 3V3 so a released (open-drain high) pin is off, a driven-low pin lights the LED through `R2` 82 Ω | CONFIRMED (UNAview_LS012 sheet 2, and measured). An image must drive it open-drain and leave it released, or the light stays on and the idle floor with it. |
| MAX17262 fuel gauge (0x36, I2C1): `VCell` 0x09 at 78.125 µV/LSB, `RepSOC` 0x06 at 1/256 %, `Temp` 0x08 at 1/256 °C, `Current` 0x0A and `AvgCurrent` 0x0B signed at **156.25 µA/LSB** on its internal sense resistor, positive into the battery | CONFIRMED (scaling per Zephyr's `max17262` driver, matching the readings on a watch). 156 µA steps resolve activity, not a microamp idle floor; that needs a meter in series with the cell. `TTE` 0x11 is the gauge's time to empty at 5.625 s/LSB, `0xFFFF` when it has no estimate (datasheet) |
| MAX17262 hibernate, capacity and offset: `HibCfg` 0xBA reads the default `0x870C` (ADI UG6597: hibernate below about C/100, a reading every 5.6 s, about 5 µA); the image sets `HibScalar` to 7 at boot (`0x870F`, read back on the watch, a reading every 45 s; its current not measured), which stays after a return to stock until the gauge loses power, and `Status2` 0xB0 bit 1 (`Hib`) is set even while charging at about 12 mA (`0x3002`). `DesignCap` 0x18 is 200 mAh and the learned `FullCapRep` 0x10 201 to 202 mAh, against the 280 mAh UNA announced. The current's zero offset is ±1 mA typical (datasheet `IOERR`), and there is no `QL`, only `QH` 0x4D at 0.5 mAh: it cannot measure this watch's draw. As the stock firmware leaves it (`power`, at boot): `Status` `0x8080` (`POR` clear; `Br`, set at power-up, and `dSOCi`, which stock never clears), `DesignCap` `0x0190` (200 mAh), `IChgTerm` `0x0059` (13.9 mA), `VEmpty` `0xAA63` (empty 3.40 V, recovery 3.96 V), `ModelCfg` `0x0400` (`VChg`: charged above 4.275 V). A gauge reset loses all of these and the learned values; whenever a check, at boot or with a reading, finds `POR` set, the image writes the last four back and clears `POR` | MEASURED for the registers; offset and register map from the datasheet. `power` counts the battery readings that find it in hibernate |

## Power-on state

Read from the PCA9420 under the stock kernel, decoded with datasheet Rev 4.2:

| Register | Value | Meaning |
|---|---|---|
| `TOP_CNTL0` 0x09 | `0x21` | `VIN_ILIM_SEL` 001 = 255 mA; `OPERATION_SEL_FROM_SHIPMODE` 0; `PWR_DN_EN` 0; **`nEN_CHG_IN_WATCHDOG` 0**; `PGood_EN` 1 |
| `TOP_CNTL1` 0x0A | `0x82` | `ASYS_PREWARNING` 3.5 V; `ASYS_INPUT_SEL` 00 (VIN or VBAT); reserved bit 3 cleared; `VIN_OVP_SEL` 5.5 V; **`VIN_UVLO_SEL` 3.3 V** |
| `TOP_CNTL2` 0x0B | `0xCC` | `ASYS_UVLO_SEL` 2.7 V; `THEM_SHDN` 110 °C; die-temp warning 75 °C |
| `TOP_CNTL3` 0x0C | `0x03` | mode 0 selected by I2C; `SW_RST` 0; **`ON_GLT_LONG` 11 = 16 s** power-button hard reset |
| `MODECFG_x_3` 0x25/0x29/0x2D/0x31 | `0x34` each | **`WD_TIMER_x` = 00, watchdog disabled in every mode**; LDO2 = 2.0 V + the 1.2 V offset = **3.2 V** (the datasheet's default is 3.3 V) |
| `MODECFG_0_0..2` 0x22–0x24 | `1C 0C 4F` | Mode 0, in force: SW1 **1.2 V**, SW2 1.8 V, LDO1 1.8 V, all four regulators on, pins ignored, not ship. Read again under this project's image, which never writes them, after the vendor bootloader |
| `MODECFG_1_*`, `MODECFG_3_*` | `1C 4C 4F 34` | As mode 0, with `ON_CFG` set (a press of `ON` keeps the mode) |
| `MODECFG_2_*` 0x2A–0x2D | `5C 4C 42 34` | Mode 2: **only LDO1 (`1V8_RTC`) on**, chosen by the `MODESEL` pins: a holding state with everything but the RTC rail off. Nothing here selects it |
| `REG_STATUS` 0x20 | `0xF0` | SW1, SW2, LDO1, LDO2 all power-good |

So under the vendor kernel no PCA9420 watchdog runs, and a standalone image inherits a
PMIC that will not reset it on its own. The same reading shows the kernel holding **PB15 as an
output driven high** with the dev tool attached (`GPIOB MODER 0x6816AAA5`, `ODR` bit 15
set, pin reads 1) and PC4 in alternate function 7.

## Power-source detection

Nothing tells the MCU about VBUS directly. The image reads two chips on I2C1: the PCA9420's
`CHG_STATUS_1` (0x19) bits 7:6, the charger's input state, and the STUSB4500's
`PORT_STATUS_1` (0x0E) bit 0, `ATTACH`. **MEASURED**: on battery the PMIC read
`00` and the PD read failed; after reconnection they read `C0` and `41`. So a failed PD read
alone means neither USB nor battery; `wrist_power::external_power` decides from the PMIC's
status and treats an attached PD source as external power, and its missing-read combinations
are host-tested.

| Claim | Status |
|---|---|
| `PWR_INT_L` (PG7) is the PCA9420's `INTB` (D4) and the STUSB4500's `ALERT` (B2), both open-drain, with no pull-up fitted; the STUSB4500 is powered only from the cable | CONFIRMED, sheets 2, 5 and 6 |
| The PCA9420 latches `IN_PWR` (`SUB_INT0` bit 0) whenever input power turns valid or invalid, and pulls `INTB` low until it is cleared by writing 1; `SUB_INT0_MASK` bit 0 masks it, and masks it by default | CONFIRMED, PCA9420 datasheet Rev 4.2, Tables 10 to 12 |

So the image unmasks only `IN_PWR` at boot and pulls PG7 up itself; it samples the input at
boot, when the line falls, once a second while something holds it low, and once a minute.
**MEASURED**: over 79 minutes on battery, `input interrupts` rose by two, the
unplug and the plug, and `samples` by one a minute.

## Retained failure record

The top two unused SRAM words, `0x2026FFF8` and `0x2026FFFC`, keep a tagged panic line and
file hash, or a HardFault's `CFSR` and `PC`, across a software reset. They sit above the
loader's RCC mailbox (`0x2026FFF0`/`0x2026FFF4`) and the stack's start (`0x2026FFE0`).
Neither flash nor the backup registers are written. `power` reports the record and `power
sleep clear-crash` clears it; nothing survives a complete power loss. [`tools/wrist-crash.py`](tools/wrist-crash.py)
resolves a panic's hash using the source paths in the exact failing image. **MEASURED**:
it found the DMA interrupt's bounds-check panic. The image clears `RCC_CSR`'s reset flags
(`RMVF`) after reporting them, so each boot's `reset flags` names only its own cause.

## Stop modes

LPTIM4 is the time driver, counting the 32768 Hz LSE in bypass, with the backup domain kept
as the bootloader configured it; it runs and wakes the core in Stop2 (MEASURED). A U5
compare write needs a fresh `CMPOK` acknowledgement, not a stale flag, and the counter
snapshot has to account for an overflow not yet serviced; without both, a timer can stall
for a whole 2 s period ([`vendor/embassy-stm32/WRIST-PATCHES.md`](vendor/embassy-stm32/WRIST-PATCHES.md)). The acknowledgement takes
a few counter clocks, which the driver waits out with interrupts masked: through the /32
prescaler (1,024 Hz) each wait takes about 3 ms and stalls the executor for up to 22 ms
(MEASURED), so the counter runs undivided and its overflow wakes the core every 2 s.

The buttons are EXTI lines: PD4/EXTI4 Up, PA5/EXTI5 Down, PD8/EXTI8 Back and PD15/EXTI15
Select. On battery the image uses Stop2. With USB or an unknown input its own guard holds
the HAL's Stop1 refcount, so the core enters no Stop mode and waits in Sleep, which keeps
the USB device answering (MEASURED: no Stop exit on USB, every wait a Sleep
under one Stop1 hold). The HAL's blanket idle clock-enable lock, which would keep the core
in Stop1 for good, is dropped on U5, whose peripheral registers are retained in Stop2 (ST
AN5371, AN5652); active transfers keep their guards, and raw debug-UART output holds Stop1
until it drains. **MEASURED**: on battery the image wakes about 40 times a minute (the 2 s
overflow, the watchdog's 8 s feed, the minute's redraw and the face's timers; 39.2 worn for
71 min with 17 presses), spends about 99.7 % of the time in Stop2, and restores its clocks
in 0.12 ms an exit. A lit face wakes the core only to turn its light off: sampling the
buttons every 50 ms while lit adds about 60 wakes a press.

Every Stop exit on U5 re-runs the HAL's whole clock initialisation from a saved
configuration: HSE and the PLLs stop in any Stop mode and come back only that way.

**MEASURED**: a write to `LPTIM4_DIER` takes effect only when `DIEROK` sets, about 70 to
200 us later, and entering Stop 2 with a `CCIE` enable still in flight loses compare
wakeups: the task runs when the counter next reaches the same value, one 2 s period late.
Upstream's driver, which does not wait, runs alarms up to 2000.4 ms late on battery (687
times over 5 ms in 23 min under the latency probe), and a short press's light comes on that
late. A second `DIER` write before `DIEROK`, which RM0456 58.7.7 forbids, is not what loses
them. ES0553 2.12.3 says changing a `DIER` enable clears its `ISR` flag. So the driver
skips unchanged writes, waits for `DIEROK` after each change (54 us a Stop 2 wake on
average), writes `CCIE` once per alarm, and handles at the overflow
an alarm found already due there: alarms run at most 4.0 ms late on battery, none over 5 ms
in 26 min.

The driver also enables `CCIE` for any alarm due before the next overflow, as upstream
embassy `fb82051b5` does. Enabling it only for alarms under `0xC000` counts (1.5 s) ahead
leaves one in the last quarter of the next period to run an overflow late, up to 500 ms
(MEASURED: 381 ms).

If the backup domain does not show the LSE running at boot, the image starts it in bypass
and the RTC itself before the HAL copies the domain (`rtc::prepare`); with no ready LSE the
LPTIM4 time driver and LPTIM1 run from the LSI. Not seen on a watch.

## What the image turns off

At boot ([`apps/standalone-boot/src/park.rs`](apps/standalone-boot/src/park.rs)); **MEASURED** in place by the report, the current it saves not yet:

- **The eMMC.** The bootloader hands over with it powered and SDMMC1 clocked (about 3 mA,
  above). SDMMC1 is powered off and its clock gated, its six lines set to analog so the
  card cannot be back-powered through its I/O, and `EMMC_EN` (PD10) driven low.
- **Every module but the display.** Held low: `BLE_RST_L` (PA8), `GNSS_EN` (PG5) and
  `WAKE_HOST_TO_GNSS` (PD1), `SENSORBODY_RST_L` (PF13) and the body slot's CS (PG12),
  `ALTIMETER_EN` (PG0), `HAPTIC_RST_N` (PF2), `BUZZER_EN1/2` (PB9, PD5). Each is a
  core-board enable or a slot's active-low reset, so low is safe whatever module is fitted.
  The BMI270 and BMM350 sit on the GNSS module's own supply whatever `GNSS_EN` says; each
  gets a soft reset (`CMD` 0x7E = 0xB6) into suspend, only when the slot's EEPROM names the
  expected module.
- **The unused lines.** The slots' buses and interrupts, `BUZZER_PWM`, the touch lines, and
  the gauge's and USB switch's open-drain alerts: with the eMMC's six, 35 pins set
  to analog, no pull. I2C4's pair follows once the boot's module checks are done.
- **RTC alarm A**, disarmed in case an earlier image armed it.
- **SRAM the image does not use**, powered down in the Stop modes through `PWR_CR2` and
  `PWR_CR4`: SRAM1 above the pages `.data` and `.bss` fill, SRAM2, SRAM3, SRAM4, SRAM5
  below its top page, and the DCACHE1, DMA2D and PKA RAMs. The linker's RAM is SRAM1; the
  stack shares SRAM5's top page with the loader's mailbox and the failure record. An update
  is staged in SRAM3's first 512 KB, kept through Stop only while a transfer holds it.

On battery ([`apps/standalone-boot/src/power.rs`](apps/standalone-boot/src/power.rs) and [`console.rs`](apps/standalone-boot/src/console.rs)), also not yet measured:

- **USB**, with its supplies (`USBPWREN`, `USBBOOSTEN`, `USV`), the 16 MHz crystal and
  PLL1, which only the USB PHY uses. The PMIC's input interrupt on `PWR_INT_L` brings them
  back when a cable returns.
- **`DAM_DETECT` (PB15)**, driven low: the debug path it enables is powered from VBUS, so on
  battery its 460 kΩ pull-down would only draw current.
- **The UART's queued output**, dropped: nobody can hear it.

What stays on is what the face needs: the panel and its light, the buttons, I2C1 to the
PMIC, gauge and USB-PD controller, the RTC and LPTIM1, LPTIM3 and LPTIM4 on the LSE, the IWDG, and SWD.
