#!/usr/bin/env python3
"""Turn a `ble-trace` capture into a replay fixture that carries no address and no console text.

    tools/ble-trace-fixture.py capture.log > crates/wrist-ble-sim/fixtures/traces/name.log

Kept: chip resets; whether the core drives, and the driver's lost reads and snapshots, which
carry no address; the driver's commands that are one of the core's operations; the chip's
events but advertising reports, console writes and replies to other commands (one lists the
bonded addresses), with a connection's peer and resolvable addresses zeroed and a reply cut after
its status; the console commands the replay reads
(its goals, answers, `ble disconnect` and a USB `update`); the `[update]` lines that bound an
image's transfer; scenario marks, with anything shaped like an address replaced. Every line keeps its host time, as seconds or the wall clock's HH:MM:SS.mmm.
"""

import argparse
import re
import sys

# The driver's commands the replay reads as operations (wrist-bluenrg2's shadow.rs, driver_op).
OPS = {0xFC83, 0xFC81, 0xFD81, 0xFC8D, 0xFC95, 0xFCA5, 0xFC88, 0xFC93, 0xFC97, 0xFC9D}
GOALS = re.compile(r'^ble (on|off|yes|no|disconnect|profile (auto|cable|battery))$')
DECIMAL = re.compile(r'^\+?[0-9]+$')
HEX = re.compile(r'^\+?[0-9A-Fa-f]+$')
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
    if len(packet) < 3 or packet[0] != EVENT:
        return None
    if packet[1] in (COMMAND_COMPLETE, COMMAND_STATUS):
        # Complete: ncmd, opcode, status; Status: status, ncmd, opcode.
        if len(packet) < 7:
            return None
        opcode = packet[4:6] if packet[1] == COMMAND_COMPLETE else packet[5:7]
        if int.from_bytes(opcode, 'little') not in OPS:
            return None
        return packet[:2] + bytes([4]) + packet[3:7]
    if packet[1] == VENDOR and len(packet) >= 5 and int.from_bytes(packet[3:5], 'little') == ATTRIBUTE_MODIFIED:
        return None
    if packet[1] == LE_META and len(packet) >= 4:
        if packet[3] == REPORT:
            return None
        if packet[3] in ADDRESSES:
            lo, hi = ADDRESSES[packet[3]]
            packet[lo:hi] = bytes(len(packet[lo:hi]))
    return packet


def usb_update(cmd):
    """`cmd` as `parse_transfer` in the firmware's console.rs reads it, normalised, or None."""
    words = cmd.split()
    if len(words) < 3 or words[0] != 'update' or not cmd.startswith('update '):
        return None
    length, crc = words[1], words[2]
    if not (DECIMAL.match(length) and int(length) < 1 << 32 and HEX.match(crc) and int(crc, 16) < 1 << 32):
        return None
    return f'update {int(length)} {int(crc, 16):X}'


def snapshot(packet):
    """Whether `packet` is a snapshot as wrist-bluenrg2's shadow/trace.rs carries it: flags, then each
    link's handle, role and encryption."""
    links = packet[1:]
    return (len(packet) >= 1 and packet[0] <= 0b111 and len(links) % 4 == 0 and len(links) <= 32
            and all(int.from_bytes(links[i:i + 2], 'little') <= 0x0EFF and links[i + 2] <= 1
                    and links[i + 3] <= 2 for i in range(0, len(links), 4)))


def scrub(line):
    """The line as the fixture keeps it, or None."""
    m = LINE.match(line)
    if not m:
        return None
    host, text = m.groups()
    text = text.strip()
    if text.startswith('== ') or text.startswith('[update] '):
        return f'{host} {ADDRESS.sub("<address>", text)}'
    if text.startswith('>> usb '):
        cmd = text[len('>> usb '):].strip()
        if GOALS.match(cmd):
            return f'{host} >> usb {cmd}'
        update = usb_update(cmd)
        return f'{host} >> usb {update}' if update else None
    fields = text.split()
    if len(fields) < 3 or fields[0] != '[trace]':
        return None
    ms, kind = fields[1], fields[2]
    if kind in ('r', 'l') or (kind == 's' and len(fields) == 3):
        return f'{host} [trace] {ms} {kind}'
    if len(fields) < 4 or fields[3].startswith('+'):
        return None
    packet = bytearray.fromhex(fields[3])
    if kind == 'c':
        keep = len(packet) >= 3 and int.from_bytes(packet[1:3], 'little') in OPS
    elif kind == 'e':
        packet = event(packet)
        keep = packet is not None
    elif kind == 's':
        keep = snapshot(packet)
    elif kind == 'm':
        keep = packet in (b'\x00', b'\x01')
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
