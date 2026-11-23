#!/usr/bin/env python3
"""Host tests for ble-suite.py's verdicts and pre-flight; no watch needed."""
import argparse
import asyncio
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import textwrap
import time
import types
import unittest
from unittest import mock
import zlib

spec = importlib.util.spec_from_file_location('ble_suite', Path(__file__).with_name('ble-suite.py'))
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)
# No wait for a late alarm in the tests that run scenarios; `Alarms` tests the wait itself.
suite.LATE_ALARM_S = 0


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


def image(build, before=b'\0[step] build : '):
    f = tempfile.NamedTemporaryFile(suffix='.bin')
    f.write(before + build.encode() + b'\0\0unknown command\0')
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
    base = dict(port=None, install=None, restore=None, image=None, btmon=False, name='UNA', scenarios=['console'], sessions=1,
                phone_adapter=None,
                restart_radio=False, profile=None, air=False)
    base.update(kw)
    return argparse.Namespace(**base)


def run(scenario, usb, phone, build, **a):
    s = suite.Scenario(scenario.__name__, '')
    with mock.patch('sys.stdout', io.StringIO()):
        asyncio.run(scenario(s, usb, phone, build, argparse.Namespace(**{'profile': None, **a})))
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
        with tempfile.TemporaryDirectory() as d:
            Path(d, 'wrist-cmd.py').write_text('import time; time.sleep(3)\n')
            them = subprocess.list2cmdline([sys.executable, str(Path(d, 'wrist-cmd.py')), 'them'])
            out = subprocess.run(['sh', '-c', f'{them} & {inner}; wait'], capture_output=True, text=True).stdout
        self.assertIn('wrist-cmd.py them', out)

    def test_a_tool_is_its_executable_or_its_script_never_a_mention(self):
        for args in ['python3 tools/hr-sim-bumble.py hci-socket:0', '/usr/bin/python3.12 -u ./tools/ble-suite.py --air',
                     'python3 -X dev tools/wrist-cmd.py ble', '/home/t/tools/wrist-session.py log cmds',
                     'picocom -b 115200 /dev/ttyACM0', 'screen /dev/ttyACM0 115200', 'minicom', 'SCREEN /dev/ttyACM0',
                     '/Library/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python '
                     'tools/wrist-ble.py --pair', 'python3.14t tools/wrist-cmd.py ble',
                     'tio /dev/ttyACM0', 'cu -l /dev/cu.usbmodem1101', 'python3.14d tools/wrist-cmd.py']:
            self.assertTrue(suite.tool_of(args), args)
        for args in ['vim tools/ble-suite.py', 'grep -n alarm tools/ble-suite.py', 'sh -c sleep 5; tools/ble-suite.py',
                     'python3 -c import time tools/wrist-cmd.py', 'python3 -m pytest tools/test-ble-suite.py',
                     'python3 tools/test-ble-suite.py', 'less hr-sim.py', 'screen -r', '', 'bash tools/wrist-cmd.py']:
            self.assertFalse(suite.tool_of(args), args)

    def test_the_radio_is_judged_from_the_status_line(self):
        status = '[ble] up, not advertising, timing cable, address 0'
        usb = Usb({'ble': ['[ble] on at boot: `ble on` was the last word (BKP29R)', '[ble] advertising refused, status 0C', status],
                   'version': ['[usb] standalone-boot 0.3.0 build b']})
        self.assertFalse(run(suite.update, usb, Phone(), 'b', update_image=None))
        self.assertEqual(asyncio.run(suite.snapshot(usb))['ble'], status)

    def test_the_radio_is_back_once_it_advertises_after_the_hang_up(self):
        replies = iter([['[ble] up, not advertising, timing cable'], ['[ble] advertising as UNA, timing cable']])
        usb = Console({'version': ['[usb] build b'], 'ble': lambda _: next(replies)})
        self.assertTrue(run(suite.update, usb, Phone(), 'b', update_image=None))
        self.assertEqual(usb.sent.count('ble'), 2)

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
                if text == 'ble' and pattern == suite.COUNTS:
                    return '[ble] counts: failures none; chip resets none'
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

    def soak_refusals(self, held):
        status = '[ble] connected, timing cable, events 9, core asked {} refused {} failed 0 timed out 0 reasks 0'
        answers = iter([[status.format(5, 3)], [status.format(6, 4)]])

        class Soak(Usb):
            async def ask(self, text, pattern, timeout=3):
                if text == 'ble' and pattern == suite.COUNTS:
                    return '[ble] counts: failures none; chip resets none'
                if text == 'ble':
                    self.lines['ble'] = next(answers)
                return await super().ask(text, pattern, timeout)

            def since(self, mark):
                return [(0, '[ble] interval request held: the central has yet to answer the one before')] * held

            def send(self, text):
                pass

        s = suite.Scenario('soak', '')
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(suite.soak(s, Soak({'uptime': ['[usb] uptime 5 s']}), None, 'b',
                                   argparse.Namespace(soak_minutes=0, hold=0, gap=0)))
        return s.checks[1]['passed']

    def test_counts_moved_names_only_the_parts_a_soak_leaves_alone(self):
        line = ('[ble] counts: failures none; phone hang-ups 13 x{h}; pairings and bonded reconnects ok {h}, pairings failed none; hung up within 10 s '
                'of encrypting {e}; chip restarts {r}; alarms none; operations in a minute at most {o}; unreachable now 0 s, worst 0 s; '
                'plan drift now 0 s, worst 0 s')
        before = line.format(h=3, e='none', r='none', o=4)
        self.assertEqual(suite.counts_moved(before, line.format(h=9, e='none', r='none', o=7), 30), [])
        self.assertEqual(suite.counts_moved(before, line.format(h=9, e='none', r='deadline x1', o=7), 30), ['chip restarts'])
        self.assertEqual(suite.counts_moved(before, '', 30), ['failures', 'pairings failed', 'hung up within', 'chip restarts', 'alarms'])

    def test_over_the_air_only_the_centrals_own_early_hang_ups_are_the_soaks(self):
        line = '[ble] counts: failures none; hung up within 10 s of encrypting {e}; chip restarts none'
        before = line.format(e='13 x2')
        self.assertEqual(suite.counts_moved(before, line.format(e='13 x9'), 30, own_hang_ups=True), [])
        self.assertEqual(suite.counts_moved(before, line.format(e='13 x9 08 x1'), 30, own_hang_ups=True), ['hung up within'])
        self.assertEqual(suite.counts_moved(line.format(e='none'), line.format(e='13 x4'), 30, own_hang_ups=True), [])
        self.assertEqual(suite.counts_moved(before, line.format(e='13 x9 08 x1'), 5, own_hang_ups=True), ['hung up within'])

    def test_a_link_a_resync_found_gone_moves_the_counts(self):
        line = '[ble] counts: failures none; phone hang-ups 13 x1{g}; chip restarts none'
        self.assertEqual(suite.counts_moved(line.format(g=''), line.format(g='; phone links found gone by a resync 1'), 30),
                         ['phone links found gone'])

    def test_a_soak_holding_links_briefly_leaves_early_hang_ups_alone(self):
        line = '[ble] counts: failures none; hung up within 10 s of encrypting {e}; chip restarts none'
        before, after = line.format(e='none'), line.format(e='13 x4')
        self.assertEqual(suite.counts_moved(before, after, 5), [])
        self.assertEqual(suite.counts_moved(before, after, 13), ['hung up within'])

    def test_a_refusal_no_held_ask_explains_moves_the_counters(self):
        self.assertFalse(self.soak_refusals(held=0))

    def test_an_ask_held_for_the_central_is_no_counter(self):
        self.assertTrue(self.soak_refusals(held=1))

    def test_a_field_of_two_counters_names_both(self):
        found = suite.counters('[ble] connected, encrypted, interval 30.00 ms latency 0, bonded 2, events 40, '
                               'irq stuck after read 1 write 2, connect failures 3, unknown events 2')
        self.assertEqual(found, {'irq stuck after read': 1, 'irq stuck after write': 2, 'connect failures': 3})

    def test_the_cores_ordinary_work_is_no_counter(self):
        found = suite.counters('[ble] connected, core asked 12 refused 1 failed 0 timed out 0 reasks 2 '
                               'divergences 0 busy restarts 0 dropped 0 restarts 0, shadow disagreements 0')
        self.assertEqual(found, {'core failed': 0, 'timed out': 0, 'core divergences': 0,
                                 'busy restarts': 0, 'core dropped': 0, 'core restarts': 0, 'shadow disagreements': 0})

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

    def test_a_capture_that_died_counts_nothing(self):
        def record(packet):
            return struct.pack('>IIIIq', len(packet), len(packet), 3, 0, 0) + packet

        class Dying(Quiet):
            def capture(self, path):
                path.write_bytes(b'btsnoop\0' + struct.pack('>II', 1, 2001) + record(bytes([0x05, 4, 0, 0x40, 0, 0x3E])))
                return subprocess.Popen(['true'])

        async def console(s, usb, phone, build, a):
            await asyncio.sleep(0.5)
            s.check(True, 'ran')
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Dying), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: Console({})), \
                mock.patch.object(suite, 'snapshot', lambda usb: asyncio.sleep(0, snap('b'))), \
                mock.patch.object(suite, 'console', console), mock.patch('sys.stdout', io.StringIO()) as shown:
            asyncio.run(suite.run(suite_args(btmon=True), Path(d), result))
        self.assertEqual(result['capture'], 'failed')
        self.assertIsNone(result['failed_to_establish'])
        self.assertIn('btmon exited 0 during the run', shown.getvalue())

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

    def test_an_image_the_watch_gives_a_crc_for_is_matched_by_it(self):
        data = b'\0UNA Watche0fbf1ea9b02\0\0'
        with tempfile.NamedTemporaryFile() as f, mock.patch.object(suite.wrist_update, 'mismatch', return_value=None):
            f.write(data)
            f.flush()
            crc = f'{zlib.crc32(data):08X}'
            self.assertTrue(suite.image_problems(f.name, 'e0fbf1ea9b02', '--image'))
            self.assertFalse(suite.image_problems(f.name, 'e0fbf1ea9b02', '--image', crc))
            self.assertTrue(suite.image_problems(f.name, 'e0fbf1ea9b02', '--image', f'{zlib.crc32(data) ^ 1:08X}'))
        self.assertEqual(suite.crc_of('[usb] standalone-boot 0.3.0+ble.local build e0fbf1ea9b02 crc 6D81FE53'), '6D81FE53')
        self.assertIsNone(suite.crc_of('[usb] standalone-boot 0.3.0+ble.local build e0fbf1ea9b02 crc pending'))

    def test_a_variant_s_version_still_gives_its_build(self):
        self.assertEqual(suite.build_of('[usb] standalone-boot 0.3.0+ble.local build 4f1e9a07c3d2 crc 6D81FE53'), '4f1e9a07c3d2')

    def install_run(self, image_build, before=None):
        """run() with --install, a clean installed build and an --image of `image_build`; `before`
        puts each build ID after those bytes, and the watch then gives each image's CRC."""
        installs = []

        async def install(usb, a, path, out):
            installs.append(path)
            return True, usb

        builds = iter(['0ld0ld0ld0ld', 'c1ea2c1ea2c1', '0ld0ld0ld0ld', '0ld0ld0ld0ld'])
        files = {name: image(build, *([before] if before else [])) for name, build in
                 [('old', '0ld0ld0ld0ld'), ('new', 'c1ea2c1ea2c1'), ('image', image_build)]}
        crcs = {'0ld0ld0ld0ld': files['old'], 'c1ea2c1ea2c1': files['new']} if before else {}
        crcs = {b: f'{zlib.crc32(Path(f.name).read_bytes()):08X}' for b, f in crcs.items()}

        async def snapshot(usb):
            build = next(builds)
            return dict(snap(build), crc=crcs.get(build))

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

    def test_images_whose_id_follows_a_word_are_matched_by_crc(self):
        ok, result, installs, seen, a = self.install_run('c1ea2c1ea2c1', before=b'UNA Watch')
        self.assertTrue(ok, result.get('preflight'))
        ok, result, installs, seen, a = self.install_run('c1ea2c1ea2c1-dirty', before=b'UNA Watch')
        self.assertFalse(ok)

    def test_the_update_scenario_s_image_is_matched_by_crc(self):
        f = image('c1ea2c1ea2c1', b'UNA Watch')
        crc = f'{zlib.crc32(Path(f.name).read_bytes()):08X}'
        result = {'scenarios': []}

        async def snapshot(usb):
            return dict(snap('c1ea2c1ea2c1'), crc=crc)

        async def update(s, usb, phone, build, a):
            s.check(True, 'ran')
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: Console({})), \
                mock.patch.object(suite, 'snapshot', snapshot), mock.patch.object(suite, 'update', update), \
                mock.patch.object(suite.wrist_update, 'mismatch', return_value=None), \
                mock.patch('sys.stdout', io.StringIO()):
            ok = asyncio.run(suite.run(suite_args(scenarios=['update'], image=Path(f.name)), Path(d), result))
        f.close()
        self.assertTrue(ok, result.get('preflight'))

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
            # A refusal that says nothing, as `fuser` finding nobody does: sudo's list says no.
            with fake_sudo(d, '[ "$2" = -l ] && exit 1; exit 1\n'):
                self.assertIsNone(suite.Linux().fuser('/dev/ttyACM0'))
            with fake_sudo(d, '[ "$2" = -l ] && exit 0; exit 1\n'):
                self.assertEqual(suite.Linux().fuser('/dev/ttyACM0'), [])

    def test_macos_holders_include_the_tty_twin(self):
        asked = []

        def lsof(argv, **kw):
            asked.append(argv)
            return subprocess.CompletedProcess(argv, 0, '7\n', '')
        with mock.patch.object(suite.subprocess, 'run', lsof), mock.patch.object(suite.os.path, 'exists', return_value=True), \
                mock.patch.object(suite.Host, 'ps', lambda self: ['7 screen /dev/tty.usbmodem101']):
            self.assertEqual(suite.Host().holders('/dev/cu.usbmodem101'), ['7 screen /dev/tty.usbmodem101'])
        self.assertEqual(asked, [['lsof', '-t', '/dev/cu.usbmodem101', '/dev/tty.usbmodem101']])

    def test_a_pipeline_subshell_is_ours_and_a_twin_is_not(self):
        # The twins meet before looking and again before exiting, so each sees the other running Python.
        with tempfile.TemporaryDirectory() as d:
            probe = Path(d, 'ble-suite.py')
            probe.write_text(textwrap.dedent(f"""
                import importlib.util, os, sys, time
                s = importlib.util.spec_from_file_location('s', {str(spec.origin)!r})
                m = importlib.util.module_from_spec(s)
                s.loader.exec_module(m)
                where, n = sys.argv[1], int(sys.argv[2])

                def meet(stage):
                    open(os.path.join(where, f'{{stage}}.{{os.getpid()}}'), 'w').close()
                    while sum(f.startswith(stage + '.') for f in os.listdir(where)) < n:
                        time.sleep(0.01)
                while 'fork' in sys.argv and sum('#me{os.getpid()}' in p for p in m.Host().ps()) < 2:
                    time.sleep(0.01)
                meet('up')
                found = m.Host().other_tools()
                meet('looked')
                print('found', found)
            """))

            def inner(where, n, *wait):
                os.mkdir(where)
                return subprocess.list2cmdline([sys.executable, str(probe), where, str(n), *wait])
            piped = f'{inner(d + "/piped", 1, "fork")} | while read l; do echo "$l"; done; : ble-suite.py #me{os.getpid()}'
            out = subprocess.run(['sh', '-c', piped], capture_output=True, text=True, timeout=60).stdout
            self.assertIn('found', out)
            self.assertNotIn(f'#me{os.getpid()}', out)
            twin = inner(d + '/twins', 2)
            out = subprocess.run(['sh', '-c', f'{twin} & {twin}; wait'], capture_output=True, text=True, timeout=60).stdout
        found = [l for l in out.splitlines() if l.startswith('found')]
        self.assertEqual(len(found), 2)
        self.assertTrue(all('ble-suite.py' in l for l in found), found)

    def test_sigterm_writes_the_result_and_restores(self):
        with tempfile.TemporaryDirectory() as d:
            driver = textwrap.dedent(f"""
                import asyncio, importlib.util, sys
                from pathlib import Path
                from unittest import mock
                sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})
                spec = importlib.util.spec_from_file_location('t', {str(Path(__file__).resolve())!r})
                t = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(t)
                suite = t.suite
                builds = iter(['a1a1a1a1a1a1', 'b2b2b2b2b2b2', 'a1a1a1a1a1a1', 'a1a1a1a1a1a1'])

                async def snapshot(usb):
                    return t.snap(next(builds))

                async def install(usb, a, path, out):
                    print('installed', path, flush=True)
                    return True, usb

                async def console(s, usb, phone, build, a):
                    print('scenario started', flush=True)
                    await asyncio.sleep(60)
                old, new = t.image('a1a1a1a1a1a1'), t.image('b2b2b2b2b2b2')
                sys.argv = ['ble-suite.py', '--install', new.name, '--restore', old.name, '--scenarios', 'console',
                            '--out', {d + '/run'!r}]
                with mock.patch.object(suite, 'host', t.Quiet), \\
                        mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \\
                        mock.patch.object(suite, 'Usb', lambda port, path: t.Console({{}})), \\
                        mock.patch.object(suite, 'snapshot', snapshot), mock.patch.object(suite, 'install', install), \\
                        mock.patch.object(suite, 'console', console), \\
                        mock.patch.object(suite.wrist_update, 'mismatch', return_value=None):
                    suite.main()
                """)
            p = subprocess.Popen([sys.executable, '-c', driver], stdout=subprocess.PIPE, text=True)
            lines = []
            for line in p.stdout:
                lines.append(line)
                if 'scenario started' in line:
                    p.send_signal(signal.SIGTERM)
            p.wait(10)
            result = json.loads((Path(d) / 'run/result.json').read_text())
        self.assertNotEqual(p.returncode, 0)
        self.assertEqual(result['stopped'], 'terminated')
        self.assertTrue(result['restored'])
        self.assertFalse(result['passed'])
        self.assertEqual([(s['name'], s['passed'], [c['what'] for c in s['checks']]) for s in result['scenarios']],
                         [('console', False, ['stopped before its end', 'console: the watch raised no alarm (none)'])])
        self.assertEqual([l.split()[1] for l in lines if l.startswith('installed')], [result['argv'][1], result['argv'][3]])

    def test_an_existing_out_is_one_line(self):
        with tempfile.TemporaryDirectory() as d, mock.patch.object(sys, 'argv', ['ble-suite.py', '--out', d]):
            with self.assertRaises(SystemExit) as end:
                suite.main()
        self.assertEqual(end.exception.code, f'ble-suite.py: {d} already exists; pick a new --out')

    def test_lateness_needs_the_probe_through_the_traffic(self):
        class Phone:
            async def connect(self):
                return Link()

        class Link:
            async def command(self, text, pattern, timeout=3):
                return 'ok'

            async def close(self):
                pass

        def run_with(ran):
            page = ['[glass] longest compose 1.0 ms, render 1.0 ms; executor late by at most 1.2 ms at 5 ms, '
                    'over 5 ms 0 times (maxima since the last `page`)',
                    f'[glass] latency probe ran {ran} s since the last `page`; off']
            usb = Console({'page': page, 'page lag 8': ['[glass] executor-latency probe armed for 8 s']})
            s = suite.Scenario('lateness', '')
            with mock.patch('sys.stdout', io.StringIO()):
                asyncio.run(suite.lateness(s, usb, Phone(), 'b', argparse.Namespace(lag_seconds=8)))
            return s
        self.assertTrue(run_with(2).passed)
        self.assertEqual(run_with(2).checks[0]['what'], 'attached over the air for traffic')
        expired = run_with(0)
        self.assertFalse(expired.passed)
        self.assertIn('the probe ran through the traffic (0 s of 2 s', ' '.join(c['what'] for c in expired.checks))

    def test_closing_the_console_mid_open_leaves_no_port_open(self):
        opened = []

        class Port:
            def __init__(self, *a, **kw):
                self.closed = False
                opened.append(self)

            def read(self, n):
                time.sleep(0.01)
                return b''

            def close(self):
                self.closed = True

        def settle(s):
            time.sleep(2.5)
            return s
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite.serial, 'Serial', Port), mock.patch.object(suite.wrist_port, 'settle', settle):
            usb = suite.Usb(None, Path(d) / 'console.log')
            time.sleep(0.2)
            usb.close()
            self.assertFalse(usb.thread.is_alive())
        self.assertEqual([p.closed for p in opened], [True])

    def test_commands_are_logged_as_the_replay_reads_them(self):
        class Port:
            def read(self, n):
                time.sleep(0.01)
                return b''

            def close(self):
                pass
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite.serial, 'Serial', lambda *a, **kw: Port()), \
                mock.patch.object(suite.wrist_port, 'settle', lambda s: s), \
                mock.patch.object(suite.wrist_port, 'write_command'):
            usb = suite.Usb(None, Path(d) / 'console.log')
            usb.send('ble profile battery')
            usb.close()
            logged = (Path(d) / 'console.log').read_text().splitlines()
        self.assertIn('>> usb ble profile battery', [line.split(None, 1)[1] for line in logged], logged)

    def test_a_link_lost_mid_command_is_an_unanswered_cycle(self):
        class Lost:
            async def command(self, text, pattern, timeout=3):
                raise RuntimeError('Service Discovery has not been performed yet')
        with mock.patch('builtins.print'):
            self.assertFalse(asyncio.run(suite.traffic(Lost(), 30)))

    def test_no_adapter_is_a_failed_connect(self):
        async def gone(name):
            raise RuntimeError('No powered Bluetooth adapters found')
        with mock.patch.object(suite.wrist_ble, 'find', gone), mock.patch('builtins.print'):
            self.assertIsNone(asyncio.run(suite.BleakPhone('UNA').connect()))


class Alarms(unittest.TestCase):
    def test_an_alarm_is_its_scenario_s_up_to_the_next_one(self):
        lines = [(1.0, '[ble] alarm: reachable, no phone can find the watch'), (5.0, '[ble] connected'),
                 (7.0, '[ble] alarm: bounded-effort, too many operations in a minute'), (9.0, '>> usb ble alarm: no')]
        self.assertEqual(suite.alarms(lines, 2.0, 7.0), [])
        self.assertEqual(suite.alarms(lines, 2.0, None), [lines[2][1]])
        self.assertEqual(len(suite.alarms(lines, 0.0, None)), 2)

    def test_a_scenario_during_which_the_watch_raised_an_alarm_fails_and_the_others_pass(self):
        async def console(s, usb, phone, build, a):
            s.check(True, 'sessions answered')

        async def reconnect(s, usb, phone, build, a):
            usb.lines.append((time.time(), '[ble] alarm: bounded-effort, too many operations in a minute'))
            s.check(True, 'reconnects answered')
        result = {'scenarios': []}
        usb = Console({})
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: usb), \
                mock.patch.object(suite, 'snapshot', lambda usb: asyncio.sleep(0, snap('b'))), \
                mock.patch.object(suite, 'console', console), mock.patch.object(suite, 'reconnect', reconnect), \
                mock.patch('sys.stdout', io.StringIO()) as shown:
            ok = asyncio.run(suite.run(suite_args(scenarios=['console', 'reconnect', 'console'], reconnects=1, encrypt_limit=2.0), Path(d), result))
        self.assertFalse(ok)
        self.assertEqual([s['passed'] for s in result['scenarios']], [True, False, True])
        self.assertIn('[FAIL] reconnect: the watch raised no alarm ([ble] alarm: bounded-effort', shown.getvalue())
        self.assertIn('no `[ble] alarm:`', result['scenarios'][0]['criteria'])

    def test_over_the_air_the_last_lines_are_read_before_judging(self):
        class Air(suite.Air):
            def __init__(self):
                self.lines, self.settled = [], 0
                self.link, self.phone = 'held', self

            async def connect(self):
                return 'own'

            async def sync(self):
                self.settled += 1
                self.lines.append((time.time(), '[ble] alarm: reachable, no phone can find the watch'))
                return True

            async def detach(self):
                self.link = None
        s = suite.Scenario('soak', '')
        s.check(True, 'cycles answered')
        air = Air()
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(suite.judge_alarms(air, [s], 0))
        self.assertEqual(air.settled, 1)
        self.assertIsNone(air.link)
        self.assertFalse(s.passed)

    def test_over_the_air_a_last_read_that_found_no_link_fails_the_last_scenario(self):
        class Phone:
            async def connect(self):
                return None
        air = suite.Air(Phone(), os.devnull)
        first, last = suite.Scenario('console', ''), suite.Scenario('soak', '')
        first.check(True, 'ran')
        last.check(True, 'ran')
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(suite.judge_alarms(air, [first, last], 0))
        self.assertTrue(first.passed)
        self.assertFalse(last.passed)
        self.assertIn('last lines were read', last.checks[-1]['what'])

    def test_an_alarm_told_after_the_last_scenario_is_waited_for(self):
        usb = Console({})
        s = suite.Scenario('update', '')
        s.check(True, 'ran')

        async def judged():
            async def late():
                await asyncio.sleep(0.2)
                usb.lines.append((time.time(), '[ble] alarm: reachable, no phone can find the watch'))
            asyncio.ensure_future(late())
            await suite.judge_alarms(usb, [s], 0.5)
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(judged())
        self.assertFalse(s.passed)

    def test_a_stop_while_judging_keeps_the_scenarios_run(self):
        async def console(s, usb, phone, build, a):
            s.check(True, 'sessions answered')

        async def stopped(usb, ran, grace):
            raise asyncio.CancelledError()
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: Console({})), \
                mock.patch.object(suite, 'snapshot', lambda usb: asyncio.sleep(0, snap('b'))), \
                mock.patch.object(suite, 'console', console), mock.patch.object(suite, 'judge_alarms', stopped), \
                mock.patch('sys.stdout', io.StringIO()):
            with self.assertRaises(asyncio.CancelledError):
                asyncio.run(suite.run(suite_args(), Path(d), result))
        self.assertEqual([(r['name'], r['passed']) for r in result['scenarios']], [('console', True)])

    def test_a_scenario_with_no_checks_gains_none(self):
        s = suite.Scenario('console', '')
        with mock.patch('sys.stdout', io.StringIO()):
            asyncio.run(suite.judge_alarms(Console({}), [s], 0))
        self.assertFalse(s.passed)
        self.assertEqual(s.checks, [])


class Watch:
    """A watch reached over the air alone: what it logs on its uptime, and its answers to `log since`."""

    def __init__(self, reasons=()):
        self.t0, self.log, self.reasons, self.sent, self.lost, self.dropped = time.time() - 100, [], list(reasons), [], 0, 0

    def say(self, text):
        time.sleep(0.005)
        self.log.append((time.time() - self.t0, text))

    async def send(self, client, text):
        self.sent.append(text)
        m = re.match(r'log since (\d+)$', text)
        if not m:
            return self.say(f'[usb] {text} answered')
        n, lines = int(m[1]), client.link.con.lines
        restarted = n > len(self.log)
        lines.append(f'[log] since {n}' + (': past the end, so from the start (the watch restarted)' if restarted else ''))
        for t, line in self.log[0 if restarted else n:]:
            lines += [f'{t:.3f} {line}', '[hb] a live line between kept ones']
        lines.append(f'[log] next {len(self.log)} at {time.time() - self.t0:.3f}' + (f', {self.lost} bytes lost' if self.lost else '')
                     + (f', {self.dropped} lines dropped on the way' if self.dropped else ''))


class WatchLink:
    def __init__(self, watch):
        self.watch = watch
        self.client = types.SimpleNamespace(is_connected=True, link=self)
        self.con = types.SimpleNamespace(lines=[])

    async def command(self, text, pattern, timeout=3):
        return 'ok'

    async def close(self):
        if self.client.is_connected:
            self.client.is_connected = False
            reason = self.watch.reasons.pop(0) if self.watch.reasons else '13'
            if reason:
                self.watch.say(f'[ble] disconnected, reason {reason}')


class WatchPhone:
    def __init__(self, watch):
        self.watch, self.links = watch, 0

    async def connect(self):
        self.links += 1
        self.watch.say('[ble] connected')
        self.watch.say('[ble] link encrypted')
        return WatchLink(self.watch)


class CableHangUps(unittest.TestCase):
    def test_a_session_whose_hang_up_never_came_fails_on_the_cable(self):
        lines = []

        class Con:
            lines = property(lambda self: lines)

            def mark(self):
                return len(lines)

            def since(self, mark):
                return lines[mark:]

            wait = suite.Usb.wait

        class SessionLink(Link):
            def __init__(self, silent):
                super().__init__()
                self.silent = silent

            async def close(self):
                if not self.silent:
                    lines.append((time.time(), '[ble] disconnected, reason 13'))

        class Sessions:
            n = 0

            async def connect(self):
                self.n += 1
                lines.append((time.time(), '[ble] connected'))
                return SessionLink(self.n == 1)

        s = suite.Scenario('console', '')
        with mock.patch.object(suite.hang_up, '__defaults__', (0.1,)), mock.patch.object(SessionLink, 'command',
                                                                                         lambda self, text, pattern, timeout=3: asyncio.sleep(0, 'build b ok')):
            asyncio.run(suite.console(s, Con(), Sessions(), 'b', argparse.Namespace(sessions=2)))
        self.assertEqual([c['passed'] for c in s.checks if 'hang-up' in c['what']], [False, True])


class AirAlone(unittest.TestCase):
    """--air: the watch's lines from its log, read over each link."""

    def air(self, watch, d):
        phone = WatchPhone(watch)
        return suite.Air(phone, Path(d) / 'console.log'), phone

    def test_a_log_answer_is_whole_or_nothing(self):
        lines = ['[log] since 7', '12.500 [ble] connected', '[hb] 3', '13.000 [pwr] 3900 mV']
        self.assertIsNone(suite.read_log(lines))
        got = suite.read_log(lines + ['[log] next 90 at 14.250, 6 bytes lost'])
        self.assertEqual(got, ([(12.5, '[ble] connected'), (13.0, '[pwr] 3900 mV')], 90, 14.25, 6, 0, False))
        again = suite.read_log(['[log] since 500: past the end, so from the start (the watch restarted)', '[log] next 3 at 1.000'])
        self.assertTrue(again[5])
        self.assertEqual([suite.quiet(l) for l in lines + ['[usb] pong']], [True, True, True, True, False])

    def test_the_log_resumes_where_it_ended_and_a_restart_is_seen(self):
        watch = Watch()
        watch.say('[boot] marked booted')
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send), \
                mock.patch('sys.stdout', io.StringIO()):
            air, _ = self.air(watch, d)
            asyncio.run(air.attach())
            self.assertEqual([l for _, l in air.lines], ['[boot] marked booted', '[ble] connected', '[ble] link encrypted'])
            watch.say('[pwr] 3900 mV')
            asyncio.run(air.sync())
            self.assertEqual([l for _, l in air.lines][3:], ['[pwr] 3900 mV'])
            self.assertLess(abs(air.lines[-1][0] - time.time()), 0.5)
            watch.log = []
            watch.say('[boot] after the reset')
            asyncio.run(air.sync())
            self.assertEqual(air.restarts, 1)
            self.assertEqual(air.lines[-1][1], '[boot] after the reset')

    def test_a_restart_that_has_said_more_than_the_last_read_is_seen(self):
        watch = Watch()
        watch.say('[boot] marked booted')
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send), \
                mock.patch('sys.stdout', io.StringIO()):
            air, _ = self.air(watch, d)
            asyncio.run(air.attach())
            watch.t0, watch.log = time.time(), []
            for line in ('[crash] the last reset', '[boot] after the reset', '[hb] 1', '[hb] 2', '[hb] 3'):
                watch.say(line)
            asyncio.run(air.sync())
            self.assertEqual(air.restarts, 1)
            self.assertEqual([l for _, l in air.lines][3:], ['[crash] the last reset', '[boot] after the reset', '[hb] 1', '[hb] 2', '[hb] 3'])
            self.assertLess(abs(air.lines[-1][0] - time.time()), 0.5)

    def test_only_what_the_run_could_have_read_counts_as_lost(self):
        watch = Watch()
        watch.lost = 40
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send):
            air, _ = self.air(watch, d)
            asyncio.run(air.attach())
            self.assertEqual(air.lost, 0)
            asyncio.run(air.sync())
            self.assertEqual(air.lost, 40)

    def test_lines_the_mirror_dropped_count_from_the_first_read(self):
        self.assertEqual(suite.read_log(['[log] since 0', '[log] next 9 at 1.000, 2 lines dropped on the way'])[4], 2)
        watch = Watch()
        watch.dropped = 3
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send):
            air, _ = self.air(watch, d)
            asyncio.run(air.attach())
            self.assertEqual(air.dropped, 3)
            asyncio.run(air.sync())
            self.assertEqual(air.dropped, 6)

    def test_an_ask_is_answered_from_the_log(self):
        watch = Watch()
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send):
            air, phone = self.air(watch, d)
            self.assertEqual(asyncio.run(air.ask('uptime', r'uptime answered')), '[usb] uptime answered')
            self.assertEqual(asyncio.run(air.ask('ping', r'ping answered')), '[usb] ping answered')
            self.assertEqual(phone.links, 1)
            self.assertIn('>> air uptime', (Path(d) / 'console.log').read_text())

    def test_traffic_reads_the_log_before_its_answers_outrun_it(self):
        watch = Watch()
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send), \
                mock.patch.object(suite, 'SYNC_EVERY', 0):
            air, _ = self.air(watch, d)
            link = asyncio.run(air.attach())
            before = sum(t.startswith('log since') for t in watch.sent)
            self.assertTrue(asyncio.run(suite.traffic(link, 0, air)))
            self.assertEqual(sum(t.startswith('log since') for t in watch.sent), before + 1)
            self.assertTrue(asyncio.run(suite.traffic(link, 0)))
            self.assertEqual(sum(t.startswith('log since') for t in watch.sent), before + 1)

    def test_each_hang_up_is_judged_from_the_next_link(self):
        watch = Watch(reasons=['13', '08'])
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send):
            air, phone = self.air(watch, d)
            s = suite.Scenario('console', '')
            with mock.patch('sys.stdout', io.StringIO()):
                asyncio.run(suite.console(s, air, suite.AirPhone(air, phone), 'b', argparse.Namespace(sessions=2)))
        said = [c for c in s.checks if 'hang-up' in c['what']]
        self.assertEqual([(c['passed'], c['what'][-11:]) for c in said], [(True, '(reason 13)'), (False, '(reason 08)')])
        self.assertEqual(phone.links, 3)

    def test_a_hang_up_the_watch_never_said_is_not_the_next_one(self):
        watch = Watch(reasons=[None, '13'])
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send):
            air, phone = self.air(watch, d)
            s = suite.Scenario('console', '')
            with mock.patch('sys.stdout', io.StringIO()):
                asyncio.run(suite.console(s, air, suite.AirPhone(air, phone), 'b', argparse.Namespace(sessions=2)))
        self.assertEqual([c['passed'] for c in s.checks if 'hang-up' in c['what']], [False, True])

    def test_a_revert_is_read_before_it_is_judged(self):
        watch = Watch()
        watch.say('[boot] marked booted')

        class Updating(WatchPhone):
            async def update(self, image):
                watch.t0, watch.log = time.time(), []
                watch.say('[boot] state revert (the previous update failed to mark itself booted)')
                return True

        async def send(client, text):
            if text == 'version':
                return watch.say('[usb] standalone-boot 0.1 build b crc 00000000')
            return await watch.send(client, text)

        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', send), \
                mock.patch.object(suite, 'radio_back', lambda usb, attempts: asyncio.sleep(0, True)), \
                mock.patch('sys.stdout', io.StringIO()):
            phone = Updating(watch)
            air = suite.Air(phone, Path(d) / 'console.log')
            asyncio.run(air.attach())
            s = suite.Scenario('update', '')
            asyncio.run(suite.update(s, air, suite.AirPhone(air, phone), 'b', argparse.Namespace(update_image='app.bin', profile=None)))
        self.assertFalse(next(c['passed'] for c in s.checks if 'reverted' in c['what']))
        self.assertTrue(next(c['passed'] for c in s.checks if 'runs build' in c['what']))

    def test_no_revert_is_judged_from_nothing_read(self):
        watch = Watch()

        class Unreachable(WatchPhone):
            async def update(self, image):
                return True

            async def connect(self):
                return None

        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite.wrist_ble, 'send', watch.send), \
                mock.patch.object(suite, 'radio_back', lambda usb, attempts: asyncio.sleep(0, True)), \
                mock.patch('sys.stdout', io.StringIO()):
            phone = Unreachable(watch)
            air = suite.Air(phone, Path(d) / 'console.log')
            s = suite.Scenario('update', '')
            asyncio.run(suite.update(s, air, suite.AirPhone(air, phone), 'b', argparse.Namespace(update_image='app.bin', profile=None)))
        self.assertFalse(next(c['passed'] for c in s.checks if 'reverted' in c['what']))

    def test_the_gauge_s_figures_and_what_they_cannot_show(self):
        lines = [(0, '[pwr] 3900 mV  soc 80.0%  25 C  now -1000 uA  avg -1200 uA  rails all good'),
                 (60, '[gauge]  RepCap 0C80 100.00 mAh  FullCapNom 0C80 1600.00 mAh'),
                 (1800, '[ble] connected'),
                 (3600, '[pwr] 3880 mV  soc 79.5%  25 C  now -500 uA  avg -800 uA  rails all good'),
                 (3660, '[gauge]  RepCap 0C7F 99.50 mAh  FullCapNom 0C80 1600.00 mAh')]
        figures = suite.gauge(lines)
        self.assertEqual((figures['readings'], figures['mean_avg_current_ua'], figures['soc_drop']), (2, -1000, 0.5))
        self.assertEqual((figures['rep_cap_drop_mah'], figures['rep_cap_drop_ua']), (0.5, 500))
        self.assertIn('156.25 uA', suite.said(figures))
        self.assertIsNone(suite.gauge(lines[2:3]))


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), Path(__file__).with_name(f'{name}.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Replayable(unittest.TestCase):
    """--restart-radio, --profile and console.log's times, the form the replay reads."""

    class Said:
        def __init__(self, answers):
            self.answers, self.sent = answers, []

        async def ask(self, text, pattern, timeout=3):
            self.sent.append(text)
            return next((l for l in self.answers.get(text, []) if re.search(pattern, l)), None)

    def prepare(self, usb, **flags):
        a = argparse.Namespace(restart_radio=False, profile=None)
        vars(a).update(flags)
        result = {}
        with mock.patch.object(suite.asyncio, 'sleep', mock.AsyncMock()):
            problems = asyncio.run(suite.prepare(usb, a, result))
        return problems, result

    def test_nothing_asked_without_the_flags(self):
        usb = self.Said({})
        self.assertEqual(self.prepare(usb), ([], {}))
        self.assertEqual(usb.sent, [])

    def test_the_radio_restarted_then_the_profile_held(self):
        usb = self.Said({'ble off': ['[ble] off'], 'ble on': ['[ble] on'],
                         'ble': ['[ble] advertising as UNA, timing cable, address C2:3D:4E:5F:60:71'],
                         'ble profile battery': ['[ble] timing: the battery\'s, whatever the power source']})
        problems, result = self.prepare(usb, restart_radio=True, profile='battery')
        self.assertEqual(problems, [])
        self.assertEqual(usb.sent, ['ble off', 'ble on', 'ble', 'ble profile battery'])
        self.assertEqual(result['profile'], 'battery')
        self.assertNotIn('radio_off', result)

    def test_a_radio_that_stays_off_stops_the_run(self):
        usb = self.Said({'ble': ['[ble] off, timing cable']})
        problems, _ = self.prepare(usb, restart_radio=True)
        self.assertEqual(len(problems), 1)
        self.assertIn('did not come back', problems[0])

    def test_the_profile_is_held_again_after_the_update_s_reset(self):
        usb = Console({'version': ['[usb] build b1'], 'ble': ['[ble] advertising as UNA, timing cable'],
                       'ble profile battery': ['[ble] timing: the battery\'s, whatever the power source']})
        self.assertTrue(run(suite.update, usb, Phone(), 'b1', update_image=None, profile='battery'))
        self.assertEqual(usb.sent[-1], 'ble profile battery')

    def test_a_run_stopped_with_the_radio_off_turns_it_on(self):
        usb = Console({'ble off': ['[ble] off'], 'ble on': ['[ble] on']})

        async def stopped(seconds):
            raise asyncio.CancelledError
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: usb), \
                mock.patch.object(suite, 'snapshot', mock.AsyncMock(return_value=snap('b1'))), \
                mock.patch.object(suite.asyncio, 'sleep', stopped), mock.patch('sys.stdout', io.StringIO()):
            with self.assertRaises(asyncio.CancelledError):
                asyncio.run(suite.run(suite_args(restart_radio=True), Path(d), result))
        self.assertEqual(usb.sent, ['ble off', 'ble on'])

    def test_a_console_gone_at_the_end_still_restores(self):
        class Gone(Console):
            def send(self, text):
                if text == 'ble on':
                    raise RuntimeError("the watch's USB console is gone")
                super().send(text)
        usb, installs = Gone({'ble off': ['[ble] off']}), []

        async def install(u, a, image, out):
            installs.append(image)
            return True, usb
        result = {'scenarios': []}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(suite, 'host', Quiet), \
                mock.patch.object(suite.wrist_port, 'find', return_value='/dev/watch'), \
                mock.patch.object(suite, 'Usb', lambda port, path: usb), \
                mock.patch.object(suite, 'snapshot', mock.AsyncMock(return_value=snap('b1'))), \
                mock.patch.object(suite, 'image_problems', lambda *a: []), mock.patch.object(suite, 'install', install), \
                mock.patch.object(suite.asyncio, 'sleep', mock.AsyncMock()), mock.patch('sys.stdout', io.StringIO()):
            with self.assertRaises(RuntimeError):
                asyncio.run(suite.run(suite_args(restart_radio=True, install=Path('new.bin'), restore=Path('old.bin')),
                                      Path(d), result))
        self.assertEqual(installs, [Path('new.bin'), Path('old.bin')])

    def test_console_times_are_seconds_since_the_start(self):
        with mock.patch.object(suite, 'START', 1000.0):
            self.assertEqual(suite.seconds(1012.3456), '   12.346')
            self.assertRegex(suite.seconds(1000.0) + ' [trace] 5 e 04', r'^\s*\d+\.\d{3} ')


FAKE_STRAP = textwrap.dedent('''
    import signal, sys, time
    print('-- advertising HR-SIM (0x180D); Ctrl-C stops', flush=True)
    if 'exit' in sys.argv:
        sys.exit(1)
    try:
        while True:
            time.sleep(0.05)
    except KeyboardInterrupt:
        print('-- 12:00:03 stopped after 3 notifications', flush=True)
    ''')


class Straps(unittest.TestCase):
    def test_a_scan_s_lines_give_the_advertisers_heard_with_the_name_and_the_service(self):
        lines = ['[ble] seen 11:22:33:44:55:66 -60 dBm 180D', '[ble] seen 11:22:33:44:55:66 -58 dBm (scan response) "HR-SIM"',
                 '[ble] seen AA:BB:CC:DD:EE:FF -70 dBm "HR-SIM"', '[ble] seen 01:02:03:04:05:06 -40 dBm "Polar H10" 180D 180F',
                 '[ble] seen 0A:0B:0C:0D:0E:0F -41 dBm "HR-SIM" 180F 0000180D-0000-1000-8000-00805F9B34FB',
                 '>> usb ble scan 5']
        self.assertEqual(suite.heard(lines), {'11:22:33:44:55:66': -60})
        self.assertEqual(suite.heard(lines, 'Polar H10'), {'01:02:03:04:05:06': -40})
        self.assertEqual(suite.heard(lines[2:3]), {})

    def test_a_strap_is_either_simulator(self):
        self.assertEqual(suite.Strap('hci1').argv()[1:], [str(suite.ROOT / 'tools/hr-sim.py'), '--adapter', 'hci1'])
        self.assertEqual(suite.Strap('bumble:usb:0', ['--att-delay', '3']).argv()[1:],
                         [str(suite.ROOT / 'tools/hr-sim-bumble.py'), 'usb:0', '--att-delay', '3'])

    def test_a_strap_starts_once_it_advertises_and_stops_as_ctrl_c_does(self):
        with tempfile.TemporaryDirectory() as d:
            strap = suite.Strap('hci0', log=Path(d, 'strap.log'))
            with mock.patch.object(strap, 'argv', lambda: [sys.executable, '-c', FAKE_STRAP]):
                self.assertTrue(asyncio.run(strap.start()))
                self.assertEqual(asyncio.run(strap.stop()), 3)
            self.assertEqual([w for _, w in strap.said('')], ['advertising HR-SIM (0x180D); Ctrl-C stops',
                                                              'stopped after 3 notifications'])
            self.assertIn('stopped after 3', Path(d, 'strap.log').read_text())
        strap = suite.Strap('hci0')
        with mock.patch.object(strap, 'argv', lambda: [sys.executable, '-c', 'print("!! no adapter hci0")']):
            self.assertFalse(asyncio.run(strap.start()))
        self.assertEqual(strap.last(), '!! no adapter hci0')
        self.assertIsNone(asyncio.run(suite.Strap('hci0').stop()))

    def test_the_scan_scenario_hears_the_strap_and_the_radio_back(self):
        found = ['[ble] scanning for 5 s, listening 50% of the time (50 ms in every 100 ms)',
                 '[ble] seen 11:22:33:44:55:66 -60 dBm "HR-SIM" 180D', '[ble] scan done: 9 reports, 1 advertisers printed']
        usb = Console({'ble scan 5': found, 'ble': ['[ble] advertising as UNA, timing cable']})

        class Strap(suite.Strap):
            async def start(self):
                self.lines.append((time.time(), '-- advertising HR-SIM (0x180D); Ctrl-C stops'))
                return True

            async def stop(self):
                return 0
        with mock.patch.object(suite, 'Strap', Strap):
            self.assertTrue(run(suite.scan, usb, None, 'b', strap='hci0', strap_flags=[], strap_log=None, scan_seconds=5))
            usb = Console({'ble scan 5': found[:1] + found[2:], 'ble': ['[ble] advertising as UNA, timing cable']})
            self.assertFalse(run(suite.scan, usb, None, 'b', strap='hci0', strap_flags=[], strap_log=None, scan_seconds=5))

    def test_the_phone_and_its_update_use_the_adapter_named(self):
        asked = []

        async def find(name, timeout=15, adapter=None):
            asked.append(('find', adapter))
            return 'device'

        class Client:
            def __init__(self, device, timeout, **kw):
                asked.append(('client', kw))

            async def connect(self):
                raise RuntimeError('no link')

            async def disconnect(self):
                pass

        async def update(name, update=None, adapter=None):
            asked.append(('update', adapter))
            return 0
        with mock.patch.object(suite.wrist_ble, 'find', find), mock.patch.object(suite.wrist_ble, 'BleakClient', Client), \
                mock.patch.object(suite.wrist_ble, 'run', update), mock.patch('sys.stdout', io.StringIO()):
            phone = suite.BleakPhone('UNA', 'hci1')
            self.assertIsNone(asyncio.run(phone.connect()))
            self.assertTrue(asyncio.run(phone.update('app.bin')))
            asyncio.run(suite.BleakPhone('UNA').connect())
        self.assertEqual(asked, [('find', 'hci1'), ('client', {'adapter': 'hci1'}), ('update', 'hci1'), ('find', None), ('client', {})])

    def test_strap_flags_need_bumble_and_the_scan_is_not_run_by_default(self):
        with mock.patch.object(sys, 'argv', ['ble-suite.py', '--strap-flags', '--att-delay 3']), \
                mock.patch('sys.stderr', io.StringIO()) as err:
            with self.assertRaises(SystemExit):
                suite.main()
        self.assertIn('--strap bumble:TRANSPORT', err.getvalue())
        self.assertNotIn('scan', suite.PHONE_SCENARIOS)


@unittest.skipUnless(importlib.util.find_spec('dbus_fast'), 'hr-sim.py needs dbus-fast (Linux)')
class Consume(unittest.TestCase):
    def test_a_connect_still_in_flight_is_cancelled_at_exit(self):
        hr_sim = load('hr-sim')
        Variant = hr_sim.Variant
        dev_path = '/org/bluez/hci0/dev_02_80_E1_00_00_01'
        calls = []

        class Bus:
            async def introspect(self, name, path):
                return None

            def get_proxy_object(self, name, path, intro):
                return self

            def get_interface(self, name):
                return self

            async def call_get_managed_objects(self):
                return {dev_path: {'org.bluez.Device1': {'Connected': Variant('b', False),
                                                         'ServicesResolved': Variant('b', False)}}}

            async def call_connect(self):
                calls.append('connect')
                await asyncio.Event().wait()

            async def call_disconnect(self):
                calls.append('disconnect')

        async def go():
            link, stop = {}, asyncio.Event()
            task = asyncio.create_task(hr_sim.consume(Bus(), 'hci0', '02:80:E1:00:00:01', True, stop, link))
            while 'connect' not in calls:
                await asyncio.sleep(0)
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            self.assertTrue(link['connecting'])
            self.assertFalse(link.get('opened'))
            await hr_sim.disconnect(Bus(), 'hci0', '02:80:E1:00:00:01', 'the connect still in flight')
        with mock.patch('sys.stdout', io.StringIO()) as out:
            asyncio.run(go())
        self.assertEqual(calls, ['connect', 'disconnect'])
        self.assertIn('consume: connecting to the watch', out.getvalue())
        self.assertIn('disconnected the connect still in flight', out.getvalue())

    def test_each_state_once_a_loud_subscribe_and_the_link_closed(self):
        hr_sim = load('hr-sim')
        Variant = hr_sim.Variant
        dev_path = '/org/bluez/hci0/dev_02_80_E1_00_00_01'
        hrm = {f'{dev_path}/service1/char2': {'org.bluez.GattCharacteristic1': {'UUID': Variant('s', hr_sim.HRM)}}}

        def device(connected, resolved=True, chars=False):
            found = {dev_path: {'org.bluez.Device1': {'Connected': Variant('b', connected),
                                                      'ServicesResolved': Variant('b', resolved)}}}
            return {**found, **(hrm if chars else {})}
        states = iter([{}, device(False), device(False), device(True, chars=False), device(True, chars=False),
                       device(True, chars=True), device(True, chars=True), device(True, chars=True), device(False)])
        calls = []
        stop = asyncio.Event()

        class Bus:
            async def introspect(self, name, path):
                return None

            def get_proxy_object(self, name, path, intro):
                return self

            def get_interface(self, name):
                return self

            async def call_get_managed_objects(self):
                state = next(states, None)
                if state is None:
                    stop.set()
                    return device(False)
                return state

            async def call_connect(self):
                calls.append('connect')
                if calls.count('connect') < 3:
                    raise RuntimeError('le-connection-abort-by-local')

            async def call_disconnect(self):
                calls.append('disconnect')

            async def call_start_notify(self):
                calls.append('start_notify')
                if calls.count('start_notify') == 1:
                    raise RuntimeError('org.bluez.Error.NotPermitted')

            def on_properties_changed(self, f):
                pass

            def off_properties_changed(self, f):
                pass
        sleep = asyncio.sleep
        link = {}

        async def go():
            with mock.patch.object(hr_sim.asyncio, 'sleep', lambda s: sleep(0)):
                await hr_sim.consume(Bus(), 'hci0', '02:80:E1:00:00:01', True, stop, link)
            await hr_sim.disconnect(Bus(), 'hci0', '02:80:E1:00:00:01')
        with mock.patch('sys.stdout', io.StringIO()) as out, mock.patch('sys.stderr', io.StringIO()) as err:
            asyncio.run(go())
        said = [re.sub(r'^-- \d\d:\d\d:\d\d consume: ', '', l) for l in out.getvalue().splitlines()]
        self.assertEqual(said, [
            "waiting for the watch to connect (BlueZ hasn't seen it)",
            "waiting for the watch to connect (it isn't connected)",
            'connecting to the watch',
            'connect failed: le-connection-abort-by-local',
            'the watch is connected but shows no Heart Rate Measurement',
            f"subscribed to the watch's heart rate ({dev_path}/service1/char2)",
            "the watch's link is down",
            "waiting for the watch to connect (it isn't connected)",
            'connecting to the watch',
            'disconnected the link it opened'])
        self.assertIn("FAILED to subscribe to the watch's heart rate", err.getvalue())
        self.assertTrue(link['opened'])
        self.assertEqual(calls[-1], 'disconnect')


if __name__ == '__main__':
    unittest.main()
