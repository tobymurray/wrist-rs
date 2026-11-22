//! `ble probe matrix`: its cells, its lines, and the scheduler model against the chip's own (`fixtures/bluenrg2-probe.txt`).

use std::collections::HashMap;

use wrist_ble::{Activities, AdvParams, Change, Duration, Schedule};
use wrist_bluenrg2::probe::{Cell, Matrix, Order, Update, MATRIX_ADVERTISING_MS, MATRIX_LINK_MS};
use wrist_bluenrg2::{anchor_reading, native, Scheduler};

#[test]
fn every_cell_once_and_no_cell_asks_for_the_interval_the_link_has() {
    let cells: Vec<Cell> = (0..Cell::COUNT).map(|i| Cell::nth(i).unwrap()).collect();
    assert_eq!(Cell::nth(Cell::COUNT), None);
    for order in [Order::LinkFirst, Order::AdvertisingFirst] {
        for adv in MATRIX_ADVERTISING_MS {
            for link in MATRIX_LINK_MS {
                assert_eq!(cells.iter().filter(|c| (c.order, c.adv_ms, c.link_ms) == (order, adv, link)).count(), 1);
            }
        }
    }
    for w in cells.windows(2) {
        assert_ne!(w[0].link_ms, w[1].link_ms, "{w:?}");
    }
    for c in &cells {
        assert_eq!(u32::from(c.adv_units()) * 625, u32::from(c.adv_ms) * 1000);
        assert_eq!(u32::from(c.link_units()) * 1250, u32::from(c.link_ms) * 1000);
    }
}

#[test]
fn a_cell_line_names_each_change_in_the_order_made() {
    let anchor = [0xA8, 0, 0, 0, 0x8B, 0, 0, 0];
    let update = Update { status: Some(0), accepted: Some(true), done: Some((0, 28, 0)) };
    let line = Matrix { cell: Cell::nth(0).unwrap(), update, advertising: Some(0), anchors: [Some((0, &[0; 8])), Some((0, &anchor))], link_up: true };
    assert_eq!(
        line.to_string(),
        "probe matrix cell=0 order=link-first adv_ms=95 link_ms=35 update=0x00 answer=accepted done=0x00 interval_us=35000 latency=0 \
         anchor1_us=0 free1_us=0 adv=0x00 anchor2_us=105000 free2_us=86875 link=up"
    );
    let line = Matrix {
        cell: Cell::nth(30).unwrap(),
        update: Update { status: Some(0x0C), ..Update::default() },
        advertising: None,
        anchors: [Some((0x12, &[])), None],
        link_up: false,
    };
    assert_eq!(
        line.to_string(),
        "probe matrix cell=30 order=adv-first adv_ms=95 link_ms=35 adv=none anchor1=0x12 update=0x0c answer=none done=none anchor2=none link=lost"
    );
}

/// LE Connection Update Complete on `handle`.
fn update_complete(handle: u16, interval: u16) -> Vec<u8> {
    let [h0, h1] = handle.to_le_bytes();
    let [i0, i1] = interval.to_le_bytes();
    vec![0x04, 0x3E, 0x0A, 0x03, 0x00, h0, h1, i0, i1, 0, 0, 0xF4, 0x01]
}

/// `ACI_L2CAP_CONNECTION_UPDATE_RESP_EVENT` on `handle`, accepted.
fn l2cap_accepted(handle: u16) -> Vec<u8> {
    let [h0, h1] = handle.to_le_bytes();
    vec![0x04, 0xFF, 0x06, 0x00, 0x08, h0, h1, 0x00, 0x00]
}

#[test]
fn another_links_update_is_not_the_cells() {
    let mut u = Update { status: Some(0), ..Update::default() };
    u.saw(0x0801, &update_complete(0x0802, 36));
    u.saw(0x0801, &l2cap_accepted(0x0802));
    assert_eq!((u.accepted, u.done), (None, None));
    u.saw(0x0801, &l2cap_accepted(0x0801));
    u.saw(0x0801, &update_complete(0x0801, 28));
    assert_eq!((u.accepted, u.done), (Some(true), Some((0, 28, 0))));
}

fn adv(ms: u16) -> AdvParams {
    let units = Duration::from_u16(ms * 8 / 5);
    AdvParams { interval_min: units, interval_max: units, connectable: true }
}

/// The anchor period and free slot as a cell's line gives them, `n` the reading after the first
/// change or the second.
fn reading(fields: &HashMap<&str, &str>, n: u8) -> Option<(u32, u32)> {
    let period = fields.get(format!("anchor{n}_us").as_str())?.parse().ok()?;
    Some((period, fields[format!("free{n}_us").as_str()].parse().unwrap()))
}

fn model_reading(a: &Activities) -> (u32, u32) {
    anchor_reading(a).map_or((0, 0), |r| (r.period.0, r.max_free_slot.0))
}

/// Each cell as the chip answered it: the same admission and code, the interval asked, the same anchor readings.
#[test]
fn the_model_agrees_with_the_chip_in_every_cell_of_the_matrix() {
    let fixture = include_str!("fixtures/bluenrg2-probe.txt");
    let mut seen = 0;
    for line in fixture.lines().filter(|l| l.starts_with("probe matrix cell=")) {
        let fields: HashMap<&str, &str> = line.split(' ').filter_map(|f| f.split_once('=')).collect();
        let ms = |k: &str| fields[k].parse::<u16>().unwrap();
        let (adv_ms, link_ms) = (ms("adv_ms"), ms("link_ms"));
        assert_eq!(fields["link"], "up", "{line}");
        let linked = Activities { peripheral: 1, ..Activities::IDLE };
        let admitted = Scheduler.admit(&linked, &Change::Advertise(Some(adv(adv_ms))));
        let chip = fields["adv"];
        assert_eq!(chip, admitted.map_or_else(|k| format!("0x{:02x}", native(k)), |_| "0x00".into()), "advertising: {line}");
        assert_eq!((fields["update"], fields["answer"], fields["done"]), ("0x00", "accepted", "0x00"), "the update: {line}");
        assert_eq!(fields["interval_us"].parse::<u32>().unwrap(), u32::from(link_ms) * 1000, "{line}");
        let advertising = Activities { advertising: admitted.is_ok().then(|| adv(adv_ms)), ..linked };
        let after = |first: bool| if first == (fields["order"] == "link-first") { linked } else { advertising };
        assert_eq!(reading(&fields, 1), Some(model_reading(&after(true))), "the first reading: {line}");
        assert_eq!(reading(&fields, 2), Some(model_reading(&advertising)), "the second reading: {line}");
        seen += 1;
    }
    assert_eq!(seen, Cell::COUNT);
}
