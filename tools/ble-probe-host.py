#!/usr/bin/env python3
"""The host's side of the probe build's chip probes (`--features ble-probe`).

    ble-probe-host.py matrix [--out FILE] [--from N]
        Hold a link to the watch from this computer (BlueZ accepts each interval the watch asks
        for) while `ble probe matrix` runs over USB; reconnect and resume if the link goes. Each
        cell's line goes to FILE, the fixture `crates/wrist-bluenrg2/tests/fixtures/bluenrg2-probe.txt`
        by default.

    ble-probe-host.py ll [--control]
        Run `ble probe ll` (`ble probe ll control`) over USB, then find `UNA-LL` and connect to it: does the chip's own host
        answer this computer's ATT requests, or do they reach the watch as ACL data? The watch's
        packets are printed as they come; BlueZ's entry for the probe's address is removed after.

    ble-probe-host.py bonds --watch D9:5B:7A:EB:29:82 [--transport hci-socket:0] [--most 40]
        Pair from one throwaway static random address after another (`C0:B0:4D:00:00:01` up),
        answering the watch's comparison over USB, until the watch's `ble bonds` count stops
        growing: the size of its bond database. Each throwaway bond is forgotten after, by `ble
        forget`, and the bonds the watch had before are checked to be there still. Runs on Bumble,
        which takes the adapter from BlueZ: as root, with BlueZ's adapter powered off (the step
        does `btmgmt power off` first and `btmgmt power on` after).

The watch is on USB (`--port`, else found by its USB ID) and reached over Bluetooth by `--name`.
Needs bleak and pyserial; `bonds` needs bumble.
"""
import argparse
import asyncio
from pathlib import Path
import re
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wrist_port  # noqa: E402

FIXTURE = Path(__file__).resolve().parent.parent / 'crates/wrist-bluenrg2/tests/fixtures/bluenrg2-probe.txt'
CELL = re.compile(r'\[ble\] (probe matrix cell=\d+ .*)$')
WENT = re.compile(r'the link went in cell (\d+)')
LL_ADDRESS = 'C0:2C:00:00:00:01'
# Longer than any cell takes (`UPDATE_WITHIN_MS` and `HOLD_MS`, apps/standalone-boot/src/ble/probe.rs).
CELL_WITHIN_S = 40


def log(text):
    print(f"-- {time.strftime('%H:%M:%S')} {text}", flush=True)


class Lines:
    """The USB console as whole lines; heartbeats left out."""

    def __init__(self, port):
        self.s = wrist_port.open_console(port, timeout=0.2)
        self.part = b''

    def send(self, text):
        wrist_port.write_command(self.s, text)

    def next(self, seconds):
        """The next line within `seconds`, or None."""
        end = time.time() + seconds
        while time.time() < end:
            if b'\n' in self.part:
                line, self.part = self.part.split(b'\n', 1)
                text = line.decode('utf-8', 'replace').rstrip('\r')
                if text and not text.startswith('[hb]'):
                    return text
                continue
            self.part += self.s.read(max(1, self.s.in_waiting))
        return None


async def lines_until(usb, done, seconds):
    """Lines printed until one satisfies `done`, or `seconds` pass with no line; that line or None."""
    while True:
        text = await asyncio.to_thread(usb.next, seconds)
        if text is None:
            return None
        print(f'   usb: {text}', flush=True)
        if done(text):
            return text


async def hold_link(name):
    from bleak import BleakClient, BleakScanner
    device = await BleakScanner.find_device_by_name(name, timeout=20)
    if device is None:
        return None
    client = BleakClient(device, timeout=20)
    await client.connect()
    return client


async def matrix(args):
    usb = Lines(args.port)
    cells = []
    start = args.start
    for attempt in range(5):
        log(f'connecting to {args.name}, attempt {attempt + 1}')
        try:
            client = await hold_link(args.name)
        except Exception as e:
            log(f'connect: {e!r}')
            client = None
        if client is None:
            await asyncio.sleep(5)
            continue
        # The link's own start (encryption, the core's first ask) over before the first cell.
        await asyncio.sleep(args.settle)
        usb.send(f'ble probe matrix from {start}' if start else 'ble probe matrix')
        resume = None
        while True:
            text = await lines_until(usb, lambda t: 'probe matrix' in t, CELL_WITHIN_S)
            if text is None:
                log('no line from the matrix in time')
                break
            if m := CELL.search(text):
                cells.append(m.group(1))
            elif m := WENT.search(text):
                resume = int(m.group(1))
                break
            elif 'probe matrix: done' in text:
                break
            elif 'probe matrix:' in text:
                log(f'the watch refused: {text}')
                break
        try:
            await client.disconnect()
        except Exception:
            pass
        if resume is None:
            break
        start = resume
        await asyncio.sleep(3)
    header = [
        '# `ble probe matrix` on the watch, the Linux host the central (tools/ble-probe-host.py matrix).',
        f'# {time.strftime("%Y-%m-%d")}; one line per cell: each change in the order made, the anchor period after each.',
    ]
    Path(args.out).write_text('\n'.join(header + cells) + '\n')
    log(f'{len(cells)} cells written to {args.out}')
    return 0 if len(cells) == 60 else 1


async def link_layer(args):
    from bleak import BleakClient, BleakScanner
    usb = Lines(args.port)
    usb.send('ble probe ll control' if args.control else 'ble probe ll')
    started = await lines_until(usb, lambda t: 'advertising as UNA-LL' in t or 'probe ll: end' in t or 'no boot' in t, 20)
    if started is None or 'UNA-LL' not in started:
        log('the watch is not advertising UNA-LL')
        await lines_until(usb, lambda t: 'probe ll: the chip reset' in t, 10)
        return 1
    log('looking for UNA-LL')
    device = await BleakScanner.find_device_by_name('UNA-LL', timeout=30)
    if device is None:
        log('UNA-LL not seen')
    else:
        log(f'found {device.address}; connecting')
        client = BleakClient(device, timeout=25)
        try:
            await client.connect()
            log('connected; services as discovered:')
            for s in client.services:
                print(f'   service {s.uuid} handle 0x{s.handle:04x}', flush=True)
                for c in s.characteristics:
                    print(f'     characteristic {c.uuid} handle 0x{c.handle:04x} {",".join(c.properties)}', flush=True)
        except Exception as e:
            log(f'connect or discovery: {e!r}')
        try:
            await client.disconnect()
        except Exception:
            pass
    await lines_until(usb, lambda t: 'probe ll: the chip reset' in t, 90)
    await lines_until(usb, lambda t: 'advertising' in t, 20)
    subprocess.run(['timeout', '10', 'bluetoothctl', 'remove', LL_ADDRESS], capture_output=True)
    return 0


BOND = re.compile(r'([0-9A-F]{2}(?::[0-9A-F]{2}){5})( \(random\))?')


async def bond_list(usb):
    """`ble bonds`' addresses, each as `ble forget` takes it."""
    usb.send('ble bonds')
    text = await lines_until(usb, lambda t: 'bonded devices' in t or 'no bonded' in t, 10) or ''
    listed = text.split('bonded devices:', 1)[-1] if 'bonded devices:' in text else ''
    return [a + (r or '') for a, r in BOND.findall(listed)]


async def bonds(args):
    from bumble import hci
    from bumble.core import BT_LE_TRANSPORT
    from bumble.device import Device, DeviceConfiguration
    from bumble.keys import MemoryKeyStore
    from bumble.pairing import PairingConfig, PairingDelegate
    from bumble.transport import open_transport

    class Yes(PairingDelegate):
        def __init__(self):
            super().__init__(io_capability=PairingDelegate.IoCapability.DISPLAY_OUTPUT_AND_YES_NO_INPUT)

        async def compare_numbers(self, number, digits):
            return True

        async def confirm(self, auto=False):
            return True

    usb = Lines(args.port)
    before = await bond_list(usb)
    log(f'bonded before: {len(before)} {" ".join(before)}')
    watch = hci.Address(args.watch, hci.Address.RANDOM_DEVICE_ADDRESS)
    subprocess.run(['btmgmt', '--index', '0', 'power', 'off'], capture_output=True)
    counts, misses = [], 0
    try:
        async with await open_transport(args.transport) as (source, sink):
            for i in range(1, args.most + 1):
                address = f'C0:B0:4D:00:00:{i:02X}'
                config = DeviceConfiguration(name='bond-probe', address=hci.Address(address, hci.Address.RANDOM_DEVICE_ADDRESS), irk=bytes([i]) * 16)
                device = Device.from_config_with_hci(config, source, sink)
                device.keystore = MemoryKeyStore()
                device.pairing_config_factory = lambda _c: PairingConfig(sc=True, mitm=True, bonding=True, delegate=Yes())
                await device.power_on()
                outcome = 'no link'
                try:
                    connection = await device.connect(watch, transport=BT_LE_TRANSPORT, own_address_type=hci.OwnAddressType.RANDOM, timeout=15)
                    pairing = asyncio.ensure_future(connection.pair())
                    shown = await lines_until(usb, lambda t: 'the central shows a number' in t or 'pairing failed' in t, 20)
                    if shown and 'shows a number' in shown:
                        usb.send('ble yes')
                    try:
                        await asyncio.wait_for(pairing, 30)
                        outcome = 'paired'
                    except Exception as e:
                        outcome = f'pairing failed: {e!r}'
                    await lines_until(usb, lambda t: t.startswith('[ble] bonded') or 'pairing failed' in t, 5)
                    try:
                        await connection.disconnect()
                    except Exception:
                        pass
                    await lines_until(usb, lambda t: 'disconnected' in t, 10)
                except Exception as e:
                    outcome = f'connect: {e!r}'
                now = await bond_list(usb)
                counts.append(len(now))
                log(f'{address}: {outcome}; the watch lists {len(now)} bonds')
                lost = [b for b in before if b not in now]
                if lost:
                    log(f'bonds from before gone: {" ".join(lost)}')
                grew = len(counts) < 2 or counts[-1] > counts[-2]
                misses = 0 if grew else misses + 1
                if misses >= 2:
                    break
    finally:
        subprocess.run(['btmgmt', '--index', '0', 'power', 'on'], capture_output=True)
        after = await bond_list(usb)
        for b in after:
            if b.startswith('C0:B0:4D:00:00:'):
                usb.send(f'ble forget {b}')
                await lines_until(usb, lambda t: 'forg' in t, 5)
        final = await bond_list(usb)
        log(f'most bonds listed: {max(counts, default=0)}; after the forgets: {len(final)} {" ".join(final)}')
        missing = [b for b in before if b not in final]
        if missing:
            log(f'NOT BACK: {" ".join(missing)}: pair them again')
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--port')
    ap.add_argument('--name', default=wrist_port.NAME)
    sub = ap.add_subparsers(dest='what', required=True)
    m = sub.add_parser('matrix')
    m.add_argument('--out', default=str(FIXTURE))
    m.add_argument('--from', dest='start', type=int, default=0)
    m.add_argument('--settle', type=float, default=8, help='seconds between connecting and the first cell')
    ll = sub.add_parser('ll')
    ll.add_argument('--control', action='store_true', help="the same boot with the chip's host kept (0x2C written 0)")
    b = sub.add_parser('bonds')
    b.add_argument('--watch', required=True, help="the watch's address, as `ble` prints it")
    b.add_argument('--transport', default='hci-socket:0')
    b.add_argument('--most', type=int, default=40)
    args = ap.parse_args()
    run = {'matrix': matrix, 'll': link_layer, 'bonds': bonds}[args.what]
    return asyncio.run(run(args))


if __name__ == '__main__':
    sys.exit(main())
