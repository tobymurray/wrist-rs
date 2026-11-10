//! The scheduler model against what the chip itself reports of its time base.

use std::collections::HashMap;

use wrist_ble::{Activities, AdvParams, Change, ConnParams, Duration};
use wrist_bluenrg2::{anchor_reading, Scheduler};

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

#[test]
fn the_free_slot_is_the_longest_a_new_link_gets() {
    let link = |ce| {
        let u = Duration::from_u16(40);
        ConnParams { interval_min: u, interval_max: u, latency: 0, timeout: Duration::from_u16(600), ce_len: Duration::from_u16(ce) }
    };
    let connect = |a: &Activities, ce| a.walk(&Scheduler, &[Change::Initiate(Some(link(ce))), Change::Connected]);
    let a = connect(&Activities::IDLE, 4).unwrap();
    let a = connect(&a, 4).unwrap().walk(&Scheduler, &[Change::Close { slot: 0 }]).unwrap();
    let r = anchor_reading(&a).unwrap();
    assert_eq!((r.period.0, r.max_free_slot.0), (50_000, 40_000));
    let slots = u16::try_from(r.max_free_slot.0 / 625).unwrap();
    assert!(connect(&a, slots).is_ok());
    assert!(connect(&a, slots + 1).is_err());
    let a = [4, 40, 4].iter().fold(Activities::IDLE, |a, &ce| connect(&a, ce).unwrap());
    let a = a.walk(&Scheduler, &[Change::Close { slot: 1 }]).unwrap();
    assert_eq!(anchor_reading(&a).map(|r| r.max_free_slot.0), Some(25_000));
    assert!(connect(&a, 40).is_ok());
    assert!(connect(&a, 41).is_err());
}
