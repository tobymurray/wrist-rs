# wrist-rs 0.5.0

| | |
|---|---|
| **Stock firmware** | **1.6.2** only. The installer runs on no other version. |
| **Watch** | UNA Watch |
| **Install guide** | [INSTALL.md](INSTALL.md) |

A watch face for the UNA Watch that shows the time, the day and date, and the battery's charge, with a daily alarm, pages for the battery, Bluetooth and the build, and the console and updates over Bluetooth, in place of the stock firmware. `manifest.json`, in the release zip, names this build's version and source commit.

## In this release

New since 0.4.0:

- Bluetooth, off until you turn it on. A Bluetooth page, between the battery and about pages, shows the radio's state and how many computers are bonded; Select turns the radio on or off, and the choice survives restarts and updates. A watch that never turns it on runs as 0.4.0 did.
- A computer bonded to the watch gets the console and application updates over the air with [`tools/wrist-ble.py`](../tools/wrist-ble.py), which works on Linux and macOS ([INSTALL.md](INSTALL.md#bluetooth)). The loader is never updated over the air, and the console answers no one else.
- Pairing is confirmed on the watch: the Bluetooth page shows the six digits the computer shows; Select if they match, Back if not. From Linux, `wrist-ble.py --pair` confirms them over the USB cable instead, with no button pressed.
- A computer that kept a key the watch no longer has can't connect until it forgets the watch and pairs again; the face shows `see Bluetooth`, and the page `forget & pair`, until it does.
- An update sent over the air is kept once the new version has an encrypted Bluetooth link, as a USB update is once the computer connects.
- Windows now recognises the watch's console port. The tools are still checked only on Linux and macOS.
- The loader is unchanged: no loader update is needed.

## Limits

- The watch takes one Bluetooth connection at a time. While another device holds it, a bonded computer finds nothing; UNA's phone app, if the watch was ever paired with it, can be that device.
- No phone can use the watch over Bluetooth yet: there is no app, and phones' own Bluetooth settings don't connect to it.
- `--pair` works only on Linux. TODO before merge: how a Mac pairs and pairs again, once one pairing through the page is checked.
- On battery the radio advertises about once a second and keeps an idle link slow, and a Linux computer takes about 9 s to secure each reconnection. TODO before merge: the radio's cost on battery, from the soaks.
- The watch follows daylight saving for the two changes [`tools/wrist-time.py`](../tools/wrist-time.py) last sent, about a year. After that, after a change of zone, or after a 16-second hold of Select, which resets the power and clears the zone and the alarm with the clock, run the tool again. It knows no zone names.
- One alarm, every day: no days of the week, no snooze, and no way yet to set it on the watch itself.
- A short press lights the face for 3 s, or turns off a light switched on from the console. Back and Select only light it, except on the Bluetooth page; Up and Down also move between pages.
- After a cable is pulled during an update, the next update can get no answer from the watch. Run it again.
- No stock apps, notifications, sensors or activity storage.
