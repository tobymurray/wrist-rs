# wrist-rs 0.3.1

| | |
|---|---|
| **Stock firmware** | **1.6.2** only. The installer runs on no other version. |
| **Watch** | UNA Watch with its original modules and LS012 display |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's
charge, with pages for the battery and the build, in place of the stock firmware.
`manifest.json`, in the release zip, names this build's version and source commit.

## In this release

Fixes on 0.3.0. New since 0.3.0:

- The watch no longer restarts by itself when its timer driver gives up waiting on the
  hardware, as 0.3.0 and 0.2.0 rarely did: it makes the write once more and, if the
  hardware still does not answer, leaves that wakeup to the next one, 2 s later at most.
- The watch selects its own power regulator before it sleeps, rather than relying on the
  stock bootloader's choice: a restart in deep sleep on the other regulator can leave the
  processor stuck, drawing tens of mA until the battery runs down (ST erratum ES0553
  2.2.10). Nothing changes on a watch that already ran from the right one.
- [`tools/wrist-update.py`](../tools/wrist-update.py) and the other tools quiet the console
  before their first command: the computer can echo the watch's greeting back, and the
  watch then read `update` glued to it and refused the install.
- The loader is unchanged: no loader update is needed.

From earlier releases:

- The face and pages in Atkinson Hyperlegible Next. Digits that do not change stay where
  they are when the minute turns.
- Drawing is faster: a page change reaches the glass in about 50 ms, not about 300 ms, and
  the panel is written by timers and DMA while the processor sleeps.
- The installer is the **Wrist-rs** app, opened from the launcher, in place of the
  `Install` glance: its dry run shows the image's CRC on screen, and holding Select 3 s
  installs ([install guide](INSTALL.md)).
- The console's `flash survey` reports which flash pages hold anything, without writing.
- The face, lit by a short press of any button, with pages for the battery and the build:
  Down steps from the face to the battery, then to about, then back to the face; Up steps
  the other way, and Back or 10 s without a press returns to the face.
- Time kept across restarts and updates, set from a computer with
  [`tools/wrist-time.py`](../tools/wrist-time.py), which also sends the zone's next two
  daylight-saving changes for the watch to apply.
- Updates over USB with [`tools/wrist-update.py`](../tools/wrist-update.py), into a second
  slot, with rollback.
- Everything the face does not use turned off at start, and deep sleep on battery.
- The way back to stock through UNA's own recovery, in the [install guide](INSTALL.md).

## Checked on the watch

To be written from this release's gate run ([GATE.md](GATE.md), step 22).

## Limits

- The original modules and LS012 display only.
- The watch follows daylight saving for the two changes
  [`tools/wrist-time.py`](../tools/wrist-time.py) last sent, about a year. After that,
  after a change of zone, or after a 16-second hold of Select, which resets the power and clears the zone with
  the clock, run the tool again. It knows no zone names.
- A short press lights the face for 3 s, or turns off a light switched on from the console.
  Back and Select only light it; Up and Down also move between pages.
- After a cable is pulled during an update, the next update can get no answer from the
  watch. Run it again.
- No stock apps, notifications, sensors, Bluetooth or activity storage.
