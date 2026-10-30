#!/usr/bin/env python3
"""Send console commands, separated by ;, and print the watch's reply.

    wrist-cmd.py [--port /dev/ttyACM0] "<cmd>[;<cmd>...]" [seconds to listen]
"""
import argparse, sys, time

try:
    import serial
except ImportError:
    sys.exit("pyserial is needed: pip install pyserial")

import wrist_port

ap = argparse.ArgumentParser()
ap.add_argument('--port')
ap.add_argument('commands')
ap.add_argument('seconds', nargs='?', type=float, default=3)
args = ap.parse_args()
s = wrist_port.open_console(args.port)
for c in args.commands.split(';'):
    wrist_port.write_command(s, c)
    time.sleep(0.3)
end = time.time() + args.seconds; buf = b''
while time.time() < end:
    buf += s.read(4096)
sys.stdout.write(buf.decode('utf-8', 'replace'))
