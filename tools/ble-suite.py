#!/usr/bin/env python3
"""The Bluetooth regression scenarios, run unattended against the watch on USB and over the air.

    tools/ble-suite.py --image app.bin                     every scenario, against the running image
    tools/ble-suite.py --install new.bin --restore old.bin    install new.bin first, put old.bin back after
    tools/ble-suite.py --scenarios console,reconnect --soak-minutes 5

The watch needs a `ble` image with the radio on (`ble on`), bonded to this computer
(`wrist-ble.py --pair`), and its USB console free. The suite holds that console for the whole
run and judges every scenario from its lines and the answers over the air. Each run writes
suite.log, console.log and result.json under target/ble-suite/ (or --out), and exits non-zero
if any check fails. It never runs `ble forget`.

--image is the running image's app.bin, which the update scenario sends again. With --install,
--restore must be the running image's app.bin, and the update scenario sends --image (which must
then be the installed build) or, without it, the installed image.
"""
import argparse
import asyncio
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import threading
import time
import traceback

try:
    from termios import error as TermiosError
except ImportError:
    TermiosError = OSError

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))

import serial  # noqa: E402

import wrist_port  # noqa: E402


def load(tool):
    spec = importlib.util.spec_from_file_location(tool.replace('-', '_'), ROOT / 'tools' / f'{tool}.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


wrist_ble = load('wrist-ble')
wrist_update = load('wrist-update')

SCENARIOS = ['console', 'reconnect', 'lateness', 'soak', 'update']
FORBIDDEN = re.compile(r'^\s*ble\s+forget\b')
OTHER_TOOLS = re.compile(r'(wrist-(ble|cmd|update|time|frame|crash)|ble-suite|hr-sim(-bumble)?|gate)\.py|picocom|minicom|screen /dev/')
LATENESS = re.compile(r'executor late by at most ([0-9.]+) ms at \d+ ms, over 5 ms (\d+) times')
BUILD_CHAR = rb'[0-9A-Za-z._-]'
STATUS = r'\[ble\] .*, timing '
HANG_UP = re.compile(r'\[ble\] disconnected, reason (\w+)')


def stamp(t=None):
    t = time.time() if t is None else t
    return time.strftime('%H:%M:%S', time.localtime(t)) + f'.{int(t * 1000) % 1000:03d}'


class Tee:
    """stdout, and the run's log with a time on every line."""

    def __init__(self, out, path):
        self.out, self.log, self.fresh = out, open(path, 'a'), True

    def write(self, text):
        self.out.write(text)
        for part in text.splitlines(keepends=True):
            if self.fresh:
                self.log.write(stamp() + ' ')
            self.log.write(part)
            self.fresh = part.endswith('\n')
        self.log.flush()

    def flush(self):
        self.out.flush()


# -- hosts --

class Host:
    name = platform.system()

    def ps(self):
        out = subprocess.run(['ps', '-eo', 'pid=,args='], capture_output=True, text=True).stdout
        return [line.strip() for line in out.splitlines()]

    def other_tools(self):
        out = subprocess.run(['ps', '-eo', 'pid=,ppid='], capture_output=True, text=True).stdout
        parent = dict(tuple(map(int, line.split())) for line in out.splitlines())
        mine, pid = set(), os.getpid()
        while pid and pid not in mine:
            mine.add(pid)
            pid = parent.get(pid, 0)
        return [p for p in self.ps() if OTHER_TOOLS.search(p) and int(p.split()[0]) not in mine]

    def holders(self, port):
        twin = re.sub(r'/(cu|tty)\.', lambda m: '/tty.' if m[1] == 'cu' else '/cu.', port, count=1)
        nodes = [port] + ([twin] if twin != port and os.path.exists(twin) else [])
        out = subprocess.run(['lsof', '-t'] + nodes, capture_output=True, text=True).stdout
        return [p for p in self.ps() if p.split()[0] in out.split()]

    def capture(self, path):
        return None


class Linux(Host):
    def holders(self, port):
        target = os.path.realpath(port)
        found, unseen = [], []
        for pid in filter(str.isdigit, os.listdir('/proc')):
            try:
                fds = os.listdir(f'/proc/{pid}/fd')
            except PermissionError:
                unseen.append(pid)
                continue
            except OSError:
                continue
            for fd in fds:
                try:
                    if os.readlink(f'/proc/{pid}/fd/{fd}') == target:
                        found.append(pid)
                        break
                except OSError:
                    pass
        if unseen:
            asked = self.fuser(port)
            if asked is None:
                print(f'  pre-flight: can\'t see {len(unseen)} processes of other users (no `sudo -n fuser`); '
                      f'if one has {port} open, stop it', flush=True)
            found += asked or []
        return [p for p in self.ps() if p.split()[0] in found]

    def fuser(self, port):
        """The pids holding `port`, by `sudo -n fuser`, or None when that can't be asked."""
        try:
            p = subprocess.run(['sudo', '-n', 'fuser', port], capture_output=True, text=True, timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            return None
        if p.stderr.startswith('sudo:'):
            return None
        return re.findall(r'\d+', p.stdout)

    def capture(self, path):
        sudo = [] if os.geteuid() == 0 else ['sudo', '-n']
        try:
            p = subprocess.Popen(sudo + ['btmon', '-w', str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        except OSError as e:
            print(f'-- btmon capture failed: {e}', flush=True)
            return None
        try:
            p.wait(timeout=1)
        except subprocess.TimeoutExpired:
            return p
        why = p.stderr.read().strip().splitlines()
        print(f'-- btmon capture failed (exit {p.returncode}{": " + why[-1] if why else ""}); '
              '--btmon needs btmon and `sudo -n btmon`', flush=True)
        return None


def host():
    return Linux() if sys.platform.startswith('linux') else Host()


# -- the watch's USB console, held open for the whole run --

class Usb:
    def __init__(self, explicit, path):
        self.explicit = explicit
        self.log = open(path, 'a')
        self.lines = []
        self.s = None
        self.lock = threading.Lock()
        self.stopping = threading.Event()
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _note(self, text, t=None):
        t = time.time() if t is None else t
        self.lines.append((t, text))
        self.log.write(f'{stamp(t)} {text}\n')
        self.log.flush()

    def _read(self):
        buf = b''
        while not self.stopping.is_set():
            if self.s is None:
                name = wrist_port.find(self.explicit)
                try:
                    self.s = wrist_port.settle(serial.Serial(name, 115200, timeout=0.05)) if name else None
                except (serial.SerialException, OSError, TermiosError):
                    self.s = None
                if self.s is None:
                    time.sleep(0.5)
                    continue
                self._note(f'## console open {name}')
                buf = b''
            try:
                chunk = self.s.read(4096)
            except (serial.SerialException, OSError, TermiosError) as e:
                self._note(f'## console closed: {e}')
                self.s.close()
                self.s = None
                continue
            buf += chunk
            while b'\n' in buf:
                line, buf = buf.split(b'\n', 1)
                self._note(line.decode('utf-8', 'replace').rstrip('\r'))

    def close(self):
        self.stopping.set()
        self.thread.join(2)
        if self.s:
            self.s.close()
            self.s = None

    def mark(self):
        return len(self.lines)

    def since(self, mark):
        return self.lines[mark:]

    def send(self, text):
        if FORBIDDEN.match(text):
            raise RuntimeError('the suite never runs `ble forget`')
        end = time.time() + 15
        while self.s is None and time.time() < end:
            time.sleep(0.1)
        if self.s is None:
            raise RuntimeError('the watch\'s USB console is gone')
        with self.lock:
            self._note(f'>> {text}')
            wrist_port.write_command(self.s, text)

    async def wait(self, pattern, mark, timeout):
        """(time, line) of the first line after `mark` matching `pattern`, or (None, None)."""
        pattern = re.compile(pattern)
        end = time.time() + timeout
        while True:
            for t, line in self.lines[mark:]:
                if pattern.search(line):
                    return t, line
            if time.time() >= end:
                return None, None
            await asyncio.sleep(0.05)

    async def ask(self, text, pattern, timeout=3):
        mark = self.mark()
        self.send(text)
        return (await self.wait(pattern, mark, timeout))[1]


# -- the phone --

class Link:
    def __init__(self, client, con):
        self.client, self.con = client, con

    async def command(self, text, pattern, timeout=3):
        if FORBIDDEN.match(text):
            raise RuntimeError('the suite never runs `ble forget`')
        start = len(self.con.lines)
        await wrist_ble.send(self.client, text)
        pattern = re.compile(pattern)
        end = time.time() + timeout
        while time.time() < end:
            for line in self.con.lines[start:]:
                if pattern.search(line):
                    return line
            await asyncio.sleep(0.05)
        return None

    async def close(self):
        try:
            await self.client.disconnect()
        except Exception as e:
            print(f'-- hang-up: {e!r}', flush=True)


class BleakPhone:
    """This computer's own adapter as a bonded phone: BlueZ on Linux, CoreBluetooth on macOS."""

    def __init__(self, name):
        self.name = name
        self.device = None

    async def connect(self):
        if self.device is None:
            self.device = await wrist_ble.find(self.name)
            if self.device is None:
                return None
        con = wrist_ble.Console(echo=lambda text: None if text.startswith('[hb]') else print(f'   air: {text}', flush=True))
        client = wrist_ble.BleakClient(self.device, timeout=20)
        try:
            await client.connect()
            if await wrist_ble.attach(client, con):
                return Link(client, con)
        except Exception as e:
            print(f'-- connect: {e!r}', flush=True)
        self.device = None
        try:
            await client.disconnect()
        except Exception:
            pass
        return None

    async def update(self, image):
        return await wrist_ble.run(self.name, update=str(image)) == 0


# -- results --

class Scenario:
    def __init__(self, name, criteria):
        self.name, self.criteria = name, criteria
        self.checks, self.data = [], {}
        self.started = time.time()
        print(f'\n== {name}: {criteria}', flush=True)

    def check(self, ok, what):
        self.checks.append({'passed': bool(ok), 'what': what})
        print(f'  [{"PASS" if ok else "FAIL"}] {what}', flush=True)
        return ok

    @property
    def passed(self):
        return bool(self.checks) and all(c['passed'] for c in self.checks)

    def result(self):
        return {'name': self.name, 'criteria': self.criteria, 'passed': self.passed,
                'seconds': round(time.time() - self.started, 1), 'measurements': self.data, 'checks': self.checks}


def counters(status):
    """The `ble` line's counters of things that should never happen, by name; a field's later counters
    take its first one's prefix (`irq stuck after write`)."""
    found = {}
    for field in status.split(', '):
        prefix = None
        for name, value in re.findall(r'([a-z][a-z ]*[a-z]) (\d+)\b', field):
            if prefix is None:
                prefix = name.rpartition(' ')[0]
            elif prefix and ' ' not in name:
                name = f'{prefix} {name}'
            if not re.search(r'events|bonded|interval|latency|address', name):
                found[name] = int(value)
    return found


def failed_to_establish(path):
    """Disconnection Complete events with reason 0x3E in a btmon capture: connections that failed to establish."""
    data = Path(path).read_bytes()
    if data[:8] != b'btsnoop\0' or int.from_bytes(data[12:16], 'big') != 2001:
        return None
    count, at = 0, 16
    while at + 24 <= len(data):
        length = int.from_bytes(data[at + 4:at + 8], 'big')
        opcode = int.from_bytes(data[at + 8:at + 12], 'big') & 0xFFFF
        event = data[at + 24:at + 24 + length]
        at += 24 + length
        if opcode == 3 and len(event) >= 6 and event[0] == 0x05 and event[2] == 0 and event[5] == 0x3E:
            count += 1
    return count


def spread(values):
    return {'min': min(values), 'median': round(statistics.median(values), 3), 'max': max(values)} if values else None


async def hang_up(usb, mark, timeout=8):
    _, line = await usb.wait(HANG_UP.pattern, mark, timeout)
    return HANG_UP.search(line)[1] if line else None


# -- scenarios --

async def console(s, usb, phone, build, a):
    for i in range(1, a.sessions + 1):
        mark = usb.mark()
        link = await phone.connect()
        if not s.check(link, f'session {i}: attached over the air'):
            continue
        s.check(await link.command('ping', r'\bpong\b'), f'session {i}: ping answered')
        s.check(await link.command('version', rf'build {re.escape(build)}\b'), f'session {i}: version is build {build}')
        s.check(await link.command('ble', r'\[ble\] connected, encrypted'), f'session {i}: ble reports the link encrypted')
        await link.close()
        reason = await hang_up(usb, mark)
        s.check(reason == '13', f'session {i}: the watch saw a clean hang-up (reason {reason})')


async def reconnect(s, usb, phone, build, a):
    times, failures = [], []
    for i in range(1, a.reconnects + 1):
        mark = usb.mark()
        link = await phone.connect()
        pong = bool(link) and bool(await link.command('ping', r'\bpong\b'))
        if link:
            await link.close()
        reason = await hang_up(usb, mark)
        lines = usb.since(mark)
        connected = next((t for t, l in lines if re.search(r'\[ble\] connected\s*$', l)), None)
        encrypted = next((t for t, l in lines if '[ble] link encrypted' in l), None)
        bad = [l for _, l in lines if re.search(r'pairing failed|encryption off|lost its bond', l)]
        took = round(encrypted - connected, 3) if connected and encrypted else None
        ok = pong and took is not None and took <= a.encrypt_limit and reason == '13' and not bad
        print(f'  {i}: {"ok" if ok else "FAILED"}, pong {pong}, connected to encrypted {took} s, reason {reason}'
              + (f', {bad}' if bad else ''), flush=True)
        if took is not None:
            times.append(took)
        if not ok:
            failures.append(i)
    s.data['connected_to_encrypted_s'] = spread(times)
    s.data['failed'] = failures
    s.check(a.reconnects > 0 and not failures, f'{a.reconnects} reconnects, each: ping answered, encrypted within {a.encrypt_limit} s, '
            f'reason 13, no pairing failure (failed: {failures or "none"}; connected to encrypted {spread(times)})')


async def page(usb):
    line = await usb.ask('page', LATENESS.pattern, 5)
    m = LATENESS.search(line or '')
    return (float(m[1]), int(m[2])) if m else None


async def traffic(link, seconds):
    end = time.time() + seconds
    while True:
        if not await link.command('ble', r'\[ble\] connected') or not await link.command('ping', r'\bpong\b'):
            return False
        if time.time() >= end:
            return True
        await asyncio.sleep(1)


async def lateness(s, usb, phone, build, a):
    await page(usb)
    armed = await usb.ask(f'page lag {a.lag_seconds}', r'probe armed', 3)
    if not s.check(armed, f'`page lag {a.lag_seconds}` armed the probe'):
        return
    link = await phone.connect()
    if not s.check(link, 'attached over the air for traffic'):
        return
    readings = []
    for _ in range(2):
        s.check(await traffic(link, a.lag_seconds / 2 - 2), 'console traffic over the air answered throughout')
        readings.append(await page(usb))
    await link.close()
    s.check(all(readings), f'`page` reported the lateness twice ({readings})')
    got = [r for r in readings if r]
    worst = max((r[0] for r in got), default=None)
    over = sum(r[1] for r in got)
    s.data.update(worst_ms=worst, over_5ms=over)
    s.check(worst is not None and worst <= 5.0 and over == 0, f'executor late by at most {worst} ms, over 5 ms {over} times')


async def soak(s, usb, phone, build, a):
    before = await usb.ask('ble', STATUS, 3) or ''
    up_before = await usb.ask('uptime', r'\[usb\] uptime (\d+)', 3)
    first = usb.mark()
    await page(usb)
    end = time.time() + a.soak_minutes * 60
    cycles, failures, readings = 0, [], []

    async def pager():
        armed = 0
        while time.time() < end:
            if time.time() - armed > 3000:
                usb.send('page lag 3600')
                armed = time.time()
            await asyncio.sleep(max(0, min(60, end - time.time())))
            if time.time() < end:
                readings.append(await page(usb))

    async def phone_cycles():
        nonlocal cycles
        while time.time() < end - a.hold:
            cycles += 1
            mark = usb.mark()
            link = await phone.connect()
            answered = bool(link) and await traffic(link, a.hold)
            if link:
                await link.close()
            reason = await hang_up(usb, mark)
            ok = answered and reason == '13'
            print(f'  cycle {cycles}: {"ok" if ok else "FAILED"}, answered {answered}, reason {reason}', flush=True)
            if not ok:
                failures.append(cycles)
            await asyncio.sleep(a.gap)

    await asyncio.gather(pager(), phone_cycles())
    readings.append(await page(usb))
    after = await usb.ask('ble', STATUS, 3) or ''
    up_after = await usb.ask('uptime', r'\[usb\] uptime (\d+)', 3)
    grew = {k: (v, counters(after).get(k)) for k, v in counters(before).items() if counters(after).get(k) != v}
    got = [r for r in readings if r]
    worst = max((r[0] for r in got), default=None)
    over = sum(r[1] for r in got)
    closed = [l for _, l in usb.since(first) if l.startswith('## console closed')]
    up = [int(re.search(r'uptime (\d+)', u)[1]) if u else None for u in (up_before, up_after)]
    s.data.update(cycles=cycles, failed=failures, worst_ms=worst, over_5ms=over, page_readings=len(got),
                  counters_before=counters(before), counters_after=counters(after), uptime_s=up)
    s.check(cycles and not failures, f'{cycles} phone cycles: attached, console answered for {a.hold} s, reason 13 '
            f'(failed: {failures or "none"})')
    s.check(before and after and not grew, f'the `ble` counters did not move ({grew or "none moved"})')
    s.check(got and worst <= 5.0 and over == 0, f'executor late by at most {worst} ms over {len(got)} `page` readings, '
            f'over 5 ms {over} times')
    s.check(None not in up and up[1] > up[0] and not closed, f'no reset: uptime {up[0]} to {up[1]} s, console never closed')


async def update(s, usb, phone, build, a):
    t0 = time.time()
    mark = usb.mark()
    kept = await phone.update(a.update_image)
    took = round(time.time() - t0, 1)
    s.data['seconds_to_state_boot'] = took
    s.check(kept, f'the update over the air reached `state boot` ({took} s)')
    s.check(not [l for _, l in usb.since(mark) if 'revert' in l], 'the loader never reverted')
    s.check(await usb.ask('version', rf'build {re.escape(build)}$', 10), f'the watch runs build {build} after it')
    s.check(await usb.ask('ble', r'\[ble\] (advertising as \S+|connected), ', 5), 'the radio is back on after the reset')


# -- pre-flight and install --

async def install(usb, a, image, out):
    """(whether wrist-update.py installed `image` over USB, the console reopened after it)."""
    usb.close()
    print(f'-- installing {image} over USB', flush=True)
    p = subprocess.run([sys.executable, str(ROOT / 'tools/wrist-update.py'), str(image)]
                       + (['--port', a.port] if a.port else []), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    print(p.stdout, end='', flush=True)
    usb = Usb(a.port, out / 'console.log')
    await asyncio.sleep(2)
    return p.returncode == 0, usb


def build_of(text):
    m = re.search(r'standalone-boot \S+ build (\S+)', text or '')
    return m[1] if m else None


async def snapshot(usb):
    version = await usb.ask('version', r'standalone-boot \S+ build', 5)
    boot = await usb.ask('boot', r'\[boot\] state', 5)
    ble = await usb.ask('ble', STATUS + '|unknown command', 5)
    bonds = await usb.ask('ble bonds', r'\[ble\] bonded devices|\[ble\] off|unknown command', 5)
    return {'version': version, 'build': build_of(version), 'boot': boot, 'ble': ble, 'bonds': bonds}


def preflight_host(h, explicit):
    problems = [f'running: {p}' for p in h.other_tools()]
    port = wrist_port.find(explicit)
    if port is None:
        problems.append('no watch console found on USB')
    else:
        problems += [f'holds {port}: {p}' for p in h.holders(port)]
    return port, problems


def image_problems(path, build, what):
    if path is None:
        return []
    data = Path(path).read_bytes()
    why = wrist_update.mismatch(data, False)
    if why:
        return [f'{what} {path}: {why}']
    if build == 'unknown':
        return [f'{what} {path}: the running build is `unknown` (built outside git), so no image can be matched to it']
    if build and not re.search(rb'(?<!%s)%s(?!%s)' % (BUILD_CHAR, re.escape(build.encode()), BUILD_CHAR), data):
        return [f'{what} {path} is not build {build}']
    return []


def ready(snap):
    problems = []
    if not snap['build']:
        problems.append('`version` did not answer')
    if 'state boot' not in (snap['boot'] or ''):
        problems.append(f'the loader is not settled: {snap["boot"]}')
    if not snap['ble'] or 'unknown command' in snap['ble']:
        problems.append('this image has no Bluetooth (not a `ble` build)')
    elif not re.search(r'\[ble\] (advertising|connected|up)', snap['ble']):
        problems.append(f'the radio is not on (`ble on` starts it): {snap["ble"]}')
    return problems


async def run(a, out, result):
    h = host()
    result['host'] = {'system': h.name, 'python': platform.python_version()}
    port, problems = preflight_host(h, a.port)
    result['port'] = port
    for p in problems:
        print(f'  pre-flight: {p}', flush=True)
    if problems:
        result['preflight'] = problems
        return False
    capture = None
    if a.btmon:
        capture = h.capture(out / 'btmon.btsnoop')
        if capture is None:
            result['capture'] = 'failed'
            result['preflight'] = ['--btmon: no capture']
            return False
    usb = Usb(a.port, out / 'console.log')
    installed = False
    try:
        before = await snapshot(usb)
        result['before'] = before
        print(f'-- running {before["version"]}; {before["boot"]}; {before["bonds"]}', flush=True)
        problems = ready(before) if not a.install else ([] if before['build'] else ['`version` did not answer'])
        problems += image_problems(a.restore, before['build'], '--restore')
        if a.install:
            problems += image_problems(a.install, None, '--install') + image_problems(a.image, None, '--image')
        elif 'update' in a.scenarios:
            problems += ['the update scenario needs --image (the running app.bin)'] if not a.image else \
                image_problems(a.image, before['build'], '--image')
        if problems:
            result['preflight'] = problems
            for p in problems:
                print(f'  pre-flight: {p}', flush=True)
            return False
        build = before['build']
        a.update_image = a.image
        if a.install:
            installed = True
            ok, usb = await install(usb, a, a.install, out)
            if not ok:
                result['preflight'] = ['the install failed']
                return False
            now = await snapshot(usb)
            result['installed'] = now
            problems = ready(now) + image_problems(a.install, now['build'], '--install') \
                + image_problems(a.image, now['build'], '--image')
            if problems:
                result['preflight'] = problems
                for p in problems:
                    print(f'  pre-flight: {p}', flush=True)
                return False
            build, a.update_image = now['build'], a.image or a.install
        result['build_under_test'] = build
        phone = BleakPhone(a.name)
        for name in a.scenarios:
            s = Scenario(name, CRITERIA[name].format(a=a))
            try:
                await globals()[name](s, usb, phone, build, a)
            except Exception as e:
                s.check(False, f'stopped by {e!r}')
            result['scenarios'].append(s.result())
        return all(s['passed'] for s in result['scenarios'])
    finally:
        if capture:
            died = capture.poll()
            capture.terminate()
            try:
                capture.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            path = out / 'btmon.btsnoop'
            count = failed_to_establish(path) if path.is_file() and path.stat().st_size else None
            result['failed_to_establish'] = count
            result['capture'] = 'failed' if died is not None or count is None else 'ok'
            if result['capture'] == 'failed':
                print(f'-- btmon capture failed{f" (btmon exited {died} during the run)" if died is not None else ""}; '
                      'no count of connections that failed to establish', flush=True)
        if installed:
            ok, usb = await install(usb, a, a.restore, out)
            after = await snapshot(usb)
            result['restored'] = ok and after['build'] == result.get('before', {}).get('build')
            print(f'-- restored: {result["restored"]} ({after["version"]})', flush=True)
        result['after'] = await snapshot(usb)
        usb.close()


CRITERIA = {
    'console': '{a.sessions} bonded sessions over the air, each: `ping` gives pong, `version` the build under test, '
               '`ble` "connected, encrypted"; the watch logs reason 13 at each hang-up',
    'reconnect': '{a.reconnects} bonded reconnects, each: ping answered, `link encrypted` within {a.encrypt_limit} s '
                 'of `connected` on the watch, reason 13, no pairing failure or lost bond',
    'lateness': '`page lag {a.lag_seconds}` with console traffic over the air: executor late by at most 5.0 ms, '
                'over 5 ms 0 times',
    'soak': '{a.soak_minutes} min of a phone connecting for {a.hold} s of console traffic and leaving for {a.gap} s: '
            'every cycle answered with reason 13, `ble` counters unchanged, executor at most 5.0 ms late, no reset',
    'update': 'an application update over the air reaches `state boot`, no revert, the same build after it, '
              'radio back on',
}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--scenarios', default=','.join(SCENARIOS), help=f'comma-separated, from {",".join(SCENARIOS)}')
    ap.add_argument('--image', type=Path, help='the running image\'s app.bin, for the update scenario')
    ap.add_argument('--install', type=Path, help='an app.bin to install over USB before the scenarios')
    ap.add_argument('--restore', type=Path, help='with --install: the running image\'s app.bin, put back after')
    ap.add_argument('--port')
    ap.add_argument('--name', default='UNA', help='the advertised name')
    ap.add_argument('--sessions', type=int, default=3)
    ap.add_argument('--reconnects', type=int, default=30)
    ap.add_argument('--encrypt-limit', type=float, default=2.0, help='seconds from connected to encrypted')
    ap.add_argument('--lag-seconds', type=int, default=60)
    ap.add_argument('--soak-minutes', type=float, default=30)
    ap.add_argument('--hold', type=float, default=30, help='seconds each soak connection stays')
    ap.add_argument('--gap', type=float, default=10, help='seconds between soak connections')
    ap.add_argument('--btmon', action='store_true', help='capture with btmon (Linux)')
    ap.add_argument('--out', type=Path)
    a = ap.parse_args()
    a.scenarios = [s.strip() for s in a.scenarios.split(',') if s.strip()]
    unknown = set(a.scenarios) - set(SCENARIOS)
    if unknown:
        ap.error(f'unknown scenarios: {", ".join(sorted(unknown))}')
    if not a.scenarios:
        ap.error('no scenarios')
    if a.install and not a.restore:
        ap.error('--install needs --restore, the running image to put back')
    if a.btmon and not sys.platform.startswith('linux'):
        ap.error('--btmon needs Linux')

    out = a.out or ROOT / 'target/ble-suite' / time.strftime('%Y%m%d-%H%M%S')
    out.mkdir(parents=True, exist_ok=False)
    sys.stdout = Tee(sys.stdout, out / 'suite.log')
    result = {'started': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'argv': sys.argv[1:], 'scenarios': []}
    try:
        ok = asyncio.run(run(a, out, result))
    except KeyboardInterrupt:
        ok = False
        result['stopped'] = 'interrupted'
    except Exception as e:
        ok = False
        result['stopped'] = repr(e)
        traceback.print_exc(file=sys.stdout)
    result['finished'] = time.strftime('%Y-%m-%dT%H:%M:%S%z')
    result['passed'] = bool(ok) and result.get('restored', True) is not False and result.get('capture') != 'failed'
    (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(f'\n{"PASSED" if result["passed"] else "FAILED"}; see {out}', flush=True)
    for s in result['scenarios']:
        print(f'  {"pass" if s["passed"] else "FAIL"}  {s["name"]} ({s["seconds"]} s) {s["measurements"]}', flush=True)
    if result.get('capture') == 'failed':
        print('  btmon: the capture failed', flush=True)
    elif result.get('failed_to_establish') is not None:
        print(f'  btmon: {result["failed_to_establish"]} connections failed to establish (0x3E)', flush=True)
    sys.exit(0 if result['passed'] else 1)


if __name__ == '__main__':
    main()
