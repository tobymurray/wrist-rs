#!/usr/bin/env python3
"""Walk release/GATE.md against one candidate: the console and drive work done here, checks
read from the watch's output, and a prompt for every press, hold and unplug.

    tools/gate.py CANDIDATE_DIR [--ota stock.ota] [--steps 4-12] [--round-trip] [--port P]

CANDIDATE_DIR holds wrist-rs-VERSION.zip and SHA256SUMS, as CI's artifact unpacks. Run it
from a checkout at the candidate's commit. Every step's output goes under
target/gate-BUILD_ID-TIME/ (or --out), with summary.md at the end. Section D runs when the
candidate's round-trip fingerprint is not gate.json's, or with --round-trip.
The battery stretch, section B (steps 9 to 11), runs last, after sections C and D.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import threading
import time
import zipfile
import zlib

try:
    from termios import error as TermiosError
except ImportError:
    TermiosError = OSError

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))

import firmware_build  # noqa: E402
import wrist_port  # noqa: E402


def plan_pages(root=ROOT):
    """(first page, last page) bank 1 may hold, from wrist-flash's plan."""
    plan = (root / 'crates/wrist-flash/src/plan.rs').read_text()
    first = int(re.search(r'FIRST_KERNEL_PAGE: \w+ = (\d+)', plan)[1])
    pages = int(re.search(r'pub const MAX_PAGES: \w+ = (\d+)', plan)[1])
    return first, first + pages - 1


def notes_stock(root=ROOT):
    """The stock version the release notes name, from their `Stock firmware` row."""
    m = re.search(r'^\| \*\*Stock firmware\*\* \| \*\*([0-9.]+)\*\*', (root / 'release/NOTES.md').read_text(), re.M)
    return m[1] if m else None


def mmio_bits(root=ROOT):
    """The bit constants in the image's mmio.rs, by name: `1 << 27`, `0b11 << 4` and the like."""
    text = (root / 'apps/standalone-boot/src/mmio.rs').read_text()
    bits = {}
    for name, value in re.findall(r'pub const (\w+): u32 = ([0-9a-fA-FxXbB_ <|]+);', text):
        a, _, b = value.replace('_', '').partition('<<')
        bits[name] = int(a.strip(), 0) << int(b or '0')
    return bits


def register(text, name):
    m = re.search(rf'^\s*{name}\s*= (0x[0-9A-Fa-f]+)', text, re.M)
    return int(m[1], 16) if m else None


def park_checks(report, bits=None):
    """GATE.md step 6's clocks and SRAM bits, from `report`."""
    bits = mmio_bits() if bits is None else bits
    want = lambda prefix: sum(v for k, v in bits.items() if k.startswith(prefix))
    has = lambda name, mask: (v := register(report, name)) is not None and v & mask == mask
    ahb2 = register(report, 'RCC_AHB2ENR1')
    return [(has('PWR_CR2', want('PWR_CR2_')), 'PWR_CR2 holds mmio.rs\'s SRAM bits'),
            (has('PWR_CR4', want('PWR_CR4_')), 'PWR_CR4 holds mmio.rs\'s SRAM bits'),
            (has('ICACHE_CR', bits['ICACHE_CR_EN']), 'ICACHE_CR_EN set'),
            (ahb2 is not None and not ahb2 & bits['RCC_AHB2ENR1_SDMMC1EN'], 'RCC_AHB2ENR1_SDMMC1EN clear')]


def check_install_log(text, crc, size, pages=None):
    """(passed, what) for each thing INSTALL.md step 4 asks of install.log."""
    first, last = plan_pages() if pages is None else pages
    found = lambda pattern: re.search(pattern, text, re.M)
    plan = found(r'^first page : (\d+) \(bank 1\)\s*\npages\s*: (\d+)')
    checks = [
        (bool(found(rf'^image\s*: image\.bin, {size} bytes')), f'image.bin, {size} bytes'),
        (bool(found(rf'^crc32\s*: {crc}\b')), f'CRC {crc}'),
        (bool(found(r'^plan code\s*: 0\b')), 'plan code 0'),
        (bool(plan) and int(plan[1]) == first and int(plan[1]) + int(plan[2]) - 1 <= last,
         f'a plan within bank 1 pages {first} to {last}'),
        (bool(found(r'^staging\s*: bank 2 pages 128-195')), 'staging in bank 2 pages 128 to 195'),
        (bool(found(r'^unlock test: 3\b')), 'unlock and re-unlock'),
        (not found(r'refus|MISMATCH|FOREIGN DATA'), 'no refusal'),
        (bool(found(r'^dry run passed')), 'dry run passed'),
    ]
    return checks


def witness_checks(text, crc):
    """Step 17: the last install's outcome record, for this CRC."""
    return [(bool(re.search(r'OUTCOME 1: erase\+program\+verify all SUCCEEDED', text)), 'outcome 1 recorded'),
            (bool(re.search(rf'image crc\s*: {crc}\b', text)), f'outcome record for {crc}'),
            (bool(re.search(r'witness page \(bank 1, 45\): ours', text)), 'witness page ours')]


def wake_rate(power):
    """Stop 2 wakes a minute over the last stretch on battery, or None."""
    stretch = re.search(r'last on battery: (?:(\d+) d )?(?:(\d+) h)? ?(?:(\d+) min)?,', power)
    wakes = re.search(r'stop2_wakes=(\d+)', power)
    if not (stretch and wakes):
        return None
    d, h, m = (int(v or 0) for v in stretch.groups())
    minutes = (d * 24 + h) * 60 + m
    return int(wakes[1]) / minutes if minutes else None


SLOTS = ('view', 'link1', 'link2')


def module_checks(text):
    """Step 7: each slot's record, as the image compared it with what it expects."""
    return [(bool(re.search(rf'^\[mod\] {slot} .*: expected \(', text, re.M)), f'{slot} record expected')
            for slot in SLOTS]


# RM0456 11.8.50, as main.rs's `report_reset_cause` names them.
RESET_FLAGS = [(31, 'LPWR'), (30, 'WWDG'), (29, 'IWDG'), (28, 'SFT'), (27, 'BOR'), (26, 'PIN'), (25, 'OBL')]


def handoff_reset_flags(report):
    """The reset flags in `RCC_CSR` as the image found it, from `report`'s handoff block, or
    None. The boot log names them too, but it is gone before USB is up."""
    handoff = report.partition('-- RCC at handoff')[2].partition('-- RCC now')[0]
    csr = register(handoff, 'RCC_CSR')
    return None if csr is None else [name for bit, name in RESET_FLAGS if csr & 1 << bit]


def charge(text):
    """The charge in percent from a `[pwr]` line, or None."""
    m = re.search(r'^\[pwr\] .*\bsoc ([\d.]+)%', text, re.M)
    return float(m[1]) if m else None


def press(button, then):
    """A real short press of `button` in the console's log, then `then`: an injected one
    says so, and does not count."""
    label = {'Up': 'SW1/Up', 'Down': 'SW3/Down', 'Back': 'SW4/Back', 'Select': 'SW2/Select'}[button]
    return rf'\[button\] short press {label}[\s\S]*?{re.escape(then)}'


def volumes():
    user = os.environ.get('USER', '')
    for base in [Path('/Volumes'), Path('/run/media') / user, Path('/media') / user, Path('/media')]:
        if base.is_dir():
            yield from (p for p in base.iterdir() if p.is_dir())


def unmounted():
    """Labelled partitions on Linux that nothing has mounted, as (device, label)."""
    if not sys.platform.startswith('linux') or not shutil.which('lsblk'):
        return []
    out = subprocess.run(['lsblk', '-rno', 'PATH,LABEL,MOUNTPOINT,TYPE'], capture_output=True, text=True).stdout
    rows = [line.split(' ') for line in out.splitlines()]
    return [(r[0], r[1].encode().decode('unicode_escape')) for r in rows
            if len(r) == 4 and r[1] and not r[2] and r[3] == 'part']


class Gate:
    def __init__(self, a):
        self.a = a
        self.results = []
        self.installs = []
        self.before = ''
        self.drive14 = self.drive17 = None
        # Whether this run's step 13 reset the watch's power, which clears the zone.
        self.power_reset = False

    # -- records --
    def start(self, out):
        self.out = out
        out.mkdir(parents=True, exist_ok=False)
        self.logf = (out / 'gate.log').open('a')

    def log(self, text):
        print(text, flush=True)
        self.logf.write(text + '\n')
        self.logf.flush()

    def save(self, name, text):
        (self.out / name).write_text(text)
        self.log(f'  (saved {name})')

    def check(self, step, ok, what):
        self.results.append((step, 'PASS' if ok else 'FAIL', what))
        self.log(f'  [{"PASS" if ok else "FAIL"}] step {step}: {what}')
        return ok

    def ask(self, step, question):
        while True:
            answer = input(f'\n>>> step {step}: {question} [y/n] ').strip().lower()
            if answer in ('y', 'n'):
                return self.check(step, answer == 'y', question + ' (seen)')

    def do(self, step, action):
        input(f'\n>>> step {step}: {action}\n    Enter when done. ')
        self.log(f'  step {step}: {action} -- done')

    def installed(self, step, what):
        self.installs.append((step, what))
        self.log(f'  ** a numbered install: step {step}, {what}')

    # -- the console --
    def wait_port(self, timeout):
        end = time.time() + timeout
        if not wrist_port.find(self.a.port):
            print(f'  waiting up to {timeout} s for the console\'s port', flush=True)
        while time.time() < end:
            port = wrist_port.find(self.a.port)
            if port:
                time.sleep(1.0)
                return port
            time.sleep(0.5)
        return None

    def cmd(self, commands, seconds=3.0, until=None, then=0.0):
        """The console's answer to `commands` over `seconds`; once `until` appears, only
        `then` seconds more."""
        import serial
        port = self.wait_port(30)
        if not port:
            return ''
        buf = b''
        # A reset or a pulled cable ends the link: keep what came before it.
        try:
            with serial.Serial(port, 115200, timeout=0.2) as s:
                if commands:
                    wrist_port.settle(s)
                else:
                    time.sleep(0.3)
                    s.reset_input_buffer()
                for c in filter(None, commands.split(';')):
                    wrist_port.write_command(s, c)
                    time.sleep(0.3)
                end = time.time() + seconds
                while time.time() < end:
                    buf += s.read(4096)
                    if until and until.encode() in buf:
                        end = min(end, time.time() + then)
                        until = None
        # A port opened as the watch restarts can fail in termios, which is not an OSError.
        except (serial.SerialException, OSError, TermiosError) as e:
            buf += f'\n[gate.py] the console closed: {e}\n'.encode()
        return '\n'.join(l for l in buf.decode('utf-8', 'replace').splitlines() if not l.startswith('[hb]'))

    def booted(self, timeout=90):
        print(f'  waiting up to {timeout} s for `boot` to report state boot', flush=True)
        end = time.time() + timeout
        while time.time() < end:
            text = self.cmd('boot;version', 2)
            if 'state boot' in text:
                return text
            time.sleep(2)
        return ''

    def tool(self, *args, kill_at=None, on_half=None, timeout=300):
        """Run tools/ARGS[0] with this Python; stop it, or call `on_half`, once half the
        image has been sent. Returns (exit code, output)."""
        image = next((Path(a) for a in args if str(a).endswith('.bin')), None)
        half = image.stat().st_size // 2 if image else None
        p = subprocess.Popen([sys.executable, str(ROOT / 'tools' / args[0]), *map(str, args[1:])],
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        lines, fired = [], False
        timer = threading.Timer(timeout, p.kill)
        timer.start()
        for line in p.stdout:
            lines.append(line.rstrip('\n'))
            print('    ' + lines[-1], flush=True)
            sent = re.match(r'\s*sent (\d+)', line)
            if half and sent and int(sent[1]) >= half and not fired:
                fired = True
                if kill_at:
                    p.kill()
                    break
                if on_half:
                    on_half()
        p.wait()
        timer.cancel()
        return p.returncode, '\n'.join(lines)

    # -- drives --
    def drive(self, step, want, what, timeout=180):
        """The mounted drive `want` picks, waited for; None asks to mount it by hand."""
        print(f'  waiting up to {timeout} s for {what} to be mounted', flush=True)
        start = time.time()
        hinted = set()
        while time.time() < start + timeout:
            for v in volumes():
                if want(v):
                    self.log(f'  drive: {v}')
                    return v
            waited = int(time.time() - start)
            for dev, label in unmounted():
                # Nothing on some desktops mounts a drive by itself.
                if waited >= 5 and dev not in hinted:
                    hinted.add(dev)
                    print(f'  {dev} ({label}) is attached but not mounted: udisksctl mount -b {dev}', flush=True)
            if waited and waited % 30 == 0:
                print(f'  still waiting for {what}: {timeout - waited} s left', flush=True)
            time.sleep(1)
        given = input(f'\n>>> step {step}: no drive found; its mount path, or Enter to stop: ').strip()
        return Path(given) if given else None

    def eject(self, step, path):
        self.log(f'  syncing and ejecting {path}')
        subprocess.run(['sync'])
        if sys.platform == 'darwin':
            ok = subprocess.run(['diskutil', 'eject', str(path)]).returncode == 0
        else:
            dev = subprocess.run(['findmnt', '-no', 'SOURCE', str(path)], capture_output=True, text=True).stdout.strip()
            ok = bool(dev) and subprocess.run(['udisksctl', 'unmount', '-b', dev]).returncode == 0
            if ok:
                print(f'  powering off {dev}; udisksctl may wait here for your password', flush=True)
            ok = ok and subprocess.run(['udisksctl', 'power-off', '-b', dev]).returncode == 0
            if ok:
                print(f'  {dev} ejected', flush=True)
        if not ok:
            self.do(step, f'eject {path} safely')

    # -- the checklist --
    def prepare(self, chosen):
        a = self.a
        zips = sorted(p for p in a.candidate.glob('wrist-rs-*.zip') if not p.name.endswith('-debug.zip'))
        if len(zips) != 1:
            sys.exit(f'expected one wrist-rs-VERSION.zip in {a.candidate}, found {len(zips)}')
        z = zips[0]
        with zipfile.ZipFile(z) as f:
            manifest = json.loads(f.read('manifest.json'))
        self.m = manifest
        self.start(a.out or ROOT / 'target' / f'gate-{manifest["build_id"]}-{time.strftime("%Y%m%d-%H%M%S")}')
        self.log(f'candidate {z.name}: build {manifest["build_id"]}, image CRC {manifest["image_crc32"]}, '
                 f'version {manifest["version"]}, commit {manifest["source_commit"]}')
        sums = (a.candidate / 'SHA256SUMS').read_text().splitlines()
        for line in sums:
            digest, name = line.split('  ', 1)
            self.check(1, hashlib.sha256((a.candidate / name).read_bytes()).hexdigest() == digest, f'SHA-256 of {name}')
        self.pkg = self.out / 'package'
        with zipfile.ZipFile(z) as f:
            f.extractall(self.pkg)
        gate = json.loads((ROOT / 'release/gate.json').read_text())
        fp = manifest.get('round_trip_fingerprint')
        # gate.json keeps the last round trip's stock version until this one passes.
        self.stock = notes_stock()
        if not self.stock:
            sys.exit('release/NOTES.md names no stock version')
        self.round_trip = (a.round_trip or fp != gate.get('round_trip_fingerprint')
                           or self.stock != gate.get('stock_version'))
        self.log(f'  round-trip fingerprint {fp}; gate.json has {gate.get("round_trip_fingerprint")} '
                 f'from {gate.get("round_trip_release")} on stock {gate.get("stock_version")}; '
                 f'the notes name stock {self.stock}: section D '
                 + ('runs' if self.round_trip else 'carries over'))
        # Step 2.
        if self.round_trip and chosen & set(range(13, 19)):
            if not (a.ota and a.ota.is_file()):
                sys.exit('section D runs: pass --ota with the stock .ota the release notes name')
            if f'_{self.stock}.ota' not in a.ota.name:
                sys.exit(f'{a.ota.name} is not stock {self.stock}, which the release notes name')
            self.log(f'  {a.ota.name}: SHA-256 {hashlib.sha256(a.ota.read_bytes()).hexdigest()}')
            self.ask(2, f'is that the SHA-256 INSTALL.md says to check for stock {self.stock}?')
        pct = charge(self.cmd('', 12, until='soc ')) if wrist_port.find(a.port) else None
        if pct is None:
            self.ask(2, 'is the watch charged above 50 %?')
        else:
            self.check(2, pct > 50, f'the watch charged above 50 % ({pct:.0f} %)')
        # Step 3: gate-test from the same inputs as the candidate.
        here = firmware_build.build_id()
        if not self.check(3, here == manifest['build_id'], f'this checkout builds {manifest["build_id"]} (it builds {here})'):
            sys.exit('check out the candidate\'s commit first')
        app = ROOT / 'apps/standalone-boot'
        subprocess.run(['cargo', 'build', '--release', *firmware_build.feature_args('standalone-boot')], cwd=app, check=True,
                       env=dict(os.environ, WRIST_RELEASE_BUILD_ID='gate-test'))
        objcopy = next(filter(None, map(shutil.which, ['llvm-objcopy', 'arm-none-eabi-objcopy', 'rust-objcopy'])))
        self.gate_test = self.out / 'gate-test.bin'
        subprocess.run([objcopy, '-O', 'binary', str(app / 'target/thumbv8m.main-none-eabihf/release/standalone-boot'),
                        str(self.gate_test)], check=True)
        self.check(3, self.gate_test.stat().st_size > 0, 'gate-test.bin built')

    def update(self, step, image, *flags):
        rc, out = self.tool('wrist-update.py', image, *flags)
        self.save(f'step{step}-update-{int(time.time())}.txt', out)
        return rc == 0 and ('done: state boot' in out or '--no-reset' in flags and '[update] ok' in out)

    def app_crc(self):
        """The CRC-32 of the candidate's app.bin, which the image prints as `crc` in `version`."""
        return f'{zlib.crc32((self.pkg / "app.bin").read_bytes()):08X}'

    def version_is(self, step, build):
        text = self.booted()
        self.check(step, 'state boot' in text, 'state boot')
        ok = self.check(step, re.search(rf'build {re.escape(build)}(?: |$)', text, re.M) is not None, f'the build is {build}')
        return self.check(step, f'crc {self.app_crc()}' in text, f'the image is app.bin, CRC {self.app_crc()}') and ok

    def step4(self):
        self.refusals_and_update()
        self.pulled_cable()
        self.killed_tool()

    def refusals_and_update(self):
        before = self.cmd('power sleep', 3)
        if crash := re.search(r'^\[crash\] (?!no retained).*$', before, re.M):
            self.log(f'    before the gate, from the previous image: {crash[0]}; cleared')
            self.save('step4-crash-before.txt', before)
        self.cmd('power sleep clear-crash', 2)
        app, image = self.pkg / 'app.bin', self.pkg / 'first-install/Apps/ImageInstaller/image.bin'
        for flags, what in [((app, '--loader'), 'app.bin as a loader'), ((image,), 'image.bin as an application')]:
            t0 = time.time()
            rc, out = self.tool('wrist-update.py', *flags)
            self.check(4, rc != 0 and time.time() - t0 < 15 and 'sent' not in out, f'refuses {what} at once, sending nothing')
        self.check(4, self.update(4, app), 'update: [update] ok, state boot')
        self.installed(4, 'the candidate over USB')
        self.version_is(4, self.m['build_id'])

    def pulled_cable(self):
        app = self.pkg / 'app.bin'
        self.do(4, 'be ready to pull the cable when told, half way through the next update')
        self.tool('wrist-update.py', app, on_half=lambda: print('\n' + '!' * 60 + '\n    PULL THE CABLE NOW\n' + '!' * 60, flush=True))
        self.do(4, 'plug the cable back in')
        self.check(4, self.wait_port(60) is not None, 'the console came back')
        self.check(4, self.update(4, app), 'the next update is accepted')
        self.installed(4, 'the candidate again, after a pulled cable')

    def killed_tool(self):
        app = self.pkg / 'app.bin'
        self.tool('wrist-update.py', app, kill_at=True)
        # Listen only: until the watch gives up, what it is sent is image data.
        text = self.cmd('', 10)
        text += '\n' + self.cmd('version', 3)
        self.save('step4-killed-tool.txt', text)
        self.check(4, 'no data for 5 s; transfer abandoned' in text, '[update] error: no data for 5 s; transfer abandoned')
        self.check(4, f'build {self.m["build_id"]}' in text, 'version answers')
        self.check(4, self.update(4, app), 'the next update is accepted')
        self.installed(4, 'the candidate again, after a killed tool')

    def step5(self, step=5):
        self.ask(step, 'the face: day and date, the time and the charge inside the ring, and the minute rolls over on time?')
        rc, out = self.tool('wrist-time.py')
        self.save(f'step{step}-time.txt', out)
        self.check(step, '[rtc] set' in out, '[rtc] set')
        self.check(step, bool(re.search(r'\[rtc\] zone .*change\(s\) kept', out)), '[rtc] zone, change(s) kept')
        before = self.cmd('time', 3)
        self.cmd('reset', 1)
        time.sleep(3)
        self.booted()
        after = self.cmd('time', 3)
        self.save(f'step{step}-reset.txt', before + '\n--- after reset ---\n' + after)
        zone = lambda t: re.findall(r'^\s*(?:zone|change)\s*:.*$', t, re.M)
        self.check(step, bool(zone(before)) and zone(before) == zone(after), 'the zone and its changes survive reset')
        offset = time.localtime().tm_gmtoff // 60
        for frm, to, word in [(offset, offset + 60, 'forward'), (offset + 60, offset, 'back')]:
            # On USB the display task checks for a due change every 10 s.
            at = int(time.time()) + 15
            # Back: a minute more, through a minute's turn, in which it must not apply again.
            then = 60 if word == 'back' else 2
            self.log(f'  time zone {frm} {at} {to}: listening until it applies, at most 45 s, then {then} s more')
            text = self.cmd(f'time zone {frm} {at} {to}', 45, until='[rtc] zone change applied', then=then)
            self.save(f'step{step}-{word}.txt', text)
            applied = text.count('[rtc] zone change applied')
            self.check(step, applied == 1 and f'calendar {word}' in text, f'one zone change applied, calendar {word}')
            self.ask(step, 'is the face an hour ahead?' if word == 'forward' else 'is the face back to the right time?')
        rc, out = self.tool('wrist-time.py')
        self.check(step, '[rtc] set' in out, 'the real zone restored with wrist-time.py')

    def step6(self, step=6):
        text = self.cmd('report', 10)
        self.save(f'step{step}-report.txt', text)
        for pattern, what in [(r'SDMMC1 (?:was on, )?now off|SDMMC1 off', 'SDMMC1 off'),
                              (r'\d+ unused lines analog', 'unused lines analog'),
                              (r'RTC alarm A off', 'RTC alarm A off'),
                              (r'IMU reset to suspend', 'IMU in suspend'),
                              (r'compass reset to suspend', 'compass in suspend')]:
            self.check(step, bool(re.search(pattern, text)), what)
        for ok, what in park_checks(text):
            self.check(step, ok, what)

    def step7(self):
        text = self.cmd('modules', 3)
        self.save('step7-modules.txt', text)
        records = module_checks(text)
        if all(ok for ok, _ in records):
            for ok, what in records:
                self.check(7, ok, what)
        else:
            for line in re.findall(r'^\[mod\].*$', text, re.M):
                self.log('    ' + line.strip())
            self.ask(7, 'the module records above are this watch\'s (saved in step7-modules.txt)?')
        self.cmd('page lag 10', 1)
        time.sleep(11)
        text = self.cmd('page', 3)
        self.save('step7-page.txt', text)
        late = re.search(r'executor late by at most ([\d.]+) ms', text)
        self.check(7, bool(late) and float(late[1]) <= 5, f'executor late by at most {late[1] if late else "?"} ms')
        text = self.cmd('page bus', 2)
        self.save('step7-bus.txt', text)
        self.check(7, 'keeps the data window' in text and bool(re.search(r'\[bus\] frames \d+, failed 0\b', text)),
                   'the bus keeps the data window, failed 0')

    def step8(self):
        import serial
        s = wrist_port.settle(serial.Serial(self.wait_port(30), 115200, timeout=0.2))
        buf, lock, stop = [], threading.Lock(), threading.Event()

        def reader():
            while not stop.is_set():
                try:
                    data = s.read(4096)
                except Exception:
                    return
                if data:
                    with lock:
                        buf.append(data.decode('utf-8', 'replace'))
        threading.Thread(target=reader, daemon=True).start()

        def text():
            with lock:
                return ''.join(buf)

        def expect(pattern, what, timeout=20, mark=None):
            mark = len(text()) if mark is None else mark
            input(f'\n>>> step 8: {what}\n    Enter, then do it. ')
            end = time.time() + timeout
            while time.time() < end:
                if re.search(pattern, text()[mark:]):
                    return self.check(8, True, what)
                time.sleep(0.2)
            return self.check(8, False, what)

        def short(button, then):
            return expect(press(button, then), f'a short press of {button}: {then}')

        short('Back', 'face lit for 3 s')
        short('Select', 'face lit for 3 s')
        # Inside Select's 3 s light, its timer would take the console's light off.
        time.sleep(3.5)
        s.write(b'page light on\r\n')
        time.sleep(0.5)
        short('Back', "the console's light off")
        short('Back', 'face lit for 3 s')
        # Each press restarts the pages' 10 s, so the moves go first, with no question between.
        for button, page in [('Down', 'battery'), ('Down', 'about'), ('Down', 'face'),
                             ('Up', 'about'), ('Up', 'battery'), ('Back', 'face')]:
            short(button, f'[page] {page}')
        cargo = (ROOT / 'apps/standalone-boot/Cargo.toml').read_text()
        version = re.search(r'(?m)^version\s*=\s*"([^"]*)"', cargo)[1]
        for button, page, seen in [('Down', 'battery', 'the charge, the voltage and input USB'),
                                   ('Up', 'about', f'wrist-rs, {version}, {self.m["build_id"].upper()} and CRC {self.app_crc()}')]:
            mark = len(text())
            short(button, f'[page] {page}')
            self.ask(8, f'the {page} page: {seen}?')
            expect(r'\[page\] face, timed out', f'leave the {page} page alone: the face comes back within 10 s', mark=mark)
        self.do(8, 'hold Back until the watch restarts, then let go')
        stop.set()
        s.close()
        self.save('step8-buttons.txt', text())
        self.booted()
        report = self.cmd('report', 6)
        flags = handoff_reset_flags(report)
        self.save('step8-report.txt', report)
        self.check(8, flags is not None and 'PIN' in flags and not {'IWDG', 'WWDG'} & set(flags),
                   f'reset flags at handoff {" ".join(flags) if flags is not None else "?"}: PIN, no IWDG or WWDG')
        self.ask(8, 'the face is back with the time still right?')

    def step9(self):
        self.before = self.cmd('power', 3)
        self.do(9, 'unplug the cable and leave the watch at least ten minutes (an hour or a night is better)')
        self.ask(9, 'the face kept time, and a short press lit it?')

    def step10(self):
        input('\n>>> step 10: plug the cable back in, then Enter at once. ')
        t0 = time.time()
        port = self.wait_port(60)
        self.check(10, port is not None, f'the console back within a minute ({time.time() - t0:.0f} s)')

    def step11(self):
        text = self.cmd('power', 4)
        self.save('step11-power.txt', self.before + '\n--- after battery ---\n' + text)
        interrupts = lambda t: int(m[1]) if (m := re.search(r'input interrupts (\d+)', t)) else None
        rate = wake_rate(text)
        for ok, what in [('input external' in text, 'the input is external'),
                         ('unanswered input reads in a row 0; Stop follows the input' in text, 'no unanswered input reads'),
                         ('PWR_INT_L high' in text, 'PWR_INT_L high'),
                         (interrupts(text) is not None and interrupts(self.before) is not None
                          and interrupts(text) - interrupts(self.before) == 2, 'input interrupts up by two'),
                         (rate is not None and 30 <= rate <= 50, f'Stop 2 wakes a minute {rate and round(rate, 1)}, about 36'),
                         ('alarms handled over 5 ms late 0' in text, 'no late alarms'),
                         ('Stop entered with one unacknowledged 0' in text, 'Stop never entered with one unacknowledged'),
                         ('[crash] no retained failure' in text, 'no panic or fault retained (wrist-crash.py names a panic\'s file)')]:
            self.check(11, ok, what)
        for line in re.findall(r'^.*last on battery.*$', text, re.M):
            self.log('    ' + line.strip())

    def step12(self):
        self.check(12, self.update(12, self.gate_test, '--no-reset'), 'gate-test written, not swapped in')
        self.installed(12, 'gate-test, then the revert')
        self.do(12, 'unplug, then hold Back until the watch restarts into gate-test')
        self.ask(12, 'the face is back, and Up shows the about page with GATE-TEST?')
        for left in range(30, 0, -5):
            print(f'    {left} s', flush=True)
            time.sleep(5)
        self.do(12, 'hold Back until the watch restarts again (the loader puts the previous image back), then plug in')
        self.version_is(12, self.m['build_id'])

    def to_stock(self, step):
        self.do(step, 'hold Select past the USB screen at about 3 s, until the screen blanks at about 16 s '
                      'and the USB screen comes back; then let go and connect the cable')
        reset = self.ask(step, 'did the screen blank and come back (the power reset)?')
        if step == 13:
            self.power_reset = reset
        boot = self.drive(step, lambda v: v.name == 'UNA BOOT', 'UNA BOOT')
        if not boot:
            sys.exit('no UNA BOOT drive')
        shutil.copy2(self.a.ota, boot / self.a.ota.name)
        self.check(step, hashlib.sha256((boot / self.a.ota.name).read_bytes()).digest()
                   == hashlib.sha256(self.a.ota.read_bytes()).digest(), f'{self.a.ota.name} copied to UNA BOOT')
        self.eject(step, boot)
        self.do(step, 'unplug the cable and choose Continue')
        self.ask(step, f'stock started and reports {self.stock}?')

    def dry_run(self, step):
        self.do(step, 'connect the cable to stock')
        drive = self.drive(step, lambda v: v.name != 'UNA BOOT' and (v / 'Apps').is_dir(), "the watch's drive")
        if not drive:
            sys.exit('no stock drive')
        folder = drive / 'Apps/ImageInstaller'
        folder.mkdir(parents=True, exist_ok=True)
        shutil.copytree(folder, self.out / f'step{step}-backup-ImageInstaller', dirs_exist_ok=True)
        for p in folder.iterdir():
            if p.is_file() and (p.name in ('ARM_INSTALL', 'image.bin') or p.suffix in ('.uapp', '.log', '.prev')):
                p.unlink()
        for p in (self.pkg / 'first-install/Apps/ImageInstaller').iterdir():
            shutil.copy2(p, folder / p.name)
        self.log(f'  {folder}: ' + ', '.join(sorted(p.name for p in folder.iterdir())))
        self.eject(step, drive)
        self.do(step, 'unplug and restart the watch, then open the launcher on battery')
        crc = self.m['image_crc32']
        self.ask(step, 'the launcher lists Wrist-rs?')
        self.do(step, 'open Wrist-rs and wait for Checking to end')
        self.ask(step, f'the screen shows {crc} and "Dry run passed"?')
        self.do(step, 'press Back to leave, then plug the cable in')
        drive = self.drive(step, lambda v: (v / 'Apps/ImageInstaller/install.log').is_file(),
                           "the watch's drive, with install.log")
        log = (drive / 'Apps/ImageInstaller/install.log').read_text(errors='replace')
        self.save(f'step{step}-install.log', log)
        size = (self.pkg / 'first-install/Apps/ImageInstaller/image.bin').stat().st_size
        for ok, what in check_install_log(log, crc, size):
            self.check(step, ok, 'install.log: ' + what)
        return drive, log

    def install(self, step, drive):
        drive = drive or self.drive(step, lambda v: (v / 'Apps/ImageInstaller').is_dir(), "the watch's drive")
        self.eject(step, drive)
        crc = self.m['image_crc32']
        self.do(step, f'unplug, open Wrist-rs, and once it shows {crc}, hold Select until it reads Installing')
        shown = input(f'\n>>> step {step}: how many seconds did Installing stay on screen? ').strip()
        self.log(f'  step {step}: Installing on screen {shown} s')
        self.do(step, 'plug the cable back in once wrist-rs shows its face')
        self.installed(step, 'the candidate from stock, with Wrist-rs\'s hold')
        self.version_is(step, self.m['build_id'])

    def step13(self):
        self.to_stock(13)

    def step14(self):
        self.drive14, _ = self.dry_run(14)

    def step15(self):
        drive = self.drive14 or self.drive(15, lambda v: (v / 'Apps/ImageInstaller').is_dir(), "the watch's drive")
        self.eject(15, drive)
        crc = self.m['image_crc32']
        self.do(15, f'unplug, and open Wrist-rs until it shows {crc}')
        for what in ['click Select once',
                     'hold Select for about two seconds and let go',
                     'hold Select, and press Back during the countdown',
                     # The launcher opens an app on release, so the press starts once it is open.
                     'press Back to leave; open Wrist-rs again, press Select while it reads Checking, '
                     'and keep holding until the ready screen appears']:
            self.do(15, what)
            self.ask(15, f'it did not install: the screen is back to {crc} and "Hold Select to install"?')
        note = input('\n>>> step 15: anything else a 3 s hold of Select brought up (Enter for nothing)? ').strip()
        self.log(f'  step 15: other things a 3 s hold brings up: {note or "nothing"}')
        self.do(15, 'press Back to leave, and plug the cable in')
        self.drive14 = self.drive(15, lambda v: v.name != 'UNA BOOT' and (v / 'Apps/ImageInstaller').is_dir(),
                                 "the watch's drive")
        self.check(15, bool(self.drive14), 'still stock: its drive is back')

    def step16(self):
        self.install(16, self.drive14)
        text = self.cmd('time', 3)
        self.save('step16-time-after-hold.txt', text)
        if self.power_reset:
            self.check(16, bool(re.search(r'^\s*zone\s*: no record', text, re.M)), 'after the 16 s hold, no zone record')
        else:
            self.log('  step 16: zone check skipped: no power reset in this run\'s step 13')
        self.step5(16)
        self.step6(16)

    def step17(self):
        self.to_stock(17)
        self.drive17, log = self.dry_run(17)
        for ok, what in witness_checks(log, self.m['image_crc32']):
            self.check(17, ok, 'install.log: ' + what)
        staging = re.search(r'^staging.*$', log, re.M)
        self.log('  staging as the recovery left it: ' + (staging[0] if staging else '?'))

    def step18(self):
        self.install(18, self.drive17)

    def summary(self):
        failed = [r for r in self.results if r[1] == 'FAIL']
        lines = [f'# Gate run: build {self.m["build_id"]}, image CRC {self.m["image_crc32"]}', '',
                 f'Version {self.m["version"]}, commit {self.m["source_commit"]}, round-trip fingerprint '
                 f'{self.m.get("round_trip_fingerprint")}; section D {"ran" if self.round_trip else "carried over"}.', '',
                 f'{len(self.results) - len(failed)} passed, {len(failed)} failed.', '',
                 '| Step | Result | Check |', '|---|---|---|']
        lines += [f'| {s} | {r} | {w} |' for s, r, w in self.results]
        lines += ['', 'Numbered installs to record:', '']
        lines += [f'- step {s}: {w}' for s, w in self.installs]
        self.save('summary.md', '\n'.join(lines) + '\n')
        return not failed


def steps(spec):
    chosen = set()
    for part in spec.split(','):
        lo, _, hi = part.partition('-')
        chosen.update(range(int(lo), int(hi or lo) + 1))
    return chosen


def run_order(chosen):
    """The steps after 1 to 3 in the order they run: section B, the battery stretch, is the
    gate's one long wait, so it goes last (GATE.md)."""
    return sorted(chosen - {1, 2, 3}, key=lambda n: (n in (9, 10, 11), n))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('candidate', type=Path, help="the directory holding the candidate's zips and SHA256SUMS")
    ap.add_argument('--ota', type=Path, help='the stock .ota, for section D')
    ap.add_argument('--steps', default='4-18', help='which steps, as 4-8,11; 1 to 3 always run')
    ap.add_argument('--round-trip', action='store_true', help='run section D even when it carries over')
    ap.add_argument('--port')
    ap.add_argument('--out', type=Path)
    a = ap.parse_args()
    g = Gate(a)
    chosen = steps(a.steps)
    g.prepare(chosen)
    if not g.round_trip:
        chosen -= set(range(13, 19))
    for n in run_order(chosen):
        g.log(f'\n== step {n} ==')
        getattr(g, f'step{n}')()
    ok = g.summary()
    print(f'\n{"all checks passed" if ok else "some checks FAILED"}; see {g.out / "summary.md"}')
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        sys.exit('stopped')
