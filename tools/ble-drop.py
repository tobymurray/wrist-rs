#!/usr/bin/env python3
"""Drop the watch's link the way a phone does when its Bluetooth is switched off: block this
host's Bluetooth with rfkill for some seconds, then unblock it. Linux; needs write access to
/dev/rfkill (the `rfkill` group, or root).

    tools/ble-drop.py 10                               block for 10 s
    tools/ble-drop.py 10 --command-file cmd.txt        and mark both ends there (`== host ...`)

BlueZ disconnects before the adapter goes down, so this is a clean hang-up, not a link lost out of range.
"""
import argparse
import signal
import subprocess
import sys
import time


def rfkill(action):
    p = subprocess.run(['rfkill', action, 'bluetooth'], capture_output=True, text=True)
    if p.returncode != 0:
        sys.exit(f'ble-drop.py: rfkill {action} bluetooth failed: {p.stderr.strip() or p.returncode}')


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('seconds', type=float, help='how long the radio stays blocked')
    ap.add_argument('--command-file', help="a logger's command file, for the marks")
    a = ap.parse_args()
    if a.seconds < 0:
        ap.error('seconds must not be negative')

    def mark(text):
        print(f'-- {time.strftime("%H:%M:%S")} {text}', flush=True)
        if a.command_file:
            with open(a.command_file, 'a') as f:
                f.write(f'== host {text}\n')

    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda *_: sys.exit(1))
    rfkill('block')
    try:
        mark(f'radio blocked for {a.seconds:g} s')
        time.sleep(a.seconds)
    finally:
        rfkill('unblock')
        mark('radio unblocked')


if __name__ == '__main__':
    main()
