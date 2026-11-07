# The source catalogue: every message the face's pages show, in English.
#
# A Fluent subset (l10n/README.md).

-product = wrist-rs

## The face

# The date line before the clock has been set.
face-set-time = set time
# The time before the clock has been set, its digits as figure dashes (U+2012).
face-time-unset = ‒‒:‒‒
# A date the clock holds that is not a real day.
face-date-invalid = ?
# The charge in whole percent, 0 to 255.
charge-percent = { $pct } %
# The charge before the gauge has a reading, its digits as figure dashes.
charge-unset = ‒‒ %
# In place of the date while the alarm rings, in capitals like the pages' titles.
face-alarm-ringing = ALARM
# In place of the date after the alarm was found too late to ring, until a press.
face-alarm-missed = MISSED

## Battery

# The page's title, in capitals like the English.
battery-title = BATTERY
# The cell's voltage in millivolts.
battery-cell-mv = { $mv } mV
# Before the first reading, its four digits as figure dashes.
battery-cell-unset = ‒‒‒‒ mV
# What is charging the watch, or that nothing is.
battery-input-usb = input USB
battery-input-cell = input cell
battery-input-unknown = input ?

## About

about-title = ABOUT
about-product = { -product }

## Bluetooth

# The page's title, in capitals like the other pages'.
bluetooth-title = BLUETOOTH
# The radio: held off, starting, up but not advertising, advertising, a phone connected, that
# link encrypted, and the chip failed to start.
bluetooth-off = off
bluetooth-starting = starting
bluetooth-on = on
bluetooth-discoverable = discoverable
bluetooth-connected = connected
bluetooth-secure = secure link
bluetooth-failed = failed
# How many phones and computers the watch keeps pairing keys for.
bluetooth-bonded = { $n } bonded
# During a numeric comparison, under the six digits: Select if the phone shows the same
# number, Back if it doesn't.
bluetooth-select-matches = select: it matches
bluetooth-back-no-match = back: no match
# During passkey entry, under the six digits: type them on the phone.
bluetooth-type-passkey = type on the phone
# On the Bluetooth page, for a phone or computer refused for a pairing key the watch no longer has.
bluetooth-old-key = forget & pair
# On the face, for the same.
bluetooth-see-page = see Bluetooth
