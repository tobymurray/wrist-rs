#!/usr/bin/env python3
"""Drive a `ble-trace` image through a scenario and log its USB console for the replay.

    tools/ble-trace-capture.py cable capture.log      bring-up, connects, interval asks, a scan,
                                                       both sides hanging up, the radio off mid-link
    tools/ble-trace-capture.py battery capture.log    the battery timing on and off a live link

The watch needs `ble on` and a bond with this host, whose Bluetooth plays the phone; the tool
holds the USB console for the whole run and prefixes each line with the host's seconds. Then
`tools/ble-trace-fixture.py` makes a fixture of it, and `cargo run -p wrist-ble-sim --bin replay`
replays it. Needs bleak and pyserial.
"""
import argparse
import asyncio
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wrist_port  # noqa: E402
from bleak import BleakClient, BleakScanner  # noqa: E402

TX = '6e400003-b5a3-f393-e0a9-e50e24dcca9e'
RX = '6e400002-b5a3-f393-e0a9-e50e24dcca9e'


class Run:
    def __init__(self, out):
        self.out = out
        self.con = wrist_port.open_console()
        self.t0 = time.time()
        threading.Thread(target=self.read, daemon=True).start()

    def log(self, line):
        self.out.write(f'{time.time() - self.t0:9.3f} {line}\n')
        self.out.flush()

    def read(self):
        buf = b''
        while True:
            buf += self.con.read(4096)
            while b'\n' in buf:
                line, buf = buf.split(b'\n', 1)
                self.log(line.decode('utf-8', 'replace').rstrip())

    def usb(self, command):
        self.log(f'>> usb {command}')
        wrist_port.write_command(self.con, command)

    def mark(self, what):
        self.log(f'== {what}')
        print(f'{time.time() - self.t0:7.1f} {what}', flush=True)

    async def restart(self):
        """From the chip's reset, so the replay sees it come up."""
        self.usb('ble off')
        await asyncio.sleep(3)
        self.usb('ble on')
        await asyncio.sleep(8)


async def connect(name):
    device = await BleakScanner.find_device_by_name(name, timeout=20)
    if device is None:
        sys.exit(f'{name} is not advertising')
    client = BleakClient(device)
    await client.connect()
    await client.start_notify(TX, lambda handle, data: None)
    return client


async def hang_up(client):
    try:
        await client.disconnect()
    except Exception as e:  # the watch may have gone first
        print(f'host disconnect: {e}')


async def cable(r, name):
    r.usb('ble profile cable')
    r.mark('radio off and on: bring-up and advertising')
    await r.restart()
    r.mark('bonded connect, encrypted, console over the air')
    c = await connect(name)
    await asyncio.sleep(3)
    await c.write_gatt_char(RX, b'uptime\n', response=True)
    await asyncio.sleep(2)
    r.mark('interval asks: 15 ms, 7.5 ms, 45 ms')
    for ask in ['ble interval now 12 12', 'ble interval now 6 6', 'ble interval now 24 24']:
        r.usb(ask)
        await asyncio.sleep(6)
    r.mark('central hangs up')
    await hang_up(c)
    await asyncio.sleep(5)
    r.mark('reconnect, hold 20 s, hang up')
    c = await connect(name)
    await asyncio.sleep(20)
    await hang_up(c)
    await asyncio.sleep(5)
    r.mark('scan 5 s while advertising')
    r.usb('ble scan 5')
    await asyncio.sleep(9)
    r.mark('watch drops the link')
    c = await connect(name)
    await asyncio.sleep(4)
    r.usb('ble disconnect')
    await asyncio.sleep(6)
    await hang_up(c)
    r.mark('radio off while connected, then on')
    c = await connect(name)
    await asyncio.sleep(4)
    r.usb('ble off')
    await asyncio.sleep(4)
    await hang_up(c)
    r.usb('ble on')
    await asyncio.sleep(8)


async def battery(r, name):
    r.usb('ble profile cable')
    await r.restart()
    r.mark('bonded connect on cable')
    c = await connect(name)
    await asyncio.sleep(6)
    r.mark('profile battery for 20 s')
    r.usb('ble profile battery')
    await asyncio.sleep(20)
    r.mark('profile cable')
    r.usb('ble profile cable')
    await asyncio.sleep(10)
    r.mark('hang up, reconnect on battery')
    await hang_up(c)
    await asyncio.sleep(4)
    r.usb('ble profile battery')
    await asyncio.sleep(2)
    c = await connect(name)
    await asyncio.sleep(20)
    await hang_up(c)
    await asyncio.sleep(4)
    r.usb('ble profile cable')
    await asyncio.sleep(2)


SCENARIOS = {'cable': cable, 'battery': battery}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('scenario', choices=SCENARIOS)
    ap.add_argument('out')
    ap.add_argument('--name', default='UNA')
    a = ap.parse_args()
    with open(a.out, 'w') as out:
        r = Run(out)
        asyncio.run(SCENARIOS[a.scenario](r, a.name))
        r.mark('done')
        time.sleep(1)


if __name__ == '__main__':
    main()
