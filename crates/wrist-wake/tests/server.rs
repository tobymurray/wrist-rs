//! The watch's GATT server: the console where RX and TX say it is, and each service named.

use wrist_ble::{CharDecl, ServerTable, ServiceDecl};
use wrist_wake::ble::server::{device_information, services, CONSOLE, RX, TX};

static DIS: [CharDecl; 3] = device_information(b"0123456789ab");
static SERVICES: [ServiceDecl; 2] = services(&DIS);

#[test]
fn rx_and_tx_are_the_consoles_characteristics_in_the_table() {
    let table = ServerTable { services: &SERVICES };
    assert_eq!(table.get(RX), Some(&CONSOLE.chars[0]));
    assert_eq!(table.get(TX), Some(&CONSOLE.chars[1]));
}

#[test]
fn each_service_has_the_name_a_failed_start_gives_it() {
    assert_eq!(SERVICES.map(|s| s.name), ["Device Information", "console"]);
}
