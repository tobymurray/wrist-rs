# The watch's words

Everything the watch shows in words is a message here. The firmware never holds a
user-visible literal of its own: `crates/wrist-glass` has a test that fails on one.

| File | What |
|---|---|
| `en.ftl` | The source catalogue, in English: every message and its note for translators |
| `ids.toml` | Each message's stable number. Append only; a retired number is never reused |
| `locales.toml` | The locales built into the image, their CLDR tags and plural rules |

`cargo run -p wrist-l10n-gen` compiles these, with CLDR's weekday and month names, date and
time patterns from [ICU4X](https://github.com/unicode-org/icu4x), into
`crates/wrist-l10n/src/generated.rs`, which is checked in. `cargo test --workspace` fails
while that file is stale, and checks every built-in locale's dates, times and plurals
against ICU4X: every day of three years, every minute of a day, every count to 10,000.

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
  literals such as `{ "{" }`.
- A selector on a number, `{ $n -> ... }` or `{ NUMBER($n) -> ... }`, whose keys are CLDR
  plural categories (`zero`, `one`, `two`, `few`, `many`) and whose default is `*[other]`.
- No attributes, functions or number options yet; the generator names the message it
  refuses.

A message's arguments are numbered in the order they first appear in the English, and a
translation may use only the English message's variables.

## Adding a message

1. Add it to `en.ftl` with a note, and give it the next free number in `ids.toml`.
2. `cargo run -p wrist-l10n-gen`, then draw it with `Msg::YourMessage`.
3. Never build a sentence from pieces in code: make the whole sentence one message with
   placeholders, so a translation can reorder it.

The console is not translated: it is the instrument, and its logs read the same everywhere.

## Built-in English

The image's English is CLDR's `en-GB`, whose date (`Fri 23 Oct`) and 24-hour time are the
face's. One override is kept in `locales.toml`, with its reason: September is `Sep`, not
CLDR's `Sept`.
