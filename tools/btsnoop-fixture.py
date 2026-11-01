#!/usr/bin/env python3
"""Turn a btsnoop capture into a test fixture of HCI events that carries no keys and no addresses.

Only events about a connection's life are kept; every address in them is zeroed. Everything else in
the capture (commands, data, Command Complete, advertising reports, btmon's index and management
records) is dropped and counted on stderr. Each output line is ms since the first kept event, then
the event in hex after its packet type byte.

    tools/btsnoop-fixture.py capture.btsnoop > crates/wrist-aci/tests/captures/name.hex
"""

import argparse
import collections
import struct
import sys

MONITOR = 2001  # btmon -w
H4 = 1002  # Android's btsnoop_hci.log
MONITOR_EVENT = 3
H4_EVENT = 4

# Event code -> the byte ranges of the parameters that hold an address.
KEEP = {
    0x05: [],  # Disconnection Complete
    0x08: [],  # Encryption Change
    0x30: [],  # Encryption Key Refresh Complete
}
# LE Meta subevent -> the byte ranges after the subevent code that hold an address.
KEEP_LE = {
    0x01: [(5, 11)],  # Connection Complete: peer
    0x03: [],  # Connection Update Complete
    0x04: [],  # Read Remote Features Complete
    0x06: [],  # Remote Connection Parameter Request
    0x07: [],  # Data Length Change
    0x0A: [(5, 23)],  # Enhanced Connection Complete: peer, local RPA, peer RPA
    0x0C: [],  # PHY Update Complete
    0x14: [],  # Channel Selection Algorithm
    0x29: [(5, 23)],  # Enhanced Connection Complete v2
}


def events(data):
    """(µs, event bytes after the packet type) for every HCI event in the capture."""
    magic, _, link = struct.unpack('>8sII', data[:16])
    if magic != b'btsnoop\0' or link not in (MONITOR, H4):
        sys.exit(f'not a btmon or H4 btsnoop capture (link type {link})')
    i = 16
    while i + 24 <= len(data):
        _, length, flags, _, ts = struct.unpack('>IIIIq', data[i:i + 24])
        packet = data[i + 24:i + 24 + length]
        i += 24 + length
        if link == MONITOR and flags & 0xFFFF == MONITOR_EVENT:
            yield ts, packet
        elif link == H4 and packet[:1] == bytes([H4_EVENT]):
            yield ts, packet[1:]


def scrub(event):
    """The event with its addresses zeroed, or None when it is not kept."""
    if len(event) < 2:
        return None
    if event[0] == 0x3E:
        ranges = KEEP_LE.get(event[2]) if len(event) > 2 else None
        offset = 3
    else:
        ranges = KEEP.get(event[0])
        offset = 2
    if ranges is None:
        return None
    out = bytearray(event)
    for lo, hi in ranges:
        out[offset + lo:offset + hi] = bytes(min(hi, len(out) - offset) - lo)
    return bytes(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('capture')
    args = parser.parse_args()
    with open(args.capture, 'rb') as f:
        data = f.read()
    first = None
    dropped = collections.Counter()
    for ts, event in events(data):
        kept = scrub(event)
        if kept is None:
            dropped[f'3e/{event[2]:02x}' if event[0] == 0x3E and len(event) > 2 else f'{event[0]:02x}'] += 1
            continue
        first = ts if first is None else first
        print(f'{(ts - first) // 1000} {kept.hex()}')
    for code, n in sorted(dropped.items()):
        print(f'dropped {n} event(s) {code}', file=sys.stderr)


if __name__ == '__main__':
    main()
