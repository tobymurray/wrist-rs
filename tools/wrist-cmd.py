#!/usr/bin/env python3
"""Send console commands, separated by ;, and print the watch's reply as it comes.

    wrist-cmd.py [--port /dev/ttyACM0] "<cmd>[;<cmd>...]" [seconds to listen]
    wrist-cmd.py --follow "ble"       ... and keep printing until Ctrl-C

Each line is printed when the watch sends it, so a pairing number on the console can be
answered while it is still asked (`wrist-cmd.py "ble yes"` from another terminal).
"""
import argparse
import codecs
import sys
import time

try:
    import serial  # noqa: F401
except ImportError:
    sys.exit("pyserial is needed: pip install pyserial")

import wrist_port


def relay(read, write, until):
    """Copy what `read` gives to `write` as it comes, until `until()` says stop; UTF-8 split across
    reads is joined before it is shown."""
    text = codecs.getincrementaldecoder('utf-8')('replace')
    while not until():
        chunk = read()
        if chunk:
            write(text.decode(chunk))
    write(text.decode(b'', final=True))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--port')
    ap.add_argument('--follow', action='store_true', help='keep printing until Ctrl-C')
    ap.add_argument('commands')
    ap.add_argument('seconds', nargs='?', type=float, default=3)
    args = ap.parse_args()
    s = wrist_port.open_console(args.port)
    for c in args.commands.split(';'):
        wrist_port.write_command(s, c)
        time.sleep(0.3)
    end = time.time() + args.seconds

    def write(text):
        sys.stdout.write(text)
        sys.stdout.flush()
    try:
        relay(lambda: s.read(4096), write, lambda: not args.follow and time.time() >= end)
    except KeyboardInterrupt:
        pass


if __name__ == '__main__':
    main()
