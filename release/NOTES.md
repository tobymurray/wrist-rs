# wrist-rs 0.3.0

| | |
|---|---|
| **Stock firmware** | **1.6.2** only. The installer runs on no other version. |
| **Watch** | UNA Watch with its original modules and LS012 display |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's
charge, with pages for the battery and the build, in place of the stock firmware.
`manifest.json`, in the release zip, names this build's version and source commit.

## In this release

New since 0.2.0:

- The face and pages in Atkinson Hyperlegible Next. Digits that do not change stay where
  they are when the minute turns.
- Drawing is faster: a page change reaches the glass in about 50 ms, not about 300 ms, and
  the panel is written by timers and DMA while the processor sleeps.
- The installer is the **Wrist-rs** app, opened from the launcher, in place of the
  `Install` glance: its dry run shows the image's CRC on screen, and holding Select 3 s
  installs ([install guide](INSTALL.md)).
- The console's `flash survey` reports which flash pages hold anything, without writing.
- The loader is unchanged: no loader update is needed.

From earlier releases:

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
  after a change of zone, or after a 16-second hold of Select, which clears the zone with
  the clock, run the tool again. It knows no zone names.
- A short press lights the face for 3 s, or turns off a light switched on from the console.
  Back and Select only light it; Up and Down also move between pages.
- After a cable is pulled during an update, the next update can get no answer from the
  watch. Run it again.
- No stock apps, notifications, sensors, Bluetooth or activity storage.
