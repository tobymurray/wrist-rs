#!/usr/bin/env python3
"""Host tests for ble-suite.py's verdicts and pre-flight; no watch needed."""
import argparse
import asyncio
import importlib.util
import io
import os
from pathlib import Path
import re
import shlex
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('ble_suite', Path(__file__).with_name('ble-suite.py'))
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)


class Usb:
    def __init__(self, lines):
        self.lines = lines

    def mark(self):
        return 0

    def since(self, mark):
        return []

    async def ask(self, text, pattern, timeout=3):
        return next((l for l in self.lines.get(text, []) if re.search(pattern, l)), None)


class Phone:
    async def update(self, image):
        return True


class Link:
    def __init__(self):
        self.sent = []

    async def command(self, text, pattern, timeout=3):
        self.sent.append(text)
        return 'ok'


class Console:
    """A watch console that answers commands by `replies`, a dict of command to lines."""

    def __init__(self, replies):
        self.replies, self.lines, self.sent = replies, [], []

    def mark(self):
        return len(self.lines)

    def since(self, mark):
        return self.lines[mark:]

    def send(self, text):
        self.sent.append(text)
        reply = self.replies.get(text, [])
        for line in (reply(self) if callable(reply) else reply):
            self.lines.append((time.time(), line))

    async def wait(self, pattern, mark, timeout):
        return next(((t, l) for t, l in self.lines[mark:] if re.search(pattern, l)), (None, None))

    async def ask(self, text, pattern, timeout=3):
        mark = self.mark()
        self.send(text)
        return (await self.wait(pattern, mark, timeout))[1]

    def close(self):
        pass


def image(build):
    f = tempfile.NamedTemporaryFile(suffix='.bin')
    f.write(b'\0[step] build : ' + build.encode() + b'\0\0unknown command\0')
    f.flush()
    return f


def fake_sudo(d, script):
    Path(d, 'sudo').write_text('#!/bin/sh\n' + script)
    os.chmod(Path(d, 'sudo'), 0o755)
    return mock.patch.dict(os.environ, {'PATH': f'{d}:{os.environ["PATH"]}'})


class Quiet(suite.Host):
    def other_tools(self):
        return []

    def holders(self, port):
        return []


def snap(build, ble=True):
    return {'version': f'[usb] standalone-boot 0.3.0 build {build}', 'build': build, 'boot': '[boot] state boot',
            'ble': '[ble] advertising as UNA, timing cable' if ble else None, 'bonds': '[ble] bonded devices: 1'}


def suite_args(**kw):
    base = dict(port=None, install=None, restore=None, image=None, btmon=False, name='UNA', scenarios=['console'], sessions=1)
    base.update(kw)
    return argparse.Namespace(**base)


def run(scenario, usb, phone, build, **a):
    s = suite.Scenario(scenario.__name__, '')
    with mock.patch('sys.stdout', io.StringIO()):
        asyncio.run(scenario(s, usb, phone, build, argparse.Namespace(**a)))
    return s.passed


class Judgements(unittest.TestCase):
    def test_a_dirty_build_is_not_the_clean_one(self):
        with tempfile.NamedTemporaryFile() as f, mock.patch.object(suite.wrist_update, 'mismatch', return_value=None):
            f.write(b'\0build 20b5b6c11988-dirty\0')
            f.flush()
            self.assertTrue(suite.image_problems(f.name, '20b5b6c11988', '--image'))
            self.assertFalse(suite.image_problems(f.name, '20b5b6c11988-dirty', '--image'))
        usb = Usb({'version': ['[usb] standalone-boot 0.3.0 build 20b5b6c11988-dirty'], 'ble': ['[ble] advertising as UNA, timing cable']})
        self.assertFalse(run(suite.update, usb, Phone(), '20b5b6c11988', update_image=None))

    def test_no_scenarios_is_refused(self):
        async def nothing(a, out, result):
            return True
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'run', nothing), \
                mock.patch.object(sys, 'argv', ['ble-suite.py', '--scenarios', '', '--out', f'{d}/run']), \
                mock.patch('sys.stderr', io.StringIO()), mock.patch('sys.stdout', io.StringIO()):
            with self.assertRaises(SystemExit) as end:
                suite.main()
        self.assertNotEqual(end.exception.code, 0)

    def test_a_run_that_dies_still_writes_its_result(self):
        async def dies(a, out, result):
            raise RuntimeError('the watch\'s USB console is gone')
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'run', dies), \
                mock.patch.object(sys, 'argv', ['ble-suite.py', '--out', f'{d}/run']), \
                mock.patch('sys.stdout', io.StringIO()) as shown:
            with self.assertRaises(SystemExit) as end:
                suite.main()
            self.assertIn('gone', (Path(d) / 'run/result.json').read_text())
        self.assertIn('Traceback', shown.getvalue())
        self.assertNotEqual(end.exception.code, 0)

    def test_the_suite_is_not_another_tool(self):
        probe = (f'import importlib.util; s = importlib.util.spec_from_file_location("s", {str(spec.origin)!r}); '
                 'm = importlib.util.module_from_spec(s); s.loader.exec_module(m); print(m.Host().other_tools())')
        inner = subprocess.list2cmdline([sys.executable, '-c', probe])
        apart = f'import subprocess, sys; subprocess.run([sys.executable, "-c", {probe!r}], start_new_session=True)'
        for shell in [f'sh -c {shlex.quote(inner + "; :")}; : ble-suite.py #me{os.getpid()}',
                      f'{sys.executable} -c {shlex.quote(apart)}; : ble-suite.py #me{os.getpid()}']:
            out = subprocess.run(['sh', '-c', shell], capture_output=True, text=True).stdout
            self.assertNotIn(f'#me{os.getpid()}', out)
        them = f'{sys.executable} -c "import time; time.sleep(3)" tools/wrist-cmd.py them'
        out = subprocess.run(['sh', '-c', f'{them} & {inner}; wait'], capture_output=True, text=True).stdout
        self.assertIn('wrist-cmd.py them', out)
        self.assertTrue(suite.OTHER_TOOLS.search('python3 tools/hr-sim-bumble.py hci-socket:0'))

    def test_the_radio_is_judged_from_the_status_line(self):
        status = '[ble] up, not advertising, timing cable, address 0'
        usb = Usb({'ble': ['[ble] on at boot: `ble on` was the last word (BKP16R)', '[ble] advertising refused, status 0C', status],
                   'version': ['[usb] standalone-boot 0.3.0 build b']})
        self.assertFalse(run(suite.update, usb, Phone(), 'b', update_image=None))
        self.assertEqual(asyncio.run(suite.snapshot(usb))['ble'], status)

    def test_no_reconnects_is_not_a_pass(self):
        self.assertFalse(run(suite.reconnect, None, None, 'b', reconnects=0, encrypt_limit=2.0))

    def test_traffic_always_sends(self):
        link = Link()
        self.assertTrue(asyncio.run(suite.traffic(link, 0)))
        self.assertEqual(link.sent, ['ble', 'ping'])

    def test_soak_reads_counters_from_the_status_line(self):
        event = '[ble] advertising as UNA every 100 ms'
        status = '[ble] advertising as UNA, timing cable, address 0, bonded 1, events 9, fragments {}'
        answers = iter([[event, status.format(0)], [status.format(0)]])

        class Soak(Usb):
            async def ask(self, text, pattern, timeout=3):
                if text == 'ble':
                    self.lines['ble'] = next(answers)
                return await super().ask(text, pattern, timeout)

            def send(self, text):
                pass

        usb = Soak({'uptime': ['[usb] uptime 5 s']})
        s = suite.Scenario('soak', '')
        a = argparse.Namespace(soak_minutes=0, hold=0, gap=0)
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(suite.soak(s, usb, None, 'b', a))
        self.assertTrue(s.checks[1]['passed'], s.checks[1]['what'])

    def test_a_field_of_two_counters_names_both(self):
        found = suite.counters('[ble] connected, encrypted, interval 30.00 ms latency 0, bonded 2, events 40, '
                               'irq stuck after read 1 write 2, connect failures 3, unknown events 2')
        self.assertEqual(found, {'irq stuck after read': 1, 'irq stuck after write': 2, 'connect failures': 3})

    def test_connections_that_failed_to_establish_are_counted(self):
        def record(opcode, packet):
            return struct.pack('>IIIIq', len(packet), len(packet), opcode, 0, 0) + packet
        disconnect = lambda reason: bytes([0x05, 4, 0, 0x40, 0, reason])
        capture = b'btsnoop\0' + struct.pack('>II', 1, 2001) + b''.join([
            record(3, disconnect(0x3E)), record(3, disconnect(0x13)), record(2, disconnect(0x3E)),
            record(3, bytes([0x3E, 3, 0x14, 0x40, 0])), record(0x10003, disconnect(0x3E))])
        with tempfile.NamedTemporaryFile() as f:
            f.write(capture)
            f.flush()
            self.assertEqual(suite.failed_to_establish(f.name), 2)
            f.seek(0)
            f.write(b'not a capture')
            f.flush()
            self.assertIsNone(suite.failed_to_establish(f.name))

    def test_the_build_is_matched_exactly(self):
        with mock.patch.object(suite.wrist_update, 'mismatch', return_value=None):
            with image('20b5b6c11988') as f:
                self.assertFalse(suite.image_problems(f.name, '20b5b6c11988', '--image'))
                self.assertTrue(suite.image_problems(f.name, '20b5b6c1198', '--image'))
                self.assertTrue(suite.image_problems(f.name, '0b5b6c11988', '--image'))
                self.assertTrue(suite.image_problems(f.name, 'unknown', '--image'))
            with image('20b5b6c11988.1') as f:
                self.assertTrue(suite.image_problems(f.name, '20b5b6c11988', '--image'))
            with image('unknown') as f:
                self.assertIn('unknown', suite.image_problems(f.name, 'unknown', '--restore')[0])

    def install_run(self, image_build):
        """run() with --install, a clean installed build and an --image of `image_build`."""
        installs = []

        async def install(usb, a, path, out):
            installs.append(path)
            return True, usb

        builds = iter(['0ld0ld0ld0ld', 'c1ea2c1ea2c1', '0ld0ld0ld0ld', '0ld0ld0ld0ld'])

        async def snapshot(usb):
            return snap(next(builds))

        files = {name: image(build) for name, build in
                 [('old', '0ld0ld0ld0ld'), ('new', 'c1ea2c1ea2c1'), ('image', image_build)]}
        a = suite_args(install=Path(files['new'].name), restore=Path(files['old'].name), image=Path(files['image'].name))
        result = {'scenarios': []}
        seen = {}

        async def console(s, usb, phone, build, a):
            seen['image'] = a.update_image
            s.check(True, 'ran')
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: Console({})), \
                mock.patch.object(suite, 'snapshot', snapshot), mock.patch.object(suite, 'install', install), \
                mock.patch.object(suite, 'console', console), \
                mock.patch.object(suite.wrist_update, 'mismatch', return_value=None), \
                mock.patch('sys.stdout', io.StringIO()):
            ok = asyncio.run(suite.run(a, Path(d), result))
        for f in files.values():
            f.close()
        return ok, result, installs, seen, a

    def test_image_beside_install_is_the_one_sent_and_must_be_the_installed_build(self):
        ok, result, installs, seen, a = self.install_run('c1ea2c1ea2c1-dirty')
        self.assertFalse(ok)
        self.assertIn(f'--image {a.image} is not build c1ea2c1ea2c1', result['preflight'])
        self.assertEqual(installs, [a.install, a.restore])
        self.assertTrue(result['restored'])
        self.assertNotIn('image', seen)
        ok, result, installs, seen, a = self.install_run('c1ea2c1ea2c1')
        self.assertTrue(ok, result.get('preflight'))
        self.assertEqual(seen['image'], a.image)

    def test_a_failed_capture_is_said_and_fails_the_run(self):
        with tempfile.TemporaryDirectory() as d, fake_sudo(d, 'echo "sudo: a password is required" >&2; exit 1\n'), \
                mock.patch('sys.stdout', io.StringIO()) as shown:
            self.assertIsNone(suite.Linux().capture(Path(d) / 'btmon.btsnoop'))
        self.assertIn('btmon capture failed (exit 1: sudo: a password is required)', shown.getvalue())

        class NoCapture(Quiet):
            def capture(self, path):
                return None
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', NoCapture), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch('sys.stdout', io.StringIO()):
            self.assertFalse(asyncio.run(suite.run(suite_args(btmon=True), Path(d), result)))
        self.assertEqual(result['capture'], 'failed')

    def test_a_capture_that_wrote_nothing_is_failed_not_absent(self):
        class Silent(Quiet):
            def capture(self, path):
                return subprocess.Popen(['sleep', '30'])

        async def console(s, usb, phone, build, a):
            s.check(True, 'ran')
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Silent), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: Console({})), \
                mock.patch.object(suite, 'snapshot', lambda usb: asyncio.sleep(0, snap('b'))), \
                mock.patch.object(suite, 'console', console), mock.patch('sys.stdout', io.StringIO()) as shown:
            asyncio.run(suite.run(suite_args(btmon=True), Path(d), result))
        self.assertEqual(result['capture'], 'failed')
        self.assertIsNone(result['failed_to_establish'])
        self.assertIn('btmon capture failed', shown.getvalue())

    def test_holders_the_suite_cannot_see(self):
        listdir = os.listdir

        def proc(path):
            if path == '/proc':
                return ['1', '2']
            if path == '/proc/1/fd':
                raise PermissionError(path)
            return [] if path.startswith('/proc/') else listdir(path)
        ps = ['1 sudo sh -c exec 3</dev/ttyACM0; sleep 120', '2 bash']
        with mock.patch.object(suite.os, 'listdir', proc), mock.patch.object(suite.Linux, 'ps', lambda self: ps):
            with mock.patch.object(suite.Linux, 'fuser', lambda self, port: ['1']):
                self.assertEqual(suite.Linux().holders('/dev/ttyACM0'), ps[:1])
            with mock.patch.object(suite.Linux, 'fuser', lambda self, port: None), \
                    mock.patch('sys.stdout', io.StringIO()) as shown:
                self.assertEqual(suite.Linux().holders('/dev/ttyACM0'), [])
            self.assertIn('can\'t see 1 processes of other users', shown.getvalue())
        with tempfile.TemporaryDirectory() as d:
            with fake_sudo(d, 'echo "sudo: a password is required" >&2; exit 1\n'):
                self.assertIsNone(suite.Linux().fuser('/dev/ttyACM0'))
            with fake_sudo(d, 'echo " 4242"; echo "/dev/ttyACM0:" >&2\n'):
                self.assertEqual(suite.Linux().fuser('/dev/ttyACM0'), ['4242'])
            with fake_sudo(d, 'echo "sudo: fuser: command not found" >&2; exit 1\n'):
                self.assertIsNone(suite.Linux().fuser('/dev/ttyACM0'))

    def test_macos_holders_include_the_tty_twin(self):
        asked = []

        def lsof(argv, **kw):
            asked.append(argv)
            return subprocess.CompletedProcess(argv, 0, '7\n', '')
        with mock.patch.object(suite.subprocess, 'run', lsof), mock.patch.object(suite.os.path, 'exists', return_value=True), \
                mock.patch.object(suite.Host, 'ps', lambda self: ['7 screen /dev/tty.usbmodem101']):
            self.assertEqual(suite.Host().holders('/dev/cu.usbmodem101'), ['7 screen /dev/tty.usbmodem101'])
        self.assertEqual(asked, [['lsof', '-t', '/dev/cu.usbmodem101', '/dev/tty.usbmodem101']])


if __name__ == '__main__':
    unittest.main()
