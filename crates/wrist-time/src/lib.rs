//! Calendar arithmetic on the RTC's local time.

#![no_std]

pub mod alarm;
pub mod counter;
pub mod zone;

/// A date and 24-hour time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// `YYYY-MM-DD HH:MM:SS`, with surrounding whitespace ignored.
#[must_use]
pub fn parse_civil(text: &str) -> Option<Civil> {
    let b = text.trim().as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b' '
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u16> {
        let mut v: u16 = 0;
        for &d in &b[from..to] {
            if !d.is_ascii_digit() {
                return None;
            }
            v = v * 10 + u16::from(d - b'0');
        }
        Some(v)
    };
    let small = |from: usize, to: usize| num(from, to).and_then(|v| u8::try_from(v).ok());
    Some(Civil {
        year: num(0, 4)?,
        month: small(5, 7)?,
        day: small(8, 10)?,
        hour: small(11, 13)?,
        minute: small(14, 16)?,
        second: small(17, 19)?,
    })
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Seconds since 1970.
#[must_use]
pub fn seconds_of(c: Civil) -> i64 {
    days_from_civil(i64::from(c.year), i64::from(c.month), i64::from(c.day)) * 86_400
        + i64::from(c.hour) * 3600
        + i64::from(c.minute) * 60
        + i64::from(c.second)
}

#[must_use]
pub fn civil_of_seconds(total: i64) -> Civil {
    let days = total.div_euclid(86_400);
    let rem = total - days * 86_400;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year: u16::try_from(year).unwrap_or(0),
        month: u8::try_from(month).unwrap_or(1),
        day: u8::try_from(day).unwrap_or(1),
        hour: u8::try_from(rem / 3600).unwrap_or(0),
        minute: u8::try_from(rem % 3600 / 60).unwrap_or(0),
        second: u8::try_from(rem % 60).unwrap_or(0),
    }
}
