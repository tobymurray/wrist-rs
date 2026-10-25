//! A short line of text built without `core::fmt`, to keep the GUI small.

/// Up to [`Label::CAPACITY`] bytes of ASCII; anything longer is cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    bytes: [u8; Label::CAPACITY],
    len: u8,
}

impl Label {
    /// "Refused -2147483648" is 19.
    pub const CAPACITY: usize = 24;

    #[must_use]
    pub fn text(text: &str) -> Label {
        let mut label = Label { bytes: [0; Label::CAPACITY], len: 0 };
        for &b in text.as_bytes() {
            label.push(b);
        }
        label
    }

    /// Eight upper-case hex digits, as `INSTALL-CRC.txt` writes a CRC.
    #[must_use]
    pub fn hex(value: u32) -> Label {
        let mut label = Label::text("");
        for b in value.to_be_bytes() {
            label = label.then_hex2(b);
        }
        label
    }

    /// Two upper-case hex digits appended.
    #[must_use]
    pub fn then_hex2(mut self, value: u8) -> Label {
        const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
        self.push(DIGITS[usize::from(value >> 4)]);
        self.push(DIGITS[usize::from(value & 0xF)]);
        self
    }

    /// A signed decimal appended.
    #[must_use]
    pub fn then_decimal(mut self, value: i32) -> Label {
        if value < 0 {
            self.push(b'-');
        }
        let mut n = value.unsigned_abs();
        let mut digits = [0u8; 10];
        let mut count = 0;
        loop {
            // A remainder below 10 always fits.
            digits[count] = b'0' + u8::try_from(n % 10).unwrap_or(0);
            count += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        for &d in digits[..count].iter().rev() {
            self.push(d);
        }
        self
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII is ever pushed.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    fn push(&mut self, b: u8) {
        if usize::from(self.len) < Label::CAPACITY && b.is_ascii() {
            self.bytes[usize::from(self.len)] = b;
            self.len += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_eight_upper_case_digits() {
        assert_eq!(Label::hex(0x1A2B_3C4D).as_str(), "1A2B3C4D");
        assert_eq!(Label::hex(0).as_str(), "00000000");
        assert_eq!(Label::hex(u32::MAX).as_str(), "FFFFFFFF");
    }

    #[test]
    fn decimals_at_the_extremes() {
        assert_eq!(Label::text("Refused ").then_decimal(i32::MIN).as_str(), "Refused -2147483648");
        assert_eq!(Label::text("").then_decimal(i32::MAX).as_str(), "2147483647");
        assert_eq!(Label::text("Unlock ").then_decimal(0).as_str(), "Unlock 0");
        assert_eq!(Label::text("").then_decimal(-16).as_str(), "-16");
    }

    #[test]
    fn too_long_is_cut() {
        let long = "x".repeat(40);
        assert_eq!(Label::text(&long).as_str().len(), Label::CAPACITY);
    }
}
