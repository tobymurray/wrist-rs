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
s = serial.Serial(wrist_port.find_or_exit(args.port), 115200, timeout=0.2)
time.sleep(0.3); s.reset_input_buffer()
for c in args.commands.split(';'):
    data = (c.strip() + '\r\n').encode()
    for i in range(0, len(data), 60):
        s.write(data[i:i + 60]); s.flush(); time.sleep(0.005)
    time.sleep(0.3)
end = time.time() + args.seconds; buf = b''
while time.time() < end:
    buf += s.read(4096)
sys.stdout.write(buf.decode('utf-8', 'replace'))
