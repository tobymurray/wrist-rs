# wrist-rs 0.2.0

| | |
|---|---|
| **Stock firmware** | **1.6.1** only. The installer runs on no other version. |
| **Watch** | UNA Watch with its original modules and LS012 display |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's
charge, with pages for the battery and the build, in place of the stock firmware.
`manifest.json`, in the release zip, names this build's version and source commit.

## In this release

New since 0.1.0:

- Pages: Down steps from the face to the battery (charge, cell voltage, and whether it runs
  from USB or the cell), then to about (the version and build ID), then back to the face. Up
  steps the other way, Back returns to the face, and so do 10 s without a press.
- The console no longer cuts the end off a line now and then, once every 8 KB of output.
- The loader is unchanged: no loader update is needed.

From 0.1.0:

- The face, lit by a short press of any button.
- Time kept across restarts and updates, set from a computer with
  [`tools/wrist-time.py`](../tools/wrist-time.py), which also sends the zone's next two
  daylight-saving changes for the watch to apply.
- Updates over USB with [`tools/wrist-update.py`](../tools/wrist-update.py), into a second
  slot, with rollback.
- Everything the face does not use turned off at start, and deep sleep on battery.
- The first install from stock firmware 1.6.1 with the `Install` glance, and the way back
  through UNA's own recovery, both in the [install guide](INSTALL.md).

## Checked on the watch

With this build's package, updating from 0.1.0: updates over USB, including a pulled cable
and a killed tool, which leave the watch on its current version; the rollback of an image
no computer keeps; the clock across restarts and daylight-saving changes; every button and
every page; and deep sleep on battery.

The first install from stock firmware 1.6.1, and the way back, are 0.1.0's: the installer
and the loader are byte for byte the ones that round trip ran.

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
