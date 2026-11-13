//! The `ACI_HAL` reads' return parameters, decoded.

use bluenrg2::hal::{link_status, updater_version, AnchorPeriod, FirmwareDetails, LinkStatus};

const FIRMWARE: [u8; 14] = [2, 1, 0, 0x02, 0x34, 0x12, 2, 2, 1, 0, 0x0F, 0x00, 0x31, 0x09];

#[test]
fn firmware_details_decode_field_by_field() {
    let FirmwareDetails {
        dtm_version,
        dtm_variant,
        dtm_build,
        stack_version,
        stack_development,
        stack_variant,
        stack_build,
        ..
    } = FirmwareDetails::decode(&FIRMWARE).unwrap();
    assert_eq!(
        (
            dtm_version,
            dtm_variant,
            dtm_build,
            stack_version,
            stack_development,
            stack_variant,
            stack_build
        ),
        ([2, 1, 0], 2, 0x1234, [2, 2, 1], 0, 0x000F, 2353)
    );
    assert_eq!(FirmwareDetails::decode(&FIRMWARE[..13]), None);
}

#[test]
fn anchor_period_decodes_little_endian() {
    let a = [0x20, 0x03, 0, 0, 0x80, 0x02, 0, 0];
    let AnchorPeriod { anchor_period, max_free_slot, .. } = AnchorPeriod::decode(&a).unwrap();
    assert_eq!((anchor_period, max_free_slot), (800, 640));
    assert_eq!(AnchorPeriod::decode(&a[..7]), None);
}

#[test]
fn the_updater_version_is_the_first_byte() {
    assert_eq!(updater_version(&[]), None);
    assert_eq!(updater_version(&[3, 0]), Some(3));
}

#[test]
fn link_status_is_eight_states_then_eight_handles_little_endian() {
    let p: Vec<u8> = (0..24).collect();
    let LinkStatus { link_status: states, link_connection_handle, .. } =
        LinkStatus::decode(&p).unwrap();
    assert_eq!(states, [0, 1, 2, 3, 4, 5, 6, 7]);
    let named = [
        link_status::IDLE,
        link_status::ADVERTISING,
        link_status::PERIPHERAL,
        link_status::SCANNING,
        link_status::INITIATING,
        link_status::CENTRAL,
        link_status::TX_TEST,
        link_status::RX_TEST,
    ];
    assert_eq!(states, named);
    assert_eq!(
        link_connection_handle,
        [0x0908, 0x0B0A, 0x0D0C, 0x0F0E, 0x1110, 0x1312, 0x1514, 0x1716]
    );
    assert_eq!(LinkStatus::decode(&p[..23]), None);
}
