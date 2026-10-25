//! `settings`: the owner's saved preferences, kept in `BKP17R` to `BKP30R` as one checked
//! record (`wrist_settings`).
//!
//! The commands that change a setting (`hr strap`, `hr off`, `hr relay`, `ble multi`) save it
//! as they apply it, and `settings set` does the same by name. The relay and multi take their
//! saved values at boot; a saved strap is looked for each time the radio comes up, and given
//! up after five minutes as a lost one is. The backup registers survive a reset and an update,
//! but not the loss of the backup domain (a PMIC hard reset, or a flat battery): then every
//! setting is back to its default, off.

use wrist_settings::{from_hex, to_hex, Key, Problem, Settings, Value, PAYLOAD, VALUE_TEXT, VERSION};

use crate::uart::Uart;

/// The saved settings, or the defaults if none were saved or the record is unreadable.
#[must_use]
pub fn load() -> Settings {
    Settings::decode(&crate::rtc::settings_words()).unwrap_or_default()
}

fn save(s: &Settings) {
    crate::rtc::set_settings_words(&s.encode());
}

/// Saves `value` for `key` and applies it now.
pub fn change(key: Key, value: Value, uart: Uart) {
    let mut s = load();
    if s.get(key) != value {
        s.set(key, value);
        save(&s);
    }
    apply(key, value, uart);
}

#[cfg(feature = "ble")]
fn apply(key: Key, value: Value, uart: Uart) {
    crate::ble::apply_setting(key, value, uart);
}

#[cfg(not(feature = "ble"))]
fn apply(_: Key, _: Value, uart: Uart) {
    uart.write_line("[settings] saved; this build has no radio, so it does nothing here");
}

/// `settings`, `settings set <name> <value>`, `settings reset`, `settings export`,
/// `settings import <hex>`.
pub fn command(args: &[u8], uart: Uart) {
    let text = core::str::from_utf8(args).unwrap_or("").trim();
    let (word, rest) = text.split_once(' ').unwrap_or((text, ""));
    match word {
        "" => list(uart),
        "set" => {
            let (name, value) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let Some(key) = Key::from_name(name) else {
                uart.write_str("[settings] usage: settings set <name> <value>; names:");
                for k in Key::ALL {
                    uart.write_str(" ");
                    uart.write_str(k.name());
                }
                return uart.write_line("");
            };
            if let Some(v) = key.parse(value) {
                change(key, v, uart);
            } else {
                uart.write_str("[settings] ");
                uart.write_str(key.name());
                uart.write_str(" takes ");
                uart.write_line(key.usage());
            }
        }
        "reset" => {
            crate::rtc::set_settings_words(&[0; wrist_settings::WORDS]);
            uart.write_line("[settings] cleared; each is back to its default");
            for key in Key::ALL {
                apply(key, Settings::default().get(key), uart);
            }
        }
        "export" => {
            let mut payload = [0u8; PAYLOAD];
            let n = load().to_bytes(&mut payload);
            let mut digits = [0u8; 2 * PAYLOAD];
            let d = to_hex(&payload[..n], &mut digits);
            uart.write_str("settings import ");
            uart.write_bytes(&digits[..d]);
            uart.write_line("");
        }
        "import" => match from_hex(rest.trim()) {
            Some(s) => {
                save(&s);
                uart.write_line("[settings] imported");
                for key in Key::ALL {
                    apply(key, s.get(key), uart);
                }
            }
            None => uart.write_line("[settings] not imported: not a settings payload (`settings export` writes one)"),
        },
        _ => uart.write_line("[settings] usage: settings [set <name> <value>|reset|export|import <hex>]"),
    }
}

fn list(uart: Uart) {
    let saved = Settings::decode(&crate::rtc::settings_words());
    uart.write_str("[settings] ");
    match saved {
        Ok(_) => {
            uart.write_str("record v");
            uart.write_u32(u32::from(VERSION));
        }
        Err(Problem::Absent) => uart.write_str("nothing saved"),
        Err(Problem::Corrupt) => uart.write_str("record UNREADABLE (bad check); defaults in force"),
        Err(Problem::Newer(v)) => {
            uart.write_str("record from a later version (v");
            uart.write_u32(u32::from(v));
            uart.write_str("); defaults in force");
        }
    }
    uart.write_line(" in BKP17R-BKP30R; kept across resets and updates, lost with the backup domain");
    let s = saved.unwrap_or_default();
    for key in Key::ALL {
        uart.write_str("[settings]   ");
        uart.write_str(key.name());
        uart.write_str(" = ");
        write_value(uart, s.get(key));
        if s.get(key) != Settings::default().get(key) {
            uart.write_str(" (default ");
            write_value(uart, Settings::default().get(key));
            uart.write_str(")");
        }
        uart.write_line("");
    }
}

fn write_value(uart: Uart, v: Value) {
    let mut out = [0u8; VALUE_TEXT];
    let n = v.write(&mut out);
    uart.write_bytes(&out[..n]);
}
