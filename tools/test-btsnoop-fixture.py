#!/usr/bin/env python3
"""Tests for btsnoop-fixture.py: python3 tools/test-btsnoop-fixture.py"""
import importlib.util
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('btsnoop_fixture', Path(__file__).parent / 'btsnoop-fixture.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

CONNECTION = bytes.fromhex('3e130100010e0001aabbccddeeff18000000f40100')


class Scrub(unittest.TestCase):
    def test_an_event_cut_short_is_not_kept(self):
        for cut in [CONNECTION[:-1], CONNECTION[:12], CONNECTION[:6]]:
            self.assertIsNone(fixture.scrub(cut), cut.hex())

    def test_an_event_longer_than_its_kind_is_not_kept(self):
        self.assertIsNone(fixture.scrub(bytes.fromhex('050a00010e16aabbccddeeff')))

    def test_every_kind_kept_passes_at_its_length(self):
        kinds = ['0504', '0804', '3003', '3e1301', '3e0a03', '3e0c04', '3e0b06', '3e0b07', '3e1f0a', '3e060c', '3e0414', '3e2229']
        for kind in kinds:
            event = bytes.fromhex(kind)
            event += bytes(event[1] + 2 - len(event))
            self.assertEqual(fixture.scrub(event), event, event.hex())

    def test_an_le_event_without_a_subevent_is_not_kept(self):
        self.assertIsNone(fixture.scrub(bytes.fromhex('3e00')))


class Capture(unittest.TestCase):
    def test_a_record_cut_short_is_dropped_and_counted(self):
        records = [bytes([4]) + CONNECTION, bytes([4])]
        data = b'btsnoop\0' + struct.pack('>II', 1, 1002) + b''.join(
            struct.pack('>IIIIq', len(r), len(r), 1, 0, 0) + r for r in records)
        with tempfile.NamedTemporaryFile(suffix='.btsnoop') as f:
            f.write(data)
            f.flush()
            run = subprocess.run([sys.executable, Path(__file__).parent / 'btsnoop-fixture.py', f.name],
                                 capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(run.stdout, '0 3e130100010e000100000000000018000000f40100\n')
        self.assertIn('dropped 1 event(s) torn', run.stderr)


if __name__ == '__main__':
    unittest.main()
