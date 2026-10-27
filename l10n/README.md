# The watch's words

Everything the face's pages show in words is a message here: `crates/wrist-glass` takes
words only as messages. The installer's screens are outside it.

| File | What |
|---|---|
| `en.ftl` | The source catalogue, in English: every message and its note for translators |
| `locales.toml` | The locales built into the image, their CLDR tags and plural rules |

`cargo run -p wrist-l10n-gen` compiles these, with CLDR's weekday and month names, date and
time patterns from [ICU4X](https://github.com/unicode-org/icu4x), into
`crates/wrist-l10n/src/generated.rs`, which is checked in. `cargo test --workspace` fails
while that file is stale, or when a built-in locale's dates, times or plurals differ from
ICU4X's.

## The Fluent subset

Messages are [Fluent](https://projectfluent.org/fluent/guide/), limited to what the watch
draws without a Fluent runtime:

```ftl
# A note for translators: what it is and where it goes.
battery-cell-mv = { $mv } mV

-product = wrist-rs
about-product = { -product }

steps-today = { $steps ->
    [one] { $steps } step
   *[other] { $steps } steps
}
```

- Text on one line, `{ $variable }` placeholders, terms without arguments, and string
  literals such as `{ "{" }`. A term comes before anything that uses it.
- Ids and variables are lower-case words joined by `-`; no id is `self`, and no variable is a
  Rust keyword.
- A selector on a number, `{ $n -> ... }` or `{ NUMBER($n) -> ... }`, whose keys are CLDR
  plural categories (`zero`, `one`, `two`, `few`, `many`) and whose default is `*[other]`.
- No attributes, functions or number options; the generator names the message it refuses.

Each message is a `Msg` variant whose fields are its variables, all whole numbers; a
translation has every message and uses only the English message's variables.

## Adding a message

1. Add it to `en.ftl` with a note.
2. `cargo run -p wrist-l10n-gen`, then draw it with `Msg::YourMessage { .. }`.
3. Use only characters that every line it can appear on draws, in both typefaces (`Faces` in
   `crates/wrist-glass/src/pages.rs`): a test fails on any other, in a message or in a
   locale's CLDR names, and another on any value too wide for its line.
4. Never build a sentence from pieces in code: make the whole sentence one message with
   placeholders, so a translation can reorder it.
5. Stand in for a number not yet known with figure dashes (`‒`, U+2012), one for each digit
   it usually has: they are as wide as digits, so nothing moves when the number arrives.

The console is not translated: it is the instrument, and its logs read the same everywhere.

## Built-in English

The image's English is CLDR's `en-GB`, as ICU4X has it.
