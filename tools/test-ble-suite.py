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
import subprocess
import sys
import tempfile
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


if __name__ == '__main__':
    unittest.main()
