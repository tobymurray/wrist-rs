# bluenrg2

The packets of ST's BlueNRG-2 Bluetooth Low Energy network coprocessor, as bytes: HCI and ACI
command packets, the SPI header's fields, and HCI and ST vendor events decoded. `no_std`, no
allocation, and no SPI bus or link state; bring your own driver. The crate documentation
describes the SPI exchange, after ST's X-CUBE-BLE2.

```rust
use bluenrg2::{
    command, header_sizes, notification, opcode, packet_len, parse, service_type, uuid_type, vendor, Event, HEADER_WRITE,
    MAX_COMMAND,
};

// ACI_GATT_ADD_SERVICE: a primary Heart Rate service (0x180D) of up to four attributes.
let mut out = [0u8; MAX_COMMAND];
let parts: [&[u8]; 3] = [&[uuid_type::U16], &0x180Du16.to_le_bytes(), &[service_type::PRIMARY, 4]];
let n = command(opcode::ACI_GATT_ADD_SERVICE, &parts, &mut out).expect("fits");
assert_eq!(&out[..n], &[0x01, 0x02, 0xFD, 5, 0x01, 0x0D, 0x18, 0x01, 4]);

// The SPI transfer leaves the chip's answer where `HEADER_WRITE` was; this one has room for 128 bytes.
let mut header = HEADER_WRITE;
header.copy_from_slice(&[0x02, 0x80, 0x00, 0x00, 0x00]);
let (room, _) = header_sizes(&header);
assert!(room >= n);

// One read: the reply, which names the service's handle, then a notification.
let read = [
    0x04, 0x0E, 0x06, 0x01, 0x02, 0xFD, 0x00, 0x0C, 0x00,
    0x04, 0xFF, 0x08, 0x0F, 0x0C, 0x01, 0x08, 0x0E, 0x00, 0x01, 0x48,
];
let mut rest = &read[..];
while let Some(packet) = packet_len(rest).and_then(|len| rest.get(..len)) {
    match parse(packet) {
        Event::CommandComplete { cmd_opcode: opcode::ACI_GATT_ADD_SERVICE, status: Some(status), return_param_bytes, .. } => {
            assert!(status.is_success());
            assert_eq!(return_param_bytes, &[0x0C, 0x00]);
        }
        Event::Vendor { code: vendor::GATT_NOTIFICATION, params, .. } => {
            assert_eq!(notification(params), Some((0x000E, &[0x48][..])));
        }
        e => panic!("{e:?}"),
    }
    rest = &rest[packet.len()..];
}
assert!(rest.is_empty());
```

The `opcode` module and the value modules beside it name a subset of the ACI; `command` builds
any opcode. Standard events
decode through [`bt-hci`](https://crates.io/crates/bt-hci).

Layouts follow:

- the [Bluetooth Core Specification](https://www.bluetooth.com/specifications/specs/core-specification/),
  Vol 4 Part E for HCI;
- ST's [PM0257](https://www.st.com/resource/en/programming_manual/pm0257-bluenrg1-bluenrg2-ble-stack-v2x-programming-guidelines-stmicroelectronics.pdf),
  the BlueNRG-1/2 stack v2.x programming guidelines;
- ST's `bluenrg1_*.h` ACI headers, shipped in [X-CUBE-BLE2](https://www.st.com/en/embedded-software/x-cube-ble2.html)
  for the [BlueNRG-2](https://www.st.com/en/wireless-connectivity/bluenrg-2.html) as a network coprocessor.

For the older BlueNRG-MS, see [`bluenrg`](https://crates.io/crates/bluenrg).

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option.
