//! A `ble-trace` capture's lines as the replay reads them, so a fixture keeps no more and no less.

use std::borrow::Cow;
use std::fmt;

/// A capture's line: the host's stamp, then the console's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamped<'a> {
    pub stamp: &'a str,
    /// The stamp in milliseconds.
    pub ms: u64,
    pub text: &'a str,
}

/// `line`'s stamp, seconds or a wall clock's `HH:MM:SS`, either to three decimals, and its text.
#[must_use]
pub fn stamped(line: &str) -> Option<Stamped<'_>> {
    let (stamp, text) = line.trim_start().split_once(' ')?;
    let (whole, ms) = stamp.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if ms.len() != 3 || !digits(ms) {
        return None;
    }
    let fields: Vec<&str> = whole.split(':').collect();
    let clock = fields.len() == 3 && fields.iter().all(|f| f.len() == 2);
    if !(clock || fields.len() == 1) || !fields.iter().all(|f| digits(f)) {
        return None;
    }
    let value = |s: &str| {
        s.bytes().fold(0u64, |v, b| v.saturating_mul(10).saturating_add(u64::from(b - b'0')))
    };
    let seconds = fields.iter().fold(0u64, |s, f| s.saturating_mul(60).saturating_add(value(f)));
    Some(Stamped { stamp, ms: seconds.saturating_mul(1_000).saturating_add(value(ms)), text })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Auto,
    Cable,
    Battery,
}

/// A console command the replay acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `ble on` or `ble off`.
    Radio(bool),
    /// `ble yes` or `ble no`.
    Answer(bool),
    Disconnect,
    Profile(Profile),
    IntervalNow(u16, u16),
    /// A USB image's length and CRC.
    Update(u32, u32),
}

/// The command `cmd` is, as the firmware's console takes it: one trailing space dropped.
#[must_use]
pub fn command(cmd: &str) -> Option<Command> {
    Some(match cmd.strip_suffix(' ').unwrap_or(cmd) {
        "ble on" => Command::Radio(true),
        "ble off" => Command::Radio(false),
        "ble yes" => Command::Answer(true),
        "ble no" => Command::Answer(false),
        "ble disconnect" => Command::Disconnect,
        "ble profile auto" => Command::Profile(Profile::Auto),
        "ble profile cable" => Command::Profile(Profile::Cable),
        "ble profile battery" => Command::Profile(Profile::Battery),
        _ => {
            if let Some(rest) = cmd.strip_prefix("ble interval now") {
                let mut units = rest.split_whitespace().map(|w| w.parse::<u16>().ok());
                let (Some(Some(min)), Some(Some(max))) = (units.next(), units.next()) else {
                    return None;
                };
                return wrist_bluenrg2::wake::ble::interval_now(min, max)
                    .then_some(Command::IntervalNow(min, max));
            }
            // `parse_transfer` in the console.
            let mut parts = cmd.strip_prefix("update ")?.split_whitespace();
            let len = parts.next()?.parse::<u32>().ok()?;
            Command::Update(len, u32::from_str_radix(parts.next()?, 16).ok()?)
        }
    })
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let on = |b: bool, yes: &'static str, no: &'static str| if b { yes } else { no };
        match self {
            Self::Radio(b) => write!(f, "ble {}", on(*b, "on", "off")),
            Self::Answer(b) => write!(f, "ble {}", on(*b, "yes", "no")),
            Self::Disconnect => write!(f, "ble disconnect"),
            Self::Profile(Profile::Auto) => write!(f, "ble profile auto"),
            Self::Profile(Profile::Cable) => write!(f, "ble profile cable"),
            Self::Profile(Profile::Battery) => write!(f, "ble profile battery"),
            Self::IntervalNow(min, max) => write!(f, "ble interval now {min} {max}"),
            Self::Update(len, crc) => write!(f, "update {len} {crc:X}"),
        }
    }
}

/// A `[trace] <ms> <kind> [<hex> [+<more>]]` line of the driver's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace<'a> {
    pub ms: u64,
    pub kind: &'a str,
    pub packet: Option<Vec<u8>>,
    /// The packet ran past what the trace keeps.
    pub cut: bool,
}

/// The trace line `text` is; `None` for one whose packet is not hex.
#[must_use]
pub fn trace(text: &str) -> Option<Trace<'_>> {
    let rest = text.strip_prefix("[trace] ")?;
    let mut f = rest.split_whitespace();
    let ms = f.next()?.parse().ok()?;
    let kind = f.next()?;
    let packet = match f.next().filter(|h| !h.starts_with('+')) {
        Some(h) if h.len() % 2 != 0 || !h.bytes().all(|b| b.is_ascii_hexdigit()) => return None,
        Some(h) => Some(
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or_default())
                .collect(),
        ),
        None => None,
    };
    Some(Trace { ms, kind, packet, cut: rest.contains(" +") })
}

/// `text` with each run of six or more hex bytes, bare or with `:`, `-` or `_` between and no hex
/// digit either side, replaced.
#[must_use]
pub fn scrub_addresses(text: &str) -> Cow<'_, str> {
    let b = text.as_bytes();
    let hex = |i: usize| b.get(i).is_some_and(u8::is_ascii_hexdigit);
    let pair = |i: usize| hex(i) && hex(i + 1);
    let ends = |i: usize| {
        std::iter::successors(pair(i).then_some(i + 2), |&j| {
            let k = if matches!(b.get(j), Some(b':' | b'-' | b'_')) { j + 1 } else { j };
            pair(k).then_some(k + 2)
        })
    };
    let mut out = String::new();
    let mut copied = 0;
    for i in 0..b.len() {
        if i < copied || (i > 0 && hex(i - 1)) {
            continue;
        }
        if let Some(end) = ends(i).skip(5).filter(|&e| !hex(e)).last() {
            out.push_str(&text[copied..i]);
            out.push_str("<address>");
            copied = end;
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}
