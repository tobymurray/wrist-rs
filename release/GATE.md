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

### Prepare

1. Beside the zips, `sha256sum -c SHA256SUMS` (macOS: `shasum -a 256 -c SHA256SUMS`).
   Note `build_id` and `image_crc32` from `manifest.json`.
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

4. First the refusals: `.venv/bin/python tools/wrist-update.py --loader app.bin` and
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
   its changes, and still shows them after `reset`. Then a forced change: with `T` sixty
   seconds ahead as UTC seconds since 1970 (`date +%s`), `cmd "time zone <offset> T
   <offset+60>"`; at the next minute the face is an hour ahead and the console says
   `[rtc] zone change applied`. The same with `<offset-60>` goes back an hour once and not
   again. Finish with [`tools/wrist-time.py`](../tools/wrist-time.py). After the 16 s hold of Select, later, `cmd
   time` shows no zone record.
6. `cmd report 10`, under `[step] park`: SDMMC1 now off, the unused lines analog, RTC
   alarm A off, unused SRAM off in Stop with the SRAM bits of
   [`apps/standalone-boot/src/mmio.rs`](../apps/standalone-boot/src/mmio.rs) set in `PWR_CR2` and `PWR_CR4`, and the IMU and
   compass reset to suspend. Under `-- clocks on --`: `ICACHE_CR_EN` set and
   `RCC_AHB2ENR1_SDMMC1EN` clear (`mmio.rs`).
7. `cmd modules`: both records as expected. `cmd "page lag 10"`, wait ten seconds, then
   `cmd page`: the longest time a task kept the executor is a few milliseconds at most.
8. Buttons. A short press of each of Up, Down, Back and Select lights the face. `cmd "page
   light on"`, then a short press: the light goes off, and the next press lights the face
   for 3 s. Hold Back until the watch restarts, then let go: it comes back to the face with
   the time still right, and `report` shows `reset flags` naming only that reset, with no
   `IWDG`. Select held on through a restart would continue into the bootloader's USB mode.

### B. On battery

9. Unplug for at least ten minutes; an hour or a night gives better figures. The face
   keeps time, and a short press lights it.
10. Plug back in. The console comes back within a few seconds. If it has not within a
    minute, record the failure; holding Back until the watch restarts, with the cable in,
    brings USB back up.
11. `cmd power`: the input is external, and the last stretch on battery shows its length,
    mean current, charge at each end and the gauge's time to empty; `unanswered input
    reads in a row 0; Stop follows the input`. `stop2_wakes` divided
    by that length in minutes is the wake rate on battery: expect about forty a minute, from
    the time driver every 2 s, the watchdog's feed every 8 s and the minute's redraw.
    `PWR_INT_L` reads high, and `input interrupts` has gone up by two, the unplug and the
    plug. Under `[lptim]`: `alarms handled over 5 ms late 0` and `Stop entered with one
    unacknowledged 0`; a late alarm is a press whose light comes on late. Record the mean
    current, the time to empty and the wake rate; a meter in series with the cell gives the
    real floor, if you have one.

### C. The update safety net

12. `.venv/bin/python tools/wrist-update.py gate-test.bin --no-reset`, then unplug. Hold
    Back until the watch restarts: the loader swaps `gate-test` in, and it starts on
    battery with no computer to keep it. Once the face is back, wait 30 s, then hold Back
    until it restarts again: the loader puts the previous image back. Plug in. `cmd
    version` shows `build_id`, not `gate-test`, and `boot` shows
    `state boot`.

### D. Stock and back: the full-size installer's round trip

13. Back to that stock version, as in INSTALL.md ("Back to stock"). Stock starts and
    reports it.
14. The dry run: INSTALL.md steps 1 to 4, with every check there passing. The launcher
    lists **Install**, and the screen shows `INSTALL-CRC.txt`'s digits.
15. The confirm's refusals, each from a passing dry run: a click of Select; Select held for
    about two seconds and let go; Select held with Back pressed during the countdown; and
    Select held from the launcher, through `Checking`, until the ready screen appears.
    None of them installs: the screen goes back to the CRC and "Hold Select to install", and
    the watch stays on stock. Note anything else a 3 s hold of Select brings up.
16. Install: INSTALL.md steps 5 to 7. Note how long `Installing` stays on screen. The
    watch comes up in wrist-rs: `cmd "boot;version"` says `state boot` and the build.
    Set the time, and repeat steps 5 and 6.
17. Back to stock again (step 13), then a dry run: `install.log` shows the last install's
    outcome record for this CRC in the witness page, the right first page and count, and
    the staging area as the recovery left it.
18. Install the same candidate again, as in step 16: wrist-rs, `state boot`, the build.

### Record

19. Write each install into the validation records, with its output; failures too.
20. On `main`, add anything learned to HARDWARE.md.
21. In [`gate.json`](gate.json): the install numbers, `stock_version` as the version steps 13 to 18
    used, `image_crc32` as step 1 noted it, and `round_trip_validated` true only if steps 4
    to 18 all passed.
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
