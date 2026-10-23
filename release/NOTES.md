# wrist-rs 0.1.0

| | |
|---|---|
| **Stock firmware** | **1.6.1** only. The installer runs on no other version. |
| **Watch** | UNA Watch with its original modules and LS012 display |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's
charge, in place of the stock firmware. `manifest.json`, in the release zip, names this
build's version and source commit.

## In this release

- The face, lit by a short press of any button.
- Time kept across restarts and updates, set from a computer with
  [`tools/wrist-time.py`](../tools/wrist-time.py), which also sends the zone's next two
  daylight-saving changes for the watch to apply.
- Updates over USB with [`tools/wrist-update.py`](../tools/wrist-update.py), into a second
  slot, with rollback.
- Everything the face does not use turned off at start, and deep sleep on battery.
- The first install from stock firmware 1.6.1 with the `Install` app and a confirm on its
  screen, and the way back
  through UNA's own recovery, both in the [install guide](INSTALL.md).

## Checked on the watch

On stock 1.6.1, with this build's package: the dry run, the install, and the way back to
stock and in again; updates over USB, including a pulled cable and a killed tool, which
leave the watch on its current version; the rollback of an image no computer keeps;
the clock across restarts and daylight-saving changes; every button; and deep sleep on
battery, at about 40 wakes a minute.

## Limits

- The original modules and LS012 display only.
- The watch follows daylight saving for the two changes
  [`tools/wrist-time.py`](../tools/wrist-time.py) last sent, about a year. After that,
  after a change of zone, or after a 16-second hold of Select, which clears the zone with
  the clock, run the tool again. It knows no zone names.
- A short press lights the face for 3 s, or turns off a light switched on from the console.
- No stock apps, notifications, sensors, Bluetooth or activity storage.
