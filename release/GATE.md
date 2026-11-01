# Release gate

A release needs, recorded in [`gate.json`](gate.json): this checklist run on a watch and written up as
numbered installs in the owner's validation records, their decisions, and a look at the
candidate's GitHub run. The checklist writes a watch's flash, so only the watch's owner
runs it.

## Install and verify

Nothing else on the computer may touch the watch's drive or port during the run: check for
a leftover logger or watcher first (`ps aux | grep -iE 'watch|serial|ttyACM|usbmodem'`).

Use one candidate throughout: `wrist-rs-VERSION.zip` from a CI run or the local recipe
([README.md](README.md)), with its debug zip. Keep each step's console output; every step
that writes flash is a numbered install in the validation records, failures included.
`cmd` below is `.venv/bin/python tools/wrist-cmd.py`, set up as in [INSTALL.md](INSTALL.md)
(the package's `START-HERE.md`), "The tools".

[`tools/gate.py`](../tools/gate.py), run from a checkout at the candidate's commit, walks
this checklist: it does the console and drive work, checks what it can read, asks for each
press, hold and unplug, and keeps every step's output under one directory. It runs the
sections in the order below: A, C, D when it runs, then B.

### Prepare

1. Beside the zips, `sha256sum -c SHA256SUMS` (macOS: `shasum -a 256 -c SHA256SUMS`).
   Note `build_id`, `image_crc32` and `round_trip_fingerprint` from `manifest.json`.
2. Download the stock `.ota` of the version the release notes name and check it as
   INSTALL.md says. Charge the watch above 50 %.
3. Build a second copy of the image that names itself `gate-test` in place of the build
   ID, for step 12:

   ```sh
   cd apps/standalone-boot
   WRIST_RELEASE_BUILD_ID=gate-test cargo build --release
   <objcopy> -O binary target/thumbv8m.main-none-eabihf/release/standalone-boot gate-test.bin
   ```

### A. Update from the build on the watch

The watch runs an earlier wrist-rs.

4. First `cmd "power sleep"`: note a `[crash]` line other than `no retained failure`, whose
   `build` names the image that wrote it, then `cmd "power sleep clear-crash"`. Then the refusals: `.venv/bin/python tools/wrist-update.py --loader app.bin` and
   `.venv/bin/python tools/wrist-update.py first-install/Apps/ImageInstaller/image.bin` each
   exit at once with one line and send nothing. Then `.venv/bin/python tools/wrist-update.py
   app.bin`. Expect `[update] ok`, a restart and `state boot`. `cmd "boot;version"`:
   `state boot`, and the build is `build_id`. Once more, pulling
   the cable at about half way and plugging it back in: the next `update` is accepted. Once
   more, killing the tool half way: about 5 s later the console prints `[update] error: no
   data for 5 s; transfer abandoned`, answers `version`, and the next `update` is accepted.
5. The face: day and date, the time, the charge, inside the ring, and the minute rolls
   over on time. The time survived the update. `.venv/bin/python tools/wrist-time.py`
   prints `[rtc] set` and `[rtc] zone …, N change(s) kept`; `cmd time` shows the zone and
   its changes, and still shows them after `reset`. Then a forced change: with `T` fifteen
   seconds ahead as UTC seconds since 1970 (`date +%s`), `cmd "time zone <offset> T
   <offset+60>"`; within 10 s of `T` the face is an hour ahead and the console says
   `[rtc] zone change applied`. On USB the watch looks for a due change every 10 s, on
   battery as the minute turns, which is when real changes fall. The same with
   `<offset-60>` goes back an hour once, and not again in the minute after. Finish with [`tools/wrist-time.py`](../tools/wrist-time.py). After step 13's 16 s hold, step 16's `cmd
   time` shows no zone record.
6. `cmd report 10`, under `[step] park`: SDMMC1 now off, the unused lines analog, RTC
   alarm A off, unused SRAM off in Stop with the SRAM bits of
   [`apps/standalone-boot/src/mmio.rs`](../apps/standalone-boot/src/mmio.rs) set in `PWR_CR2` and `PWR_CR4`, and the IMU and
   compass reset to suspend. Under `-- clocks on --`: `ICACHE_CR_EN` set and
   `RCC_AHB2ENR1_SDMMC1EN` clear (`mmio.rs`).
7. `cmd modules`: each of `view`, `link1` and `link2` reads `expected`. `cmd "page lag 10"`, wait ten seconds, then
   `cmd page`: the longest time a task kept the executor is a few milliseconds at most.
   `cmd "page bus"`: the bus keeps the data window, and `failed 0`.
8. Buttons. A short press of each of Up, Down, Back and Select lights the face. `cmd "page
   light on"`, then a short press: the light goes off, and the next press lights the face
   for 3 s. Pages: from the face, Down shows the battery (the charge, the voltage, `input
   USB`), Down again about (the version and `build_id` in upper case), and Down again the
   face; Up goes the other way, and Back returns to the face from either page. Left on the
   battery page, the face comes back within 10 s. Hold Back until the watch restarts, then
   let go: it comes back to the face with the time still right, and `report`'s `RCC_CSR`
   at handoff names `PIN`, with no `IWDG` or `WWDG` (`SFT` reads set too, as at 0.3.0's and
   0.3.1's gates). Select held on through a restart would
   continue into the bootloader's USB mode.

### C. The update safety net

12. `.venv/bin/python tools/wrist-update.py gate-test.bin --no-reset`, then unplug. Hold
    Back until the watch restarts: the loader swaps `gate-test` in, and it starts on battery
    with no computer to keep it. Once the face is back, Up shows about with `GATE-TEST`;
    wait 30 s, then hold Back until it restarts again: the loader puts the previous image
    back. Plug in. `cmd version` shows `build_id`, not `gate-test`, and `boot` shows `state
    boot`.

### D. Stock and back: the full-size installer's round trip

Run this section when the candidate's `round_trip_fingerprint` is not `gate.json`'s, when
the stock version changes, or when INSTALL.md's "Stock to wrist-rs" or "Back to stock"
changes. Otherwise the round trip of `round_trip_release` stands: the installer and loader
are byte for byte the ones it ran, and the recipe holds the image to the installer's plan.

13. Back to that stock version, as in INSTALL.md ("Back to stock"), but holding Select
    through the power reset: past the USB screen at about 3 s, until the screen blanks at
    about 16 s and the USB screen comes back. Stock starts and reports it. A run that
    starts at step 14 skips step 16's zone check.
14. The dry run: INSTALL.md steps 1 to 4, with every check there passing. The launcher
    lists **Wrist-rs**, and the screen shows `INSTALL-CRC.txt`'s digits.
15. The confirm's refusals, each from a passing dry run: a click of Select; Select held for
    about two seconds and let go; Select held with Back pressed during the countdown; and
    Select pressed during `Checking` and held until the ready screen appears (the launcher
    opens an app on release, so a hold cannot start there).
    None of them installs: the screen goes back to the CRC and "Hold Select to install", and
    the watch stays on stock. Note anything else a 3 s hold of Select brings up.
16. Install: INSTALL.md steps 5 to 7. Note how long `Installing` stays on screen. The
    watch comes up in wrist-rs: `cmd "boot;version"` says `state boot` and the build.
    Set the time, and repeat steps 5 and 6.
17. Back to stock again (step 13), then a dry run: `install.log` shows the last install's
    outcome record for this CRC in the witness page, the right first page and count, and
    the staging area as the recovery left it.
18. Install the same candidate again, as in step 16: wrist-rs, `state boot`, the build.

### B. On battery, last

The gate's one long wait, so it runs after C, and after D when D runs: once it starts, the
rest is unplugging, waiting and plugging back in.

9. Unplug for at least ten minutes; an hour or a night gives better figures. The face
   keeps time, and a short press lights it.
10. Plug back in. The console comes back within a few seconds. If it has not within a
    minute, record the failure; holding Back until the watch restarts, with the cable in,
    brings USB back up.
11. `cmd power`: the input is external, and the last stretch on battery shows its length,
    mean current, charge at each end and the gauge's time to empty; `unanswered input
    reads in a row 0; Stop follows the input`. `stop2_wakes` divided
    by that length in minutes is the wake rate on battery: expect about thirteen a minute, from
    the time driver every 8 s, the watchdog's feed every 16 s and the minute's redraw.
    `PWR_INT_L` reads high, and `input interrupts` has gone up by two, the unplug and the
    plug. Under `[lptim]`: `alarms handled over 5 ms late 0`, `Stop entered with one
    unacknowledged 0` and `waits over 2 ms: DIEROK 0, CMPOK 0`; a late alarm is a press
    whose light comes on late. `[crash] no
    retained failure`: nothing panicked or faulted since step 4, through every restart (a
    record resolves with [`tools/wrist-crash.py`](../tools/wrist-crash.py) and this release's zip). Record the mean
    current, the time to empty and the wake rate; a meter in series with the cell gives the
    real floor, if you have one.

### Record

19. Write each install into the validation records, with its output; failures too.
20. On `main`, add anything learned to HARDWARE.md.
21. In [`gate.json`](gate.json): the install numbers, `image_crc32` as step 1 noted it, and
    `round_trip_validated` true only if steps 4 to 12 passed, and 13 to 18 when they ran.
    After a round trip: `stock_version` as the version it used, `round_trip_release` as this
    version and `round_trip_fingerprint` as step 1 noted it.
22. In [NOTES.md](NOTES.md): what this release was checked to do, and on which stock
    version.

## Owner decisions

- **`license_distribution_approved`**: wrist-rs is AGPL-3.0-or-later, and each release's
  source, at the commit in its manifest, stays public.
- **`notices_reviewed`**: the open items in [NOTICES.md](NOTICES.md).
- **`github_candidate_run_verified`**: the candidate's CI run passed, and its archive
  hashes, attestation and downloads check out without private access.

## Publishing

Commit [`gate.json`](gate.json), tag that commit `vA.B.C` and push the tag. Review the tag's CI run,
then run the firmware workflow on the tag with `publish` set.

The tag's image is the gated one byte for byte: the build ID hashes only what goes into the
binaries ([`build-inputs.txt`](build-inputs.txt)), and `gate.json`, the docs and the
release notes are not among it, so the tag's commit rebuilds the same `loader.bin`,
`app.bin` and `image.bin`, with the same CRC; the installer's `.uapp` also carries the
version it is built as. The recipe and the publish job refuse a release whose `image_crc32`
is not `gate.json`'s. A commit after the gate that changes an input changes the image, and
the gate runs again on the new candidate.

A loader change is validated on its own, recovery included: the loader has no second
slot.
