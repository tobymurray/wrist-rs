# wrist-glass

The watch face, kept apart from the hardware so it can be checked on the host. The panel
is square behind a round window, so a layout that fits the buffer can still run under the
rim.

| Check | Where | What it proves |
|---|---|---|
| [`tests/every_value.rs`](tests/every_value.rs) | `cargo test -p wrist-glass` | every value of every line, in every built-in locale and both typefaces, fits the glass under its ink and touches no other line |
| [`tests/drawable.rs`](tests/drawable.rs) | the same | every string a built-in locale can put on a line is in that line's face |
| [`tests/on_the_glass.rs`](tests/on_the_glass.rs) | the same | the pages at their extremes: nothing under the ring, nothing lost to the disc's clip |
| [`tests/columns.rs`](tests/columns.rs) | the same | rewriting only `Glass::changed_columns` gives the frame a full rewrite does |
| [`tests/repaint.rs`](tests/repaint.rs) | the same | repainting the changed columns in strips, from any frame, every minute of a day, every charge, between every page and through every stress screen, leaves the frame a full render draws |
| [`tests/figures.rs`](tests/figures.rs) | the same | a line that changes keeps every character that did not where it was |
| `--features preview` on those tests | on request | PNGs of each case under `target/tmp/wrist-glass/`, one folder per typeface |

The text is [IncisedText](https://crates.io/crates/incised-text)'s. `pages::FACES` picks
the typeface, Poppins or Atkinson Hyperlegible Next, and `pages::Faces` the face of each
kind of line. `text` places lines on the disc, `pages` composes the face from a `Status` and
the battery and about pages from theirs, `scene` lays a glass out once so a frame repaints only
the columns that changed, `stress` composes the screens the console's `page stress` puts in
place of a page to work the drawing hard, and `dump` codes a frame for the console's `page
dump`. Which page is up is [`wrist-pages`](../wrist-pages)'.

A line that does not fit the glass loses characters from its end until it does, and its
bit in `Short` is set. To add a line, give it a face and a baseline in `pages` and add its extremes to
the tests' cases.
