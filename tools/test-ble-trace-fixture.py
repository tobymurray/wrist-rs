#!/usr/bin/env python3
"""Tests for ble-trace-fixture.py: python3 tools/test-ble-trace-fixture.py"""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('ble_trace_fixture', Path(__file__).parent / 'ble-trace-fixture.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class Scrub(unittest.TestCase):
    def test_an_address_in_a_mark_is_replaced(self):
        for written in ['C2:3D:4E:5F:60:71', 'c2-3d-4e-5f-60-71', 'C23D4E5F6071', 'C2_3D_4E_5F_60_71']:
            kept = fixture.scrub(f'12.345 == the phone {written} connects')
            self.assertEqual(kept, '12.345 == the phone <address> connects', written)

    def test_a_mark_without_an_address_is_kept_as_written(self):
        mark = '12.345 == P1 try 2: phone B connect at 235.269, reason 13, 0x3E'
        self.assertEqual(fixture.scrub(mark), mark)

    def test_twelve_hex_digits_are_taken_for_an_address(self):
        # A build id can't be told from an address written bare.
        self.assertEqual(fixture.scrub('1.000 == build 77df27bcf274'), '1.000 == build <address>')

    def test_the_operators_disconnect_is_kept(self):
        self.assertEqual(fixture.scrub('  85.804 >> usb ble disconnect'), '85.804 >> usb ble disconnect')
        self.assertEqual(fixture.scrub('85.804 >> usb ble profile auto'), '85.804 >> usb ble profile auto')
        self.assertIsNone(fixture.scrub('85.804 >> usb ble bonds'))
        self.assertIsNone(fixture.scrub('85.804 >> usb ble forget C2:3D:4E:5F:60:71'))

    def test_wall_clock_times_are_read_as_seconds_are(self):
        self.assertEqual(fixture.scrub('09:27:15.813 >> usb ble off'), '09:27:15.813 >> usb ble off')
        self.assertEqual(fixture.scrub('09:27:15.870 [trace] 946160 r'), '09:27:15.870 [trace] 946160 r')
        self.assertIsNone(fixture.scrub('9:27:15.813 >> usb ble off'))


if __name__ == '__main__':
    unittest.main()
