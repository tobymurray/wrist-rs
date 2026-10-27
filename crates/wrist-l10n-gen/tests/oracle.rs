//! Every built-in locale's generated tables against ICU4X itself: every day of three years,
//! every minute of a day, and every count from 0 to 10,000.

use std::fmt::Write as _;

use icu::calendar::{Date, Gregorian};
use icu::datetime::{fieldsets, FixedCalendarDateTimeFormatter, NoCalendarFormatter};
use icu::plurals::{PluralCategory, PluralRules};
use wrist_l10n::{Clock, Plural, Sink, BUILT_IN};

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
    fn day(&mut self, d: u8) -> Result<(), ()> {
        self.number(u32::from(d))
    }
}

#[test]
fn dates_match_icu4x() {
    for l in BUILT_IN {
        let tag: icu::locale::Locale = l.tag.parse().unwrap();
        let f = FixedCalendarDateTimeFormatter::<Gregorian, _>::try_new(tag.into(), fieldsets::MDE::medium()).unwrap();
        let mut days = 0;
        for year in 2026..=2028u16 {
            for month in 1..=12u8 {
                for day in 1..=31u8 {
                    let Ok(icu_date) = Date::try_new_gregorian(i32::from(year), month, day) else { continue };
                    let want = f.format(&icu_date).to_string();
                    let mut got = Text::default();
                    l.date(&mut got, wrist_l10n::Date { year, month, day }).unwrap();
                    assert_eq!(got.0, want, "{} {year}-{month}-{day}", l.tag);
                    days += 1;
                }
            }
        }
        assert_eq!(days, 365 * 3 + 1);
    }
}

#[test]
fn times_match_icu4x() {
    for l in BUILT_IN {
        let tag: icu::locale::Locale = format!("{}-u-hc-h23", l.tag).parse().unwrap();
        let f = NoCalendarFormatter::try_new(tag.into(), fieldsets::T::hm()).unwrap();
        for h in 0..24u8 {
            for m in 0..60u8 {
                let want = f.format(&icu::time::Time::try_new(h, m, 0, 0).unwrap()).to_string();
                let mut got = Text::default();
                l.time(&mut got, Clock { h, m, s: 0 }).unwrap();
                assert_eq!(got.0, want, "{} {h}:{m}", l.tag);
            }
        }
    }
}

#[test]
fn plurals_match_icu4x() {
    for l in BUILT_IN {
        let tag: icu::locale::Locale = l.tag.parse().unwrap();
        let rules = PluralRules::try_new_cardinal(tag.into()).unwrap();
        for n in 0..=10_000u32 {
            let want = match rules.category_for(n) {
                PluralCategory::Zero => Plural::Zero,
                PluralCategory::One => Plural::One,
                PluralCategory::Two => Plural::Two,
                PluralCategory::Few => Plural::Few,
                PluralCategory::Many => Plural::Many,
                PluralCategory::Other => Plural::Other,
            };
            assert_eq!((l.plural)(n), want, "{} {n}", l.tag);
        }
    }
}
