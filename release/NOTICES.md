# Distribution notices

wrist-rs's own code is licensed under **AGPL-3.0-or-later**; the full text is the package's
[`LICENSE`](../LICENSE). Binaries in this package are object code under AGPL section 6: their
corresponding source is the wrist-rs repository at the `source_commit` in `manifest.json`,
which must stay publicly available for any published release. Third-party material keeps
its own licence, and none of it may be linked into wrist-rs on terms incompatible with the
AGPL; in particular, TouchGFX (SLA0048) must stay out of anything wrist-rs ships.

`licenses/` collects the license and notice files of every Cargo dependency, of what the
installer links from the SDK and the GNU Arm toolchain, and of the fonts; collecting them is
not legal clearance, and host-only dependencies may be over-included.

Known bundled material:

- **The UNA SDK**, linked into the installer (`wrist-rs-installer_*.uapp`); the repository does not
  contain it. MIT covers UNA's own code only; its `THIRD-PARTY-LICENSES.md` excludes its
  third-party components. Of those, the installer links coreJSON (MIT) and no TouchGFX
  (SLA0048), which is restricted to ST processors and must not be relicensed.
- **libstdc++**: the installer links objects from the SDK's `libstdc++.a`, which is
  byte-identical to the one in GNU Tools for STM32 13.3.rel1.20240926-1715 (GCC 13.3.1),
  the compiler the release recipe uses. It is GPL-3.0-or-later with the GCC Runtime
  Library Exception 3.1, which permits conveying the installer under the AGPL: its C and
  C++ are compiled by GCC and its Rust by rustc, which uses no GCC-based work. Both texts
  are in `licenses/sdk/libstdc++/`.
- **The GNU Arm compiler runtime**: the installer also links parts of newlib and GCC's
  `libgcc` unwinder from that toolchain, whose licence collection is
  `licenses/gnu-arm-toolchain/license.txt`. Rust's `core` and compiler builtins are under
  `licenses/rust-toolchain/`.
- **InscribedDisc**: MIT.
- **IncisedText** draws the installer's text and the face's. Its code is MIT OR Apache-2.0;
  its glyph data is rendered from Poppins and Atkinson Hyperlegible Next, which makes it a
  Modified Version of each under the SIL Open Font License 1.1, whose texts are in
  `licenses/root/incised-text-*/Fonts/` for the installer and `licenses/app/incised-text-*/Fonts/`
  for the image.
- **`wrist-glass`'s 5×7 glyphs** derive from the font table in SparkFun's Nokia 5110
  example (MIT, Copyright (c) 2016 SparkFun Electronics), with changes; its licence is
  `licenses/wrist-glass-font/LICENSE`.
- **Embassy**, including the vendored and patched HAL and USB driver: MIT OR Apache-2.0.
  Other Rust crates carry their own licences.
- **pyserial** (BSD) is installed separately and not bundled.

No official firmware OTA, private flash dump, decompiled excerpt, signing key or NDA
PixArt material is included. Raw firmware and image files are not vendor-signed OTAs.
