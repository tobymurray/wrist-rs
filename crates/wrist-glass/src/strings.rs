//! Every word and symbol the pages put on the glass, in English, and nowhere else.
//!
//! The pages take their text from here, so the catalogue (`plan/I18N-2026-10-23.md`, C14.2)
//! can replace this module without touching a layout. A test holds the rest of the crate to
//! it: a string literal anywhere else in `src` is a failure unless it never reaches the
//! glass, or is data, and its line says so with `// verbatim`.
//!
//! The face's date and time are still built in English order here (`push_date`, `push_hm`);
//! C14.2's formatter replaces that order with CLDR's patterns.

/// Monday first, as [`crate::pages::Date::weekday`] counts.
pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Between the weekday, the day and the month: `Fri 23 Oct`.
pub const DATE_GAP: &str = " ";
/// Between hours and minutes.
pub const TIME_SEPARATOR: &str = ":";
/// A date with a month or day out of range.
pub const DATE_INVALID: &str = "?";

/// The face's date line before the clock is set.
pub const SET_TIME: &str = "set time";
/// The face's time before the clock is set.
pub const TIME_UNSET: &str = "--:--";
/// A reading not yet made.
pub const UNSET: &str = "--";

/// After a charge in whole percent: `100 %`.
pub const PERCENT_SUFFIX: &str = " %";
/// After a cell voltage in millivolts: `4405 mV`.
pub const MILLIVOLT_SUFFIX: &str = " mV";

pub const BATTERY_TITLE: &str = "BATTERY";
pub const INPUT_USB: &str = "input USB";
pub const INPUT_CELL: &str = "input cell";
pub const INPUT_UNKNOWN: &str = "input ?";

pub const ABOUT_TITLE: &str = "ABOUT";
/// The product's name, a term every locale keeps.
pub const PRODUCT: &str = "wrist-rs";
