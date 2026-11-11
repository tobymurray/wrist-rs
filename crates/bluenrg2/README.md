# bluenrg2

The packets of ST's BlueNRG-2 Bluetooth Low Energy network coprocessor, as bytes: the
five-byte SPI header, HCI and ACI command packets, and HCI and ST vendor events decoded.
`no_std`, no allocation, and no SPI bus or link state; bring your own driver.

```rust
use bluenrg2::{command, header_sizes, opcode, parse, Event, MAX_COMMAND};

// A command packet, ready to write after the SPI header.
let mut out = [0u8; MAX_COMMAND];
let n = command(opcode::HCI_READ_LOCAL_VERSION, &[], &mut out);
assert_eq!(&out[..n], &[0x01, 0x01, 0x10, 0x00]);

// The chip's answer to a header: room to write, bytes to read.
assert_eq!(header_sizes(&[0x02, 0x80, 0x00, 0x07, 0x00]), (128, 7));

// One event from a read.
let read = [0x04, 0x0E, 0x04, 0x01, 0x01, 0x10, 0x00];
assert!(matches!(parse(&read), Event::CommandComplete { cmd_opcode: 0x1001, .. }));
```

The `opcode` module names a subset of the ACI; `command` builds any opcode. Standard events
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
