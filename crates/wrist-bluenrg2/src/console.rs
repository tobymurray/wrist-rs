//! The chip's values as the watch's console words them.

use bluenrg2::{AddrKind, Address, BdAddr, BootReason};
use core::fmt;

/// What a boot reason says of the chip's last start, in a few words.
#[must_use]
pub const fn boot_reason(reason: BootReason) -> &'static str {
    match reason {
        BootReason::STARTED => "the stack started",
        BootReason::UPDATER_BY_COMMAND => "the updater, by command",
        BootReason::UPDATER_BAD_FLAG => "the updater, the stack not marked valid",
        BootReason::UPDATER_BY_PIN => "the updater, by the IRQ pin",
        BootReason::WATCHDOG => "a watchdog reset",
        BootReason::LOCKUP => "a lockup reset",
        BootReason::BROWNOUT => "a brownout reset",
        BootReason::CRASH => "a crash reset",
        BootReason::ECC_ERROR => "an ECC error reset",
        _ => "a reason unknown",
    }
}

/// `0A:1B:2C:3D:4E:5F`, then ` (random)` for a random address, resolved (3) or not, as
/// [`parse_address`] reads it back.
#[must_use]
pub fn address(a: Address) -> impl fmt::Display {
    struct Text(Address);
    impl fmt::Display for Text {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)?;
            if self.0.kind.as_raw() & 0x01 != 0 {
                f.write_str(" (random)")?;
            }
            Ok(())
        }
    }
    Text(a)
}

/// An address as [`address`] writes it, read back: colons or dashes, either case.
#[must_use]
pub fn parse_address(text: &str) -> Option<Address> {
    let text = text.trim();
    let (text, kind) = match text.strip_suffix("(random)") {
        Some(rest) => (rest.trim_end(), AddrKind::RANDOM),
        None => (text, AddrKind::PUBLIC),
    };
    let mut bytes = [0u8; 6];
    let mut parts = text.split([':', '-']);
    for i in (0..6).rev() {
        let part = parts.next()?;
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        bytes[i] = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(Address { kind, addr: BdAddr::new(bytes) })
}
