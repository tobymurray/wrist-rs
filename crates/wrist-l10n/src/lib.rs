#![no_std]

//! Every word the face's pages show, and the dates, times and numbers around them, per locale,
//! as compiled from `l10n/` (`l10n/README.md`).

pub mod generated;

pub use generated::{Msg, BUILT_IN, EN_GB, MESSAGES};

/// A CLDR plural category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// A piece of a compiled message.
#[derive(Debug)]
pub enum Piece {
    Text(&'static str),
    /// The message's argument at this index, in order of first appearance in the English.
    Arg(u8),
    /// The pieces of the first case whose category the argument's number has, else `other`.
    Select { arg: u8, cases: &'static [(Plural, &'static [Piece])], other: &'static [Piece] },
}

/// A piece of a date pattern (CLDR `E`, `d`, `MMM` and literal text).
#[derive(Debug, PartialEq, Eq)]
pub enum DatePiece {
    /// The abbreviated weekday.
    Weekday,
    /// The day of the month, unpadded.
    Day,
    /// The abbreviated month, in its format (not stand-alone) form.
    Month,
    Text(&'static str),
}

/// A piece of a time pattern (CLDR `HH`, `mm` and literal text).
#[derive(Debug, PartialEq, Eq)]
pub enum TimePiece {
    /// The hour, 00 to 23.
    Hour,
    /// The minute, 00 to 59.
    Minute,
    Text(&'static str),
}

/// What a locale needs on the watch.
pub struct Locale {
    /// The CLDR locale its data came from.
    pub tag: &'static str,
    /// Indexed by [`Msg::parts`].
    pub messages: &'static [&'static [Piece]; MESSAGES],
    /// Monday first.
    pub weekdays: [&'static str; 7],
    pub months: [&'static str; 12],
    /// The weekday, day and month, as the face shows them (CLDR skeleton `MMMEd`).
    pub date: &'static [DatePiece],
    /// Hours and minutes on the 24-hour clock (CLDR skeleton `Hm`).
    pub time: &'static [TimePiece],
    pub plural: fn(u32) -> Plural,
}

/// Where text is written, so a page's line takes it without a buffer of its own.
pub trait Sink {
    type Error;
    /// # Errors
    /// When the text does not fit.
    fn text(&mut self, s: &str) -> Result<(), Self::Error>;
    /// # Errors
    /// When the number does not fit.
    fn number(&mut self, n: u32) -> Result<(), Self::Error>;
    /// `n % 100` as two digits.
    ///
    /// # Errors
    /// When the digits do not fit.
    fn two_digits(&mut self, n: u8) -> Result<(), Self::Error>;
}

/// A date in the proleptic Gregorian calendar; any values hold, and the caller checks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    /// 0 Monday to 6 Sunday, by Sakamoto's method.
    #[must_use]
    pub const fn weekday(self) -> u8 {
        const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let m = if self.month >= 1 && self.month <= 12 { self.month } else { 1 };
        let y = if m < 3 { self.year.saturating_sub(1) } else { self.year } as u32;
        let sunday0 = (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] as u32 + self.day as u32) % 7;
        ((sunday0 + 6) % 7) as u8
    }

    /// A month from 1 to 12 and a day from 1 to 31.
    #[must_use]
    pub const fn named(self) -> bool {
        self.month >= 1 && self.month <= 12 && self.day >= 1 && self.day <= 31
    }
}

/// A 24-hour time of day; any values print, modulo 100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    pub h: u8,
    pub m: u8,
    pub s: u8,
}

impl Locale {
    /// # Errors
    /// The sink's, when the message does not fit.
    pub fn message<S: Sink>(&self, sink: &mut S, msg: Msg) -> Result<(), S::Error> {
        let (row, args) = msg.parts();
        self.pieces(sink, self.messages.get(row).copied().unwrap_or(&[]), &args)
    }

    fn pieces<S: Sink>(&self, sink: &mut S, pieces: &[Piece], args: &[u32]) -> Result<(), S::Error> {
        for piece in pieces {
            match piece {
                Piece::Text(s) => sink.text(s)?,
                Piece::Arg(i) => {
                    if let Some(n) = args.get(usize::from(*i)) {
                        sink.number(*n)?;
                    }
                }
                Piece::Select { arg, cases, other } => {
                    let category = args.get(usize::from(*arg)).map_or(Plural::Other, |n| (self.plural)(*n));
                    let chosen = cases.iter().find(|(c, _)| *c == category).map_or(*other, |(_, p)| *p);
                    self.pieces(sink, chosen, args)?;
                }
            }
        }
        Ok(())
    }

    /// The weekday, day and month; the caller checks [`Date::named`] first.
    ///
    /// # Errors
    /// The sink's, when the date does not fit.
    pub fn date<S: Sink>(&self, sink: &mut S, d: Date) -> Result<(), S::Error> {
        for piece in self.date {
            match piece {
                DatePiece::Weekday => sink.text(self.weekdays[usize::from(d.weekday() % 7)])?,
                DatePiece::Day => sink.number(u32::from(d.day))?,
                DatePiece::Month => sink.text(self.months[usize::from(d.month.clamp(1, 12) - 1)])?,
                DatePiece::Text(s) => sink.text(s)?,
            }
        }
        Ok(())
    }

    /// # Errors
    /// The sink's, when the time does not fit.
    pub fn time<S: Sink>(&self, sink: &mut S, c: Clock) -> Result<(), S::Error> {
        for piece in self.time {
            match piece {
                TimePiece::Hour => sink.two_digits(c.h)?,
                TimePiece::Minute => sink.two_digits(c.m)?,
                TimePiece::Text(s) => sink.text(s)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::fmt::Write as _;
    use std::string::{String, ToString};

    #[derive(Default)]
    struct Text(String);

    impl Sink for Text {
        type Error = ();
        fn text(&mut self, s: &str) -> Result<(), ()> {
            self.0.push_str(s);
            Ok(())
        }
        fn number(&mut self, n: u32) -> Result<(), ()> {
            self.0.push_str(&n.to_string());
            Ok(())
        }
        fn two_digits(&mut self, n: u8) -> Result<(), ()> {
            write!(self.0, "{:02}", n % 100).map_err(|_| ())
        }
    }

    /// `{ $day }: { $steps -> ... }`, with `$steps` in each case.
    static STEPS_MESSAGE: &[Piece] = &[
        Piece::Arg(1),
        Piece::Text(": "),
        Piece::Select {
            arg: 0,
            cases: &[(Plural::One, &[Piece::Arg(0), Piece::Text(" step")]), (Plural::Few, &[Piece::Arg(0), Piece::Text(" steps (few)")])],
            other: &[Piece::Arg(0), Piece::Text(" steps")],
        },
    ];

    /// Russian-shaped: one, few and other.
    static STEPS: Locale = Locale {
        tag: "test",
        messages: &[&[]; MESSAGES],
        weekdays: ["M", "T", "W", "T", "F", "S", "S"],
        months: ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12"],
        date: &[DatePiece::Day, DatePiece::Text("."), DatePiece::Month, DatePiece::Text(" "), DatePiece::Weekday],
        time: &[TimePiece::Hour, TimePiece::Text("h"), TimePiece::Minute],
        plural: |n| match n % 10 {
            1 => Plural::One,
            2..=4 => Plural::Few,
            _ => Plural::Other,
        },
    };

    fn say(args: &[u32]) -> String {
        let mut t = Text::default();
        STEPS.pieces(&mut t, STEPS_MESSAGE, args).unwrap();
        t.0
    }

    #[test]
    fn a_select_takes_the_case_of_its_number() {
        assert_eq!(say(&[1, 23]), "23: 1 step");
        assert_eq!(say(&[3, 23]), "23: 3 steps (few)");
        assert_eq!(say(&[5, 23]), "23: 5 steps");
    }

    #[test]
    fn dates_and_times_follow_the_pattern() {
        let mut t = Text::default();
        STEPS.date(&mut t, Date { year: 2026, month: 10, day: 23 }).unwrap();
        STEPS.time(&mut t, Clock { h: 7, m: 5, s: 0 }).unwrap();
        assert_eq!(t.0, "23.10 F07h05");
    }

    #[test]
    fn weekdays_by_sakamoto() {
        assert_eq!(Date { year: 2026, month: 10, day: 23 }.weekday(), 4);
        assert_eq!(Date { year: 2000, month: 2, day: 29 }.weekday(), 1);
        assert!(!Date { year: 2026, month: 13, day: 1 }.named());
        assert!(!Date { year: 2026, month: 1, day: 0 }.named());
    }

    #[test]
    fn a_message_draws_its_arguments() {
        let mut t = Text::default();
        EN_GB.message(&mut t, Msg::BatteryCellMv { mv: 4405 }).unwrap();
        assert_eq!(t.0, "4405 mV");
    }
}
