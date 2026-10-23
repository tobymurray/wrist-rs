#!/usr/bin/env python3
"""Send a new application, or with --loader a new loader, over the USB console (apps/wrist-loader/LAYOUT.md).

    tools/wrist-update.py app.bin [--port /dev/ttyACM0] [--no-reset] [--loader]
"""
import argparse, os, struct, sys, time, zlib

try:
    import serial
except ImportError:
    sys.exit("pyserial is needed: pip install pyserial")

import wrist_port

LOADER = (0x08060000, 32 * 1024)
APPLICATION = (0x08068000, 512 * 1024)

def vectors_ok(data, base, size):
    """The watch's own check, crates/wrist-flash/src/vectors.rs."""
    if len(data) < 8:
        return False
    sp, reset = struct.unpack_from('<II', data)
    return 0x20000000 <= sp <= 0x2026FFF0 and reset & 1 == 1 and base <= reset & ~1 < base + size

def mismatch(data, loader):
    """Why the file is not an image for the slot it is going to, or None."""
    as_loader = vectors_ok(data, *LOADER)
    as_app = vectors_ok(data, *APPLICATION)
    if as_loader and len(data) > LOADER[1]:
        return "this is the combined installer image, not an application or a loader; pick app.bin, or loader.bin with --loader"
    if as_loader if loader else as_app:
        return None
    if as_loader:
        return "this is a loader image, not an application; add --loader or pick app.bin"
    if as_app:
        return "this is an application image, not a loader; drop --loader or pick loader.bin"
    return "this is not a firmware image; pick app.bin, or loader.bin with --loader"

def wait_for(port, needles, timeout, echo=True):
    """(needle, line) for the first line holding a needle, or (None, None) at the timeout."""
    deadline = time.time() + timeout
    buf = b''
    while time.time() < deadline:
        chunk = port.read(4096)
        if not chunk:
            continue
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            text = line.decode('ascii', 'replace').rstrip('\r')
            if echo and text and '[hb]' not in text:
                print("   ", text, flush=True)
            for n in needles:
                if n in text:
                    return n, text
    return None, None

def reopen(explicit, timeout):
    """The reopened console, or None if it does not enumerate in time; a named port that
    does not come back (the watch now has another USB identity) is looked up by ID."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        name = wrist_port.find(explicit)
        if name and not os.path.exists(name):
            name = wrist_port.find(None)
        if name:
            try:
                return serial.Serial(name, 115200, timeout=0.5)
            except serial.SerialException:
                pass
        time.sleep(1)
    return None

def wait_until_booted(s, timeout):
    """(whether `boot` said `state boot`, whether it said revert on the way)."""
    deadline = time.time() + timeout
    reverted = False
    while time.time() < deadline:
        s.write(b"boot\r\n")
        _, line = wait_for(s, ["[boot] state"], 3, echo=False)
        if line:
            reverted |= "revert" in line
            if line.rstrip().endswith("state boot"):
                return True, reverted
        time.sleep(2)
    return False, reverted

# What a lost port means at each stage, for the one-line message in place of a traceback.
LOST = "the port went away before the whole image was sent; the watch abandons the transfer after 5 s and keeps its current version: plug it back in and run this again"

def main():
    global LOST
    ap = argparse.ArgumentParser()
    ap.add_argument('image')
    ap.add_argument('--port')
    ap.add_argument('--no-reset', action='store_true')
    ap.add_argument('--loader', action='store_true',
                    help='the image is a new wrist-loader: written over the loader pages in place, no swap, no mark')
    ap.add_argument('--pace', type=float, default=0.005,
                    help='seconds between 64-byte packets (default 0.005; the watch drops bursts, see LAYOUT.md)')
    args = ap.parse_args()
    data = open(args.image, 'rb').read()
    why = mismatch(data, args.loader)
    if why:
        sys.exit(why)
    crc = zlib.crc32(data) & 0xffffffff
    name = wrist_port.find_or_exit(args.port)
    print(f"image {len(data)} bytes, CRC-32 {crc:08X}, port {name}")
    s = serial.Serial(name, 115200, timeout=0.5, write_timeout=5)
    time.sleep(1.0)
    s.reset_input_buffer()
    tag = "[loader]" if args.loader else "[update]"
    verb = "loader" if args.loader else "update"
    s.write(f"{verb} {len(data)} {crc:08X}\r\n".encode())
    hit, _ = wait_for(s, [f"{tag} send", f"{tag} error"], 30)
    if hit != f"{tag} send":
        sys.exit(f"the watch did not accept the {verb}")
    t0 = time.time()
    step = 64 if args.pace > 0 else 4096
    for off in range(0, len(data), step):
        try:
            s.write(data[off:off + step])
            s.flush()
        except serial.SerialTimeoutException:
            sys.exit(f"the watch stopped accepting data after {off} bytes (write timed out)")
        if args.pace > 0:
            time.sleep(args.pace)
        if off % 8192 == 0:
            print(f"    sent {off}", flush=True)
            s.timeout = 0
            pending = s.read(65536)
            s.timeout = 0.5
            for text in pending.decode('ascii', 'replace').splitlines():
                if tag in text:
                    print("   ", text, flush=True)
    print(f"    sent {len(data)} (all)", flush=True)
    LOST = ("the port went away after the whole image was sent, which the watch may have checked and marked "
            "to install at the next reset: plug it back in and run `boot` with tools/wrist-cmd.py; "
            "`state boot` means nothing was marked, so run this again, and `state swap` means `reset` installs it")
    hit, line = wait_for(s, [f"{tag} ok", f"{tag} error"], 180)
    if hit != f"{tag} ok":
        sys.exit("the watch reported an error, or went quiet")
    print(f"written and verified in {time.time() - t0:.1f} s")
    if args.no_reset:
        return
    LOST = "the port went away after the reset: do not reset the watch; check `boot` with tools/wrist-cmd.py until it reports `state boot`"
    s.write(b"reset\r\n")
    s.close()
    print("reset sent; waiting for the watch to come back")
    time.sleep(3)
    s = reopen(args.port, 60)
    if s is None:
        sys.exit("the watch did not come back within 60 s; the loader may have reverted, or it needs the stock route")
    time.sleep(1.0)
    s.reset_input_buffer()
    s.write(b"version\r\n")
    wait_for(s, ["standalone-boot"], 10)
    print("waiting for the new version to mark itself booted (about 15 s); do not reset the watch")
    booted, reverted = wait_until_booted(s, 60)
    s.close()
    if reverted:
        sys.exit("the loader reverted to the previous version: the new one did not mark itself booted")
    if not booted:
        sys.exit("no `state boot` within 60 s: do not reset; check `boot` again with tools/wrist-cmd.py")
    print("done: state boot, the new version is kept")

if __name__ == '__main__':
    try:
        main()
    except serial.SerialException as e:
        sys.exit(f"{LOST} ({e})")
