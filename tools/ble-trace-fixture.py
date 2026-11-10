#!/usr/bin/env python3
"""Turn a `ble-trace` capture into a replay fixture that carries no address and no console text.

    tools/ble-trace-fixture.py capture.log > crates/wrist-ble-sim/fixtures/traces/name.log

Kept: chip resets; the driver's commands that are one of the core's operations; the chip's
events but its replies (one lists the bonded addresses), advertising reports and console writes,
with a connection's peer and resolvable addresses zeroed; the console commands the replay reads
(its goals, answers and `ble disconnect`); scenario marks, with anything shaped like an address
replaced. Every line keeps its host time, as seconds or the wall clock's HH:MM:SS.mmm.
"""

import argparse
import re
import sys

# The driver's commands the replay reads as operations (wrist-ble-sim's replay.rs, driver_op).
OPS = {0xFC83, 0xFC81, 0xFD81, 0xFC8D, 0xFC95, 0xFCA5, 0xFC88, 0xFC93, 0xFC97, 0xFC9D}
GOALS = re.compile(r'^ble (on|off|yes|no|disconnect|profile (auto|cable|battery))$')
LINE = re.compile(r'^\s*((?:\d{2}:\d{2}:\d{2}|\d+)\.\d{3}) (.*)$')
# Six bytes, with or without separators.
ADDRESS = re.compile(r'(?<![0-9A-Fa-f])[0-9A-Fa-f]{2}(?:[:_-]?[0-9A-Fa-f]{2}){5,}(?![0-9A-Fa-f])')

EVENT, LE_META, VENDOR, COMMAND_COMPLETE, COMMAND_STATUS = 0x04, 0x3E, 0xFF, 0x0E, 0x0F
REPORT, CONNECTION_COMPLETE, ENHANCED_CONNECTION_COMPLETE = 0x02, 0x01, 0x0A
ATTRIBUTE_MODIFIED = 0x0C01
# Packet offsets: type, code, length, subevent, status, handle (2), role, peer type, then the
# peer's address and, in the enhanced event, the local and peer resolvable addresses.
ADDRESSES = {CONNECTION_COMPLETE: (9, 15), ENHANCED_CONNECTION_COMPLETE: (9, 27)}


def event(packet):
    """The event scrubbed, or None when it is not kept."""
    if len(packet) < 3 or packet[0] != EVENT or packet[1] in (COMMAND_COMPLETE, COMMAND_STATUS):
        return None
    if packet[1] == VENDOR and len(packet) >= 5 and int.from_bytes(packet[3:5], 'little') == ATTRIBUTE_MODIFIED:
        return None
    if packet[1] == LE_META and len(packet) >= 4:
        if packet[3] == REPORT:
            return None
        if packet[3] in ADDRESSES:
            lo, hi = ADDRESSES[packet[3]]
            packet[lo:hi] = bytes(len(packet[lo:hi]))
    return packet


def scrub(line):
    """The line as the fixture keeps it, or None."""
    m = LINE.match(line)
    if not m:
        return None
    host, text = m.groups()
    text = text.strip()
    if text.startswith('== '):
        return f'{host} {ADDRESS.sub("<address>", text)}'
    if text.startswith('>> usb '):
        cmd = text[len('>> usb '):].strip()
        return f'{host} >> usb {cmd}' if GOALS.match(cmd) else None
    fields = text.split()
    if len(fields) < 3 or fields[0] != '[trace]':
        return None
    ms, kind = fields[1], fields[2]
    if kind == 'r':
        return f'{host} [trace] {ms} r'
    if len(fields) < 4 or fields[3].startswith('+'):
        return None
    packet = bytearray.fromhex(fields[3])
    if kind == 'c':
        keep = len(packet) >= 3 and int.from_bytes(packet[1:3], 'little') in OPS
    elif kind == 'e':
        packet = event(packet)
        keep = packet is not None
    else:
        keep = False
    if not keep:
        return None
    return f'{host} [trace] {ms} {kind} {packet.hex().upper()}'


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('capture')
    args = parser.parse_args()
    with open(args.capture) as f:
        for line in f:
            kept = scrub(line)
            if kept is not None:
                print(kept)


if __name__ == '__main__':
    main()
