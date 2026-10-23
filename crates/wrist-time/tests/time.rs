use wrist_time::{civil_of_seconds, parse_civil, seconds_of, Civil};

fn civil(text: &str) -> Civil {
    parse_civil(text).unwrap()
}

#[test]
fn civil_text_parses_exactly() {
    assert_eq!(
        parse_civil("2026-10-17 14:57:46"),
        Some(Civil { year: 2026, month: 10, day: 17, hour: 14, minute: 57, second: 46 })
    );
    assert_eq!(parse_civil(" 2026-10-17 14:57:46\r\n"), Some(civil("2026-10-17 14:57:46")));
    assert_eq!(parse_civil("2026-10-17T14:57:46"), None);
    assert_eq!(parse_civil("2026-10-17 14:57"), None);
    assert_eq!(parse_civil("2026-1x-17 14:57:46"), None);
}

#[test]
fn seconds_round_trip_across_edges() {
    assert_eq!(seconds_of(civil("1970-01-01 00:00:00")), 0);
    assert_eq!(seconds_of(civil("2000-03-01 00:00:00")), 951_868_800);
    for text in ["1970-01-01 00:00:00", "2000-02-29 23:59:59", "2026-10-17 14:57:46", "2099-12-31 23:59:59"] {
        let c = civil(text);
        assert_eq!(civil_of_seconds(seconds_of(c)), c, "{text}");
    }
}
