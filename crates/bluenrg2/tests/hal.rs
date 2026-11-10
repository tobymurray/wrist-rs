//! The `ACI_HAL` reads' return parameters, decoded.

use bluenrg2::hal::{updater_version, AnchorPeriod, FirmwareDetails};

const FIRMWARE: [u8; 14] = [2, 1, 0, 0x02, 0x34, 0x12, 2, 2, 1, 0, 0x0F, 0x00, 0x31, 0x09];

#[test]
fn firmware_details_decode_field_by_field() {
    assert_eq!(
        FirmwareDetails::decode(&FIRMWARE),
        Some(FirmwareDetails {
            dtm_version: [2, 1, 0],
            dtm_variant: 2,
            dtm_build: 0x1234,
            stack_version: [2, 2, 1],
            stack_development: 0,
            stack_variant: 0x000F,
            stack_build: 2353,
        })
    );
    assert_eq!(FirmwareDetails::decode(&FIRMWARE[..13]), None);
}

#[test]
fn anchor_period_decodes_little_endian() {
    let a = [0x20, 0x03, 0, 0, 0x80, 0x02, 0, 0];
    assert_eq!(AnchorPeriod::decode(&a), Some(AnchorPeriod { anchor_period: 800, max_free_slot: 640 }));
    assert_eq!(AnchorPeriod::decode(&a[..7]), None);
}

#[test]
fn the_updater_version_is_the_first_byte() {
    assert_eq!(updater_version(&[]), None);
    assert_eq!(updater_version(&[3, 0]), Some(3));
}
