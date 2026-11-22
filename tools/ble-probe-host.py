#!/usr/bin/env python3
"""The host's side of the probe build's chip probes (`--features ble-probe`).

    ble-probe-host.py matrix [--out FILE] [--from N]
        Hold a link to the watch from this computer (BlueZ accepts each interval the watch asks
        for) while `ble probe matrix` runs over USB; reconnect and resume if the link goes. A
        whole run's cells go to FILE, the fixture `crates/wrist-bluenrg2/tests/fixtures/bluenrg2-probe.txt`
        by default; a run that stops short, to FILE.partial.

    ble-probe-host.py ll [--control]
        Run `ble probe ll` (`ble probe ll control`) over USB, then find `UNA-LL` and connect to it: does the chip's own host
        answer this computer's ATT requests, or do they reach the watch as ACL data? The watch's
        packets are printed as they come; BlueZ's entry for the probe's address is removed after.

    ble-probe-host.py bonds --watch D9:5B:7A:EB:29:82 [--transport hci-socket:0]
            [--scenario NAME | --steps STEPS] [--log FILE] [--keep]
        Pair throwaway static random centrals (`C0:B0:4D:00:00:<n>`, n in hex), answering the
        watch's comparison over USB, and list the watch's `ble bonds` after each link's connect,
        pairing or encryption, hang-up and 2 s later, naming each bond that went (LOSS). The steps,
        `;` between them:
            clear                    `ble forget`: the whole bond database
            forget <n>|test          `ble forget` one throwaway bond, or every one listed
            bond <n> [options]       pair central n, then hang up
            reconnect <n>|last       connect again and encrypt with the keys kept from its bond
            fill <n> [options]       bond n, n+1, ... until `losses=` loss events (1) or `max=` (16)
            restart                  `ble restart`: the chip reset through its pin
            list | wait <s>
        Options: dist=default|none|noid|sign (the keys the central offers and asks for), subscribe
        (to every notification and indication after encrypting), hold=<s> (the link kept, listed
        every 5 s), hangup=central|watch|restart. `--scenario` names a list (SCENARIOS). The
        throwaway bonds are forgotten at the end unless `--keep`. Runs on Bumble, which takes the
        adapter from BlueZ: as root (the step does `btmgmt power off` first and `power on` after).

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
# Longer than any cell takes, or the wait before the first (`UPDATE_WITHIN_MS`, `HOLD_MS`, apps/standalone-boot/src/ble/probe.rs).
CELL_WITHIN_S = 40
# What the watch prints for a matrix it won't run.
REFUSED = ('probe matrix:', '[ble] off', 'held after a fault', 'unknown command', 'not queued')
BUSY = 'operation in flight'


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


def kept(text):
    """The cell's line for the fixture, or None."""
    m = CELL.search(text)
    return m.group(1) if m and not text.endswith('link=lost') else None


def destination(out, start, cells):
    """Where a run's cells go, and whether it was a whole run: only a whole run replaces `out`."""
    whole = start == 0 and len(cells) == 60
    return (out if whole else out + '.partial'), whole


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
    failures = resumes = busy = 0
    while failures < 5 and resumes < 10 and busy < 5:
        log(f'connecting to {args.name}')
        try:
            client = await hold_link(args.name)
        except Exception as e:
            log(f'connect: {e!r}')
            client = None
        if client is None:
            failures += 1
            await asyncio.sleep(5)
            continue
        # The link's own start (encryption) over before the first cell.
        await asyncio.sleep(args.settle)
        usb.send(f'ble probe matrix from {start}' if start else 'ble probe matrix')
        resume = None
        while True:
            text = await lines_until(usb, lambda t: 'probe matrix' in t or any(r in t for r in REFUSED), CELL_WITHIN_S)
            if text is None:
                log('no line from the matrix in time')
                break
            if CELL.search(text):
                if line := kept(text):
                    cells.append(line)
            elif m := WENT.search(text):
                resume = int(m.group(1))
                break
            elif 'probe matrix: done' in text:
                break
            elif BUSY in text:
                busy += 1
                resume = start
                break
            elif any(r in text for r in REFUSED):
                log(f'the watch refused: {text}')
                break
        try:
            await client.disconnect()
        except Exception:
            pass
        if resume is None:
            break
        resumes += 1
        start = resume
        await asyncio.sleep(3)
    header = [
        '# `ble probe matrix` on the watch, the Linux host the central (tools/ble-probe-host.py matrix).',
        f'# {time.strftime("%Y-%m-%d")}; one line per cell: each change in the order made, the anchor period after each.',
    ]
    out, whole = destination(args.out, args.start, cells)
    Path(out).write_text('\n'.join(header + cells) + '\n')
    log(f'{len(cells)} cells written to {out}')
    return 0 if whole else 1


async def link_layer(args):
    try:
        return await link_layer_run(args)
    finally:
        subprocess.run(['timeout', '10', 'bluetoothctl', 'remove', LL_ADDRESS], capture_output=True)


async def link_layer_run(args):
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
        # Outlasts BlueZ's own ATT timeout.
        client = BleakClient(device, timeout=45)
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
    return 0


BOND = re.compile(r'([0-9A-F]{2}(?::[0-9A-F]{2}){5})( \(random\))?')
TEST = 'C0:B0:4D:00:00:'

# The bond steps' scenarios, by name (`bonds --scenario`); `--steps` takes the same language.
SCENARIOS = {
    # From an empty database, bond until a loss, and on past it: where it overflows, and again.
    'fill': 'clear; fill 1 losses=3 max=16',
    # A middle bond forgotten, so that age and list position differ for the bonds after it.
    'middle': 'clear; bond 1; bond 2; bond 3; bond 4; forget 2; fill 5 losses=2 max=14',
    # The oldest bond used again before the overflow: dropped by bonding time, or by use?
    'reuse': 'clear; bond 1; bond 2; bond 3; bond 4; reconnect 1; fill 5 losses=2 max=14',
    # Each link held 10 s, listed as it goes: whether a loss waits for the hang-up.
    'hold': 'clear; fill 1 hold=10 max=16',
    # The watch hangs up instead of the central.
    'watch-hangs-up': 'clear; fill 1 hangup=watch max=16',
    # The chip reset under each link, so no disconnect is processed; then one ordinary link.
    'restart-linked': 'clear; fill 1 hangup=restart max=10; list; reconnect last',
    # What each central distributes, and its subscriptions: does the overflow move?
    'keys-none': 'clear; fill 1 dist=none max=16',
    'keys-noid': 'clear; fill 1 dist=noid max=16',
    'keys-sign': 'clear; fill 1 dist=sign max=16',
    'subscribe': 'clear; fill 1 subscribe max=16',
    # Bonds that held, made larger by subscribing at a reconnect, with no new bond.
    'grow': 'clear; bond 1; bond 2; bond 3; bond 4; bond 5; reconnect 1 subscribe; reconnect 2 subscribe; '
            'reconnect 3 subscribe; reconnect 4 subscribe; reconnect 5 subscribe; list',
    # Do forgotten bonds still take room? Fill, forget one by one, fill again; then after a chip
    # reset; then after clearing the whole database.
    'history': 'clear; fill 1; forget test; fill 33; forget test; fill 65; forget test; restart; fill 97; '
               'clear; fill 129',
}
DISTRIBUTIONS = ('default', 'none', 'noid', 'sign')
HANGUPS = ('central', 'watch', 'restart')
OPTIONS = {'dist', 'subscribe', 'hold', 'hangup', 'max', 'losses'}


def listed(text):
    """The addresses on a `bonded devices:` line, each as `ble forget` takes it; None for another line."""
    if 'bonded devices:' not in text:
        return None
    return [a + (r or '') for a, r in BOND.findall(text.split('bonded devices:', 1)[1])]


def test_bond(n):
    """The `n`th throwaway central's static random address, as `ble bonds` lists it."""
    return f'{TEST}{n:02X} (random)'


def parse_steps(text):
    """`--steps` as (word, argument, options) each; a ValueError names the first bad step."""
    steps = []
    for part in text.split(';'):
        words = part.split()
        if not words:
            continue
        word, rest = words[0], words[1:]
        arg = None
        if word in ('bond', 'fill', 'reconnect', 'forget', 'wait'):
            if not rest:
                raise ValueError(f'{part.strip()!r}: {word} takes an argument')
            arg = rest.pop(0)
            if word == 'wait':
                arg = float(arg)
            elif not (arg == 'last' and word == 'reconnect') and not (arg == 'test' and word == 'forget'):
                arg = int(arg, 0)
                if not 1 <= arg <= 0xFF:
                    raise ValueError(f'{part.strip()!r}: centrals are 1-255')
        elif word not in ('clear', 'restart', 'list'):
            raise ValueError(f'{part.strip()!r}: unknown step')
        options = {}
        for r in rest:
            k, eq, v = r.partition('=')
            if k not in OPTIONS:
                raise ValueError(f'{part.strip()!r}: unknown option {k}')
            options[k] = v if eq else 'yes'
        if options.get('dist', 'default') not in DISTRIBUTIONS or options.get('hangup', 'central') not in HANGUPS:
            raise ValueError(f'{part.strip()!r}: dist is one of {DISTRIBUTIONS}, hangup one of {HANGUPS}')
        steps.append((word, arg, options))
    return steps


def fresh(path):
    """`path`, or `path` numbered past every log already there: a run never overwrites one."""
    p = Path(path)
    n = 2
    while p.exists():
        p = Path(path).with_name(f'{Path(path).stem}-{n}{Path(path).suffix}')
        n += 1
    return p


def losses(expected, now):
    """The bonds expected that `now` lacks, and those it has unexpected."""
    return [b for b in expected if b not in now], [b for b in now if b not in expected]


class Tee:
    def __init__(self, *streams):
        self.streams = streams

    def write(self, text):
        for s in self.streams:
            s.write(text)

    def flush(self):
        for s in self.streams:
            s.flush()


def pairing_config(delegate):
    """The throwaway address as the identity, never the adapter's own."""
    from bumble.pairing import PairingConfig
    return PairingConfig(sc=True, mitm=True, bonding=True, delegate=delegate,
                         identity_address_type=PairingConfig.AddressType.RANDOM)


def key_distribution(dist):
    """What the central offers and asks for, by `dist=`."""
    from bumble.pairing import PairingDelegate
    k = PairingDelegate.KeyDistribution
    return {
        'default': PairingDelegate.DEFAULT_KEY_DISTRIBUTION,
        'none': k(0),
        'noid': k.DISTRIBUTE_ENCRYPTION_KEY,
        'sign': k.DISTRIBUTE_ENCRYPTION_KEY | k.DISTRIBUTE_IDENTITY_KEY | k.DISTRIBUTE_SIGNING_KEY,
    }[dist]


class BondRun:
    """`bonds`' steps against the watch: Bumble the centrals, the USB console the watch's side."""

    def __init__(self, usb, watch, source, sink):
        from bumble.keys import MemoryKeyStore
        self.usb, self.watch, self.source, self.sink = usb, watch, source, sink
        self.stores = {}
        self.new_store = MemoryKeyStore
        self.expected = []
        self.loss_events = 0
        self.last = None

    async def drain(self):
        while await lines_until(self.usb, lambda _t: True, 0.3) is not None:
            pass

    async def listing(self, stage):
        """`ble bonds` now, checked against what should be there."""
        await self.drain()
        self.usb.send('ble bonds')
        text = await lines_until(self.usb, lambda t: listed(t) is not None or 'no bonded' in t, 10) or ''
        now = listed(text) or []
        gone, extra = losses(self.expected, now)
        log(f'LIST {stage}: {len(now)} {" ".join(now)}')
        if gone:
            self.loss_events += 1
            log(f'LOSS {stage}: {len(gone)} gone: {" ".join(gone)}')
            self.expected = [b for b in self.expected if b not in gone]
        if extra:
            log(f'NEW {stage}: {" ".join(extra)}')
            self.expected += extra
        return now

    def device(self, n, dist):
        from bumble import hci
        from bumble.device import Device, DeviceConfiguration
        from bumble.pairing import PairingDelegate

        offered = key_distribution(dist)

        class Yes(PairingDelegate):
            def __init__(self):
                super().__init__(io_capability=PairingDelegate.IoCapability.DISPLAY_OUTPUT_AND_YES_NO_INPUT,
                                 local_initiator_key_distribution=offered,
                                 local_responder_key_distribution=offered)

            async def compare_numbers(self, number, digits):
                return True

            async def confirm(self, auto=False):
                return True

        address = hci.Address(f'{TEST}{n:02X}', hci.Address.RANDOM_DEVICE_ADDRESS)
        config = DeviceConfiguration(name='bond-probe', address=address, irk=bytes([n]) * 16)
        device = Device.from_config_with_hci(config, self.source, self.sink)
        device.keystore = self.stores.setdefault(n, self.new_store())
        device.pairing_config_factory = lambda _c: pairing_config(Yes())
        return device

    async def link(self, n, options, new):
        """Bond central `n` (`new`), or reconnect and encrypt it, then hang up as `options` say."""
        from bumble import hci
        from bumble.core import BT_LE_TRANSPORT
        name = f'{TEST}{n:02X}'
        what = 'bond' if new else 'reconnect'
        log(f'STEP {what} {name} {" ".join(f"{k}={v}" for k, v in options.items())}')
        device = self.device(n, options.get('dist', 'default'))
        await device.power_on()
        watch = hci.Address(self.watch, hci.Address.RANDOM_DEVICE_ADDRESS)
        connection = None
        for attempt in range(2):
            try:
                connection = await asyncio.wait_for(device.connect(
                    watch, transport=BT_LE_TRANSPORT, own_address_type=hci.OwnAddressType.RANDOM, timeout=15), 20)
                break
            except Exception as e:
                log(f'{name}: connect: {e!r}')
                await asyncio.sleep(3)
        if connection is None:
            return 'no link'
        await lines_until(self.usb, lambda t: '[ble] connected' in t, 5)
        await self.listing(f'{name} linked')
        if new:
            pairing = asyncio.ensure_future(connection.pair())
            shown = await lines_until(self.usb, lambda t: 'the central shows a number' in t or 'pairing failed' in t, 20)
            if shown and 'shows a number' in shown:
                self.usb.send('ble yes')
            try:
                await asyncio.wait_for(pairing, 30)
                outcome = 'paired'
            except Exception as e:
                outcome = f'pairing failed: {e!r}'
            await lines_until(self.usb, lambda t: t.startswith('[ble] bonded') or 'pairing failed' in t, 5)
        else:
            try:
                await asyncio.wait_for(connection.encrypt(), 15)
                outcome = 'encrypted'
            except Exception as e:
                outcome = f'encryption failed: {e!r}'
            await lines_until(self.usb, lambda t: 'link encrypted' in t or 'disconnected' in t, 5)
        log(f'{name}: {outcome}')
        now = await self.listing(f'{name} {outcome.split(":")[0]}')
        if new and outcome == 'paired':
            self.last = n
            if test_bond(n) not in now:
                log(f'OWN BOND NOT LISTED: {test_bond(n)}')
            elif test_bond(n) not in self.expected:
                self.expected.append(test_bond(n))
        if 'yes' == options.get('subscribe') and outcome in ('paired', 'encrypted'):
            await self.subscribe(connection, name)
            await self.listing(f'{name} subscribed')
        hold = float(options.get('hold', 0))
        for held in range(5, int(hold) + 1, 5):
            await asyncio.sleep(5)
            await self.listing(f'{name} held {held} s')
        hangup = options.get('hangup', 'central')
        if hangup == 'central':
            try:
                await connection.disconnect()
            except Exception as e:
                log(f'{name}: disconnect: {e!r}')
            await lines_until(self.usb, lambda t: 'disconnected' in t, 10)
        elif hangup == 'watch':
            await self.drain()
            self.usb.send('ble disconnect')
            await lines_until(self.usb, lambda t: 'disconnected' in t, 10)
        else:
            await self.drain()
            self.usb.send('ble restart')
            await lines_until(self.usb, lambda t: 'advertising as' in t, 20)
        await self.listing(f'{name} after the {hangup} hang-up')
        await asyncio.sleep(2)
        await self.listing(f'{name} 2 s later')
        await device.power_off()
        return outcome

    async def subscribe(self, connection, name):
        from bumble.device import Peer
        from bumble.gatt import Characteristic
        wanted = Characteristic.Properties.NOTIFY | Characteristic.Properties.INDICATE
        count = 0
        try:
            peer = Peer(connection)
            await asyncio.wait_for(peer.discover_all(), 20)
            for service in peer.services:
                for c in service.characteristics:
                    if c.properties & wanted:
                        await asyncio.wait_for(peer.subscribe(c, lambda _v: None), 10)
                        count += 1
            log(f'{name}: subscribed to {count} characteristics')
        except Exception as e:
            log(f'{name}: subscribing, after {count}: {e!r}')

    async def forget(self, bond):
        await self.drain()
        self.usb.send(f'ble forget {bond}')
        await lines_until(self.usb, lambda t: 'forg' in t, 5)
        self.expected = [b for b in self.expected if b != bond]

    async def step(self, word, arg, options):
        if word == 'clear':
            log('STEP clear')
            await self.drain()
            self.usb.send('ble forget')
            await lines_until(self.usb, lambda t: 'cleared' in t or 'forget' in t, 5)
            self.expected = []
            await self.listing('cleared')
        elif word == 'forget':
            log(f'STEP forget {arg}')
            now = await self.listing('before the forget')
            for b in [b for b in now if b.startswith(TEST)] if arg == 'test' else [test_bond(arg)]:
                await self.forget(b)
            await self.listing('forgotten')
        elif word == 'restart':
            log('STEP restart')
            await self.drain()
            self.usb.send('ble restart')
            await lines_until(self.usb, lambda t: 'advertising as' in t, 20)
            await self.listing('restarted')
        elif word == 'list':
            await self.listing('list')
        elif word == 'wait':
            await asyncio.sleep(arg)
        elif word == 'bond':
            await self.link(arg, options, True)
        elif word == 'reconnect':
            if arg == 'last' and self.last is None:
                return log('STEP reconnect last: nothing bonded')
            await self.link(self.last if arg == 'last' else arg, options, False)
        elif word == 'fill':
            most, stop = int(options.get('max', 16)), self.loss_events + int(options.get('losses', 1))
            failures, n = 0, arg
            log(f'STEP fill from {TEST}{arg:02X}: until {stop - self.loss_events} losses or {most} bonds')
            while n < arg + most and n <= 0xFF and self.loss_events < stop and failures < 3:
                outcome = await self.link(n, options, True)
                failures = 0 if outcome == 'paired' else failures + 1
                n += 1
            log(f'FILL END: {n - arg} centrals, {len(self.expected)} bonds expected')


async def bonds(args):
    from bumble.transport import open_transport
    steps = parse_steps(SCENARIOS[args.scenario] if args.scenario else args.steps)
    if args.log:
        out = fresh(args.log)
        out.parent.mkdir(parents=True, exist_ok=True)
        sys.stdout = Tee(sys.__stdout__, open(out, 'w'))
        log(f'log {out}')
    log(f'scenario {args.scenario or "-"}: {args.scenario and SCENARIOS[args.scenario] or args.steps}')
    usb = Lines(args.port)
    subprocess.run(['btmgmt', '--index', '0', 'power', 'off'], capture_output=True)
    try:
        async with await open_transport(args.transport) as (source, sink):
            run = BondRun(usb, args.watch, source, sink)
            await run.listing('before')
            for word, arg, options in steps:
                await run.step(word, arg, options)
            if not args.keep:
                for b in [b for b in await run.listing('at the end') if b.startswith(TEST)]:
                    await run.forget(b)
            final = await run.listing('final')
            log(f'END: {run.loss_events} loss events; {len(final)} bonds left')
    finally:
        subprocess.run(['btmgmt', '--index', '0', 'power', 'on'], capture_output=True)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--port')
    ap.add_argument('--name', default=wrist_port.NAME)
    sub = ap.add_subparsers(dest='what', required=True)
    m = sub.add_parser('matrix')
    m.add_argument('--out', default=str(FIXTURE))
    m.add_argument('--from', dest='start', type=int, default=0, choices=range(60), metavar='0-59')
    m.add_argument('--settle', type=float, default=8, help='seconds between connecting and the first cell')
    ll = sub.add_parser('ll')
    ll.add_argument('--control', action='store_true', help="the same boot with the chip's host kept (0x2C written 0)")
    b = sub.add_parser('bonds')
    b.add_argument('--watch', required=True, help="the watch's address, as `ble` prints it")
    b.add_argument('--transport', default='hci-socket:0')
    which = b.add_mutually_exclusive_group()
    which.add_argument('--scenario', choices=SCENARIOS, help='a named list of steps (SCENARIOS)')
    which.add_argument('--steps', default='fill 1', help='steps, `;` between them (the module docstring)')
    b.add_argument('--log', help='also write the run here; an existing log is never overwritten')
    b.add_argument('--keep', action='store_true', help='leave the throwaway bonds on the watch at the end')
    args = ap.parse_args()
    run = {'matrix': matrix, 'll': link_layer, 'bonds': bonds}[args.what]
    return asyncio.run(run(args))


if __name__ == '__main__':
    sys.exit(main())
