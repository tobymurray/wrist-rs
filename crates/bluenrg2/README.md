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

The `opcode` module names a subset of the ACI; `command` builds any opcode. Layouts follow the Bluetooth Core Specification and ST's
PM0257 and `bluenrg1_*.h` headers. Standard events decode through
[`bt-hci`](https://crates.io/crates/bt-hci).

For the older BlueNRG-MS, see [`bluenrg`](https://crates.io/crates/bluenrg).

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option.
