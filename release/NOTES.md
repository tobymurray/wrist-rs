# wrist-rs 0.4.0

| | |
|---|---|
| **Stock firmware** | **1.6.2** only. The installer runs on no other version. |
| **Watch** | UNA Watch |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's charge, with a daily alarm and pages for the battery and the build, in place of the stock firmware. `manifest.json`, in the release zip, names this build's version and source commit.

## In this release

New since 0.3.1:

- A daily alarm. Set it from the computer with [`tools/wrist-time.py`](../tools/wrist-time.py) `--alarm HH:MM`, or `alarm HH:MM` on the console (24-hour, the watch's local time); `alarm off` turns it off. While it is on, the face shows its time below the charge.
- At its minute the alarm wakes the watch, lights the face, shows `ALARM`, and buzzes the vibration motor, 350 ms on and 250 ms off, for a minute or until any button is pressed. That press only stops it.
- An alarm the watch could not ring on time, because it was restarting or updating, still rings up to 5 minutes late; later than that, the face shows `MISSED` until a button is pressed.
- The vibration motor is driven for the first time; the console's `haptic` plays a tap or the alarm's buzz and reads the motor's driver back.
- The about page and `version` show the image's CRC, the same one [`tools/wrist-update.py`](../tools/wrist-update.py) prints when it installs, so you can check which image is on the watch.
- A crash record names the build that wrote it, when, and how many times in a row; [`tools/wrist-crash.py`](../tools/wrist-crash.py) takes the release zip and refuses a record from another build.
- A frame whose transfer to the display loses part of the picture is drawn again, rather than shown shifted by a column.
- During an update, the watch's check of the received image no longer holds it busy for the whole check.
- The loader is unchanged: no loader update is needed.

## Limits

- The watch follows daylight saving for the two changes [`tools/wrist-time.py`](../tools/wrist-time.py) last sent, about a year. After that, after a change of zone, or after a 16-second hold of Select, which resets the power and clears the zone and the alarm with the clock, run the tool again. It knows no zone names.
- One alarm, every day: no days of the week, no snooze, and no way yet to set it on the watch itself.
- A short press lights the face for 3 s, or turns off a light switched on from the console. Back and Select only light it; Up and Down also move between pages.
- After a cable is pulled during an update, the next update can get no answer from the watch. Run it again.
- No stock apps, notifications, sensors, Bluetooth or activity storage.
