# ImageInstaller

A utility app for the stock firmware that replaces the vendor kernel with wrist-rs's loader
and application, the image [`tools/combine-image.py`](../../tools/combine-image.py) makes.
It is the only destructive thing in this repository. [release/README.md](../../release/README.md)
builds it, and [release/INSTALL.md](../../release/INSTALL.md) is how to use it.

It appears in the launcher as **Install**. Its one input is `image.bin`, in this app's
folder on the watch beside the `.uapp`.

## How it runs

Like every UNA app it is two processes. The service, in [`Libs/`](Libs), does every check
and every write to flash. The GUI, in [`GUI/`](GUI) and built on the SDK's LVGL port, shows
what the service found and sends the confirm; it does not link the flash code, and the
release recipe checks that it does not.

1. **Opening Install from the launcher** starts the service, then the GUI. Once the GUI is
   up, the service runs the dry run: the isolation gate, reading `image.bin` and its
   CRC-32, the plan, the flash unlock self-test and an erase-and-program self-test on a
   scratch page in bank 2. It writes `install.log` and reports to the screen.
2. **The screen shows the verdict.** A passing dry run shows the image's CRC-32, to be
   checked against the package's `INSTALL-CRC.txt`.
3. **A hold installs, as a hold saves an activity in the stock apps.** On the ready
   screen, R1 (Select, top right) pressed and held opens "Hold to install", a 3, 2, 1 countdown with
   a ring that fills; after 3 s the GUI sends the confirm. Letting go, R2 or leaving the
   screen before then cancels, and a press that began before the ready screen appeared
   does not count. The stock activities hold for 1.5 s; replacing the firmware takes
   twice that. The rules are in [`ConfirmFlow.hpp`](GUI/gui/include/gui/ConfirmFlow.hpp),
   and the build checks them at compile time
   ([`ConfirmFlowChecks.cpp`](GUI/gui/src/ConfirmFlowChecks.cpp)).
4. **The confirm names the image by its CRC.** The service installs only if that is the
   CRC its own dry run passed in this run, and only once per run. It stages the image,
   checks it, and commits; the watch restarts into wrist-rs.

A service started any other way, with no GUI, leaves after 5 s without checking anything,
so nothing but the launcher runs even the dry run's self-test. The release recipe refuses
an installer whose header marks it as anything but a utility app, or sets the autostart or
glance bit.

There is no arm file. A file on the watch's drive would install whenever the app next
started, with nobody at the watch to see which image it named; the confirm on screen needs
the person installing to be there, and to have seen the CRC.

## What the screen says

| Screen | Meaning |
| --- | --- |
| `Checking` | The dry run is under way. |
| `XXXXXXXX`, "Dry run passed" | Ready: the image with that CRC-32 passed every check. Holding R1 installs it. |
| "Hold to install", 3, 2, 1 | R1 is held; letting go cancels. |
| `Refused`, "isolates apps" | The firmware isolates apps; nothing was attempted. |
| `No image` | `image.bin` is missing, empty or unreadable. |
| `Unlock N` | The flash unlock self-test failed. |
| `Flash NN` | The erase-and-program self-test on the scratch page failed (its result bits). |
| `Refused -N` | A check refused the image: `plan::Refusal` in [`crates/wrist-flash`](../../crates/wrist-flash), or the `kErr` codes in [`Libs/Header/Service.hpp`](Libs/Header/Service.hpp). After a confirm, staging may have written bank 2. |
| `Installing` | Confirmed: staging, then the commit. |
| (the watch restarts) | It installed. |

None of the others has touched the stock firmware.

## How it stays safe

- The isolation gate ([`IsolationGate.hpp`](Libs/Header/IsolationGate.hpp)) refuses
  unless the service runs privileged with the MPU off and TrustZone off, reading
  `CONTROL`, then `MPU_CTRL`, then `FLASH_OPTR`, an order in which no read can fault. It
  runs before the dry run and again before staging.
- The image is staged in bank 2 and checked against the confirmed CRC while the kernel's
  filesystem, which the commit erases, still exists.
- The commit copies flash to flash from code already in RAM, with interrupts masked; the
  release recipe refuses a build whose commit path branches outside RAM.
- The kernel write starts at page 48 or not at all (`plan::Refusal::WouldEraseBootloader`).
- Every wait is bounded, and every erase and program is read back.

`install.log` and `stage.log` stay on the watch's storage. The outcome record a commit
leaves in the bootloader partition's tail is defined in [`crates/wrist-flash/src/hw.rs`](../../crates/wrist-flash/src/hw.rs),
and the next dry run reports it. UNA's recovery leaves it in place, so a dry run after a
return to stock reports the last install.

## Building

The release recipe builds it against the SDK commit in
[`release/toolchain.json`](../../release/toolchain.json), with the SDK's `ThirdParty/lvgl`
submodule checked out. The screen draws in LVGL's built-in Montserrat 14, 28 and 48,
enabled in [`GUI/lv_conf.h`](GUI/lv_conf.h). [`Resources/make-icons.py`](Resources/make-icons.py)
draws the launcher icons a utility app needs.
