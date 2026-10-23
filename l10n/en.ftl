# The source catalogue: every message the watch shows, in English.
#
# A Fluent subset (l10n/README.md): plain text, { $variable } placeholders,
# plural selection on a number, and terms. Every message needs an id in ids.toml.

-product = wrist-rs

## The face

# The date line before the clock has been set.
face-set-time = set time
# The time before the clock has been set: two digits, a colon, two digits.
face-time-unset = --:--
# A date the clock holds that is not a real day.
face-date-invalid = ?
# The charge in whole percent, 0 to 255.
charge-percent = { $pct } %
# The charge before the gauge has a reading.
charge-unset = -- %

## Battery

# The page's title. The font has capitals only at this size.
battery-title = BATTERY
# The cell's voltage in millivolts.
battery-cell-mv = { $mv } mV
battery-cell-unset = -- mV
# What is charging the watch, or that nothing is.
battery-input-usb = input USB
battery-input-cell = input cell
battery-input-unknown = input ?

## About

about-title = ABOUT
about-product = { -product }
