#![no_std]

//! The memory LCD's `VCOM` rate (LCP-2619063C Table 6-3-3).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Auto,
    Low,
    High,
}

impl Mode {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Low => 1,
            Self::High => 2,
        }
    }

    /// Unknown values read as [`Mode::Auto`].
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Low,
            2 => Self::High,
            _ => Self::Auto,
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "low" => Some(Self::Low),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rate {
    Low,
    High,
}

#[must_use]
pub const fn rate(mode: Mode, light_on: bool) -> Rate {
    match mode {
        Mode::Low => Rate::Low,
        Mode::High => Rate::High,
        Mode::Auto => {
            if light_on {
                Rate::High
            } else {
                Rate::Low
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_follows_the_light_only_in_auto() {
        assert_eq!(rate(Mode::Auto, false), Rate::Low);
        assert_eq!(rate(Mode::Auto, true), Rate::High);
        assert_eq!(rate(Mode::Low, true), Rate::Low);
        assert_eq!(rate(Mode::High, false), Rate::High);
    }

    #[test]
    fn mode_round_trips() {
        for m in [Mode::Auto, Mode::Low, Mode::High] {
            assert_eq!(Mode::from_u8(m.as_u8()), m);
            assert_eq!(Mode::parse(m.name()), Some(m));
        }
        assert_eq!(Mode::from_u8(9), Mode::Auto);
        assert_eq!(Mode::parse("fast"), None);
    }
}
