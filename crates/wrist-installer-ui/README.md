# wrist-installer-ui

The installer app's screen, kept apart from the watch so it can be checked on the host. The
GUI process of [`apps/image-installer`](../../apps/image-installer) links it as a static
library; the C++ shell in its `GUI/` only carries the kernel's messages to it.

| Module | What it holds |
|---|---|
| `flow` | The confirm: R1 held for `HOLD_MS` sends it; letting go, R2 or leaving cancels; a press from before the ready screen never counts |
| `installer` | The last verdict, the text each one shows, and what the shell does after each event |
| `draw` | The screens, with the SDK widgets' button hints and ring |
| `ffi` | The C interface and the panic handler, with `--features device` |

| Check | Where | What it proves |
|---|---|---|
| unit tests in `flow` and `installer` | `cargo test -p wrist-installer-ui` | the confirm's rules |
| [`tests/on_the_glass.rs`](tests/on_the_glass.rs) | the same | every screen is on the glass, its text clear of the hints and the ring, every word whole, every glyph present, and the ring grows on every tick |

The text is [IncisedText](https://crates.io/crates/incised-text)'s Poppins, with Atkinson
Hyperlegible Next for the headline and the CRC. Both are under the SIL Open Font License 1.1,
whose texts the release collects from the crate.
