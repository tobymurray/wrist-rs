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
