//! The scheduler model against what the chip itself reports of its time base.

use std::collections::HashMap;

use wrist_ble::{Activities, AdvParams, Duration};
use wrist_bluenrg2::anchor_reading;

/// Each reading in the fixture, and what the model says the chip reads beside the same activity.
#[test]
fn the_model_reads_the_anchor_the_chip_reported() {
    let fixture = include_str!("fixtures/probe-anchor.txt");
    let mut seen = 0;
    for line in fixture.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let fields: HashMap<&str, &str> = line.split(' ').filter_map(|f| f.split_once('=')).collect();
        let us = |k: &str| fields[k].parse::<u32>().unwrap();
        let a = match fields["beside"] {
            "idle" => Activities::IDLE,
            "advertising" => {
                let units = u16::try_from(us("adv_interval_us") / 625).unwrap();
                let adv = AdvParams { interval_min: Duration::from_u16(units), interval_max: Duration::from_u16(units), connectable: true };
                Activities { advertising: Some(adv), ..Activities::IDLE }
            }
            "link" => Activities { peripheral: 1, ..Activities::IDLE },
            other => panic!("{other}"),
        };
        let model = anchor_reading(&a).map_or((0, 0), |r| (r.period.0, r.max_free_slot.0));
        assert_eq!(model, (us("anchor_period_us"), us("max_free_slot_us")), "{line}");
        seen += 1;
    }
    assert_eq!(seen, 3);
}
