# wrist-glass

The watch face, kept apart from the hardware so it can be checked on the host. The panel
is square behind a round window, so a layout that fits the buffer can still run under the
rim.

| Check | Where | What it proves |
|---|---|---|
| `const { assert!(..) }` in `text::Glass::put` | every build | each line fits the glass under its rows |
| [`tests/on_the_glass.rs`](tests/on_the_glass.rs) | `cargo test -p wrist-glass` | the face fits at every extreme, and nothing lies under the ring |
| `--features preview` on those tests | on request | PNGs of each case under `target/tmp/wrist-glass/` |

`text` places lines on the disc, `font` holds the 5×7 glyphs, and `pages` composes the face
from a `Status`.

To add a line, give its `Line<N>` the width of its widest content, place it with
`Glass::put`, and add its extremes to the tests' cases; if the glass cannot hold it, the
build fails.
