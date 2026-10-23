# Local patch to embassy-usb-synopsys-otg

Upstream: https://github.com/embassy-rs/embassy/tree/81cabe7e5307c702d27e98577ac5e063c7796910/embassy-usb-synopsys-otg
License: MIT OR Apache-2.0 (included).

Sibling path dependencies are removed; the app pins Embassy's other crates to the same
revision.

Driver construction clears the endpoint allocations, enabled flags, OUT receive state and
pending setup state a previous instance left in the shared `State`; otherwise a rebuilt
driver allocates EP0 at a nonzero index and panics at start. The previous `Bus` and its
endpoint futures must be dropped, with its IRQ disabled, before a new driver uses the
peripheral; the STM32 `Bus` drop does that.

Remove this copy when upstream fixes reinitialization equivalently.
