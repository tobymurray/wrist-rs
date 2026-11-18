//! The watch's GATT server: Device Information, and the console in the Nordic UART Service's shape.

use wrist_ble::bt_hci::uuid::BluetoothUuid128;
use wrist_ble::{BluetoothUuid, CharDecl, Permission, Props, ServerAttr, ServiceDecl, ATT_MAX_MTU};

/// The longest Device Information string kept.
const MAX_STRING: usize = 32;

/// The console's characteristics hold what one notification at the largest MTU carries.
pub const CONSOLE_VALUE_LEN: usize = ATT_MAX_MTU - 3;
#[allow(clippy::cast_possible_truncation, reason = "under one MTU")]
const CONSOLE_MAX_LEN: u16 = CONSOLE_VALUE_LEN as u16;

/// `6E40000x-B5A3-F393-E0A9-E50E24DCCA9E`, little-endian, with `x` = `last`.
const fn nus(last: u8) -> BluetoothUuid {
    let mut b = [0x9E, 0xCA, 0xDC, 0x24, 0x0E, 0xE5, 0xA9, 0xE0, 0x93, 0xF3, 0xA3, 0xB5, 0x00, 0x00, 0x40, 0x6E];
    b[12] = last;
    BluetoothUuid::Uuid128(BluetoothUuid128::from_le_bytes(b))
}

const fn string(uuid: u16, value: &'static [u8]) -> CharDecl {
    let len = if value.len() < MAX_STRING { value.len() } else { MAX_STRING };
    #[allow(clippy::cast_possible_truncation, reason = "at most MAX_STRING")]
    let max_len = len as u16;
    CharDecl { uuid: BluetoothUuid::from_u16(uuid), props: Props::READ, permission: Permission::Open, max_len, value }
}

/// Device Information (Bluetooth SIG assigned numbers): manufacturer, model, and `firmware`.
#[must_use]
pub const fn device_information(firmware: &'static [u8]) -> [CharDecl; 3] {
    [string(0x2A29, b"wrist-rs"), string(0x2A24, b"UNA Watch"), string(0x2A26, firmware)]
}

pub const DEVICE_INFORMATION: BluetoothUuid = BluetoothUuid::from_u16(0x180A);

/// The console: the central writes commands and image bytes to RX, and the watch notifies its
/// output on TX; RX's properties and permission are measured in HARDWARE.md's Bluetooth module.
pub const CONSOLE: ServiceDecl = ServiceDecl {
    name: "console",
    uuid: nus(0x01),
    chars: &[
        CharDecl {
            uuid: nus(0x02),
            props: Props::WRITE.with(Props::WRITE_WITHOUT_RESPONSE),
            permission: Permission::Authenticated,
            max_len: CONSOLE_MAX_LEN,
            value: &[],
        },
        CharDecl { uuid: nus(0x03), props: Props::NOTIFY, permission: Permission::Open, max_len: CONSOLE_MAX_LEN, value: &[] },
    ],
};

/// The watch's services, in the order the driver adds them, with `device_information`'s
/// characteristics.
#[must_use]
pub const fn services(device_information: &'static [CharDecl; 3]) -> [ServiceDecl; 2] {
    [ServiceDecl { name: "Device Information", uuid: DEVICE_INFORMATION, chars: device_information }, CONSOLE]
}

/// RX and TX in the table [`services`] gives.
pub const RX: ServerAttr = ServerAttr(3);
pub const TX: ServerAttr = ServerAttr(4);
