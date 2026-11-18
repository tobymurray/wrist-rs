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

    def test_a_transfers_bounds_are_kept(self):
        for line in ['2524.936 [update] send 345624 bytes', '2529.546 [update] received 16492',
                     '2625.693 [update] ok 345624 bytes, crc 0xFE7A7220, swap marked; reset to apply']:
            self.assertEqual(fixture.scrub(line), line)
        self.assertEqual(fixture.scrub('1.000 >> usb update 345624 FE7A7220'), '1.000 >> usb update 345624 FE7A7220')
        self.assertIsNone(fixture.scrub('1.000 >> usb update now'))

    def test_only_an_update_the_console_takes_is_kept(self):
        # As `parse_transfer` in the firmware's console.rs.
        self.assertIsNone(fixture.scrub('1.000 >> usb update 5 0x1234'))
        self.assertIsNone(fixture.scrub('1.000 >> usb update 4294967296 1234'))
        self.assertIsNone(fixture.scrub('1.000 >> usb update\t5 1234'))
        self.assertIsNone(fixture.scrub('1.000 >> usb update 5 123456789'))
        self.assertIsNone(fixture.scrub('1.000 >> usb update -5 1234'))
        self.assertEqual(fixture.scrub('1.000 >> usb update +5 +00001234 more'), '1.000 >> usb update 5 1234')
        self.assertEqual(fixture.scrub('1.000 >> usb update 345624 fe7a7220'), '1.000 >> usb update 345624 FE7A7220')

    def test_only_a_reply_to_an_operation_is_kept_and_only_to_its_status(self):
        self.assertEqual(fixture.scrub('1.000 [trace] 5 e 040F0440018DFC'), '1.000 [trace] 5 e 040F0440018DFC')
        self.assertEqual(fixture.scrub('1.000 [trace] 5 e 040E050181FC0012'), '1.000 [trace] 5 e 040E040181FC00')
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 e 040E0A01030C00010203040506'))
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 e 040F040001030C'))
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 e 040E0401'))

    def test_the_drivers_lost_reads_and_snapshots_are_kept(self):
        self.assertEqual(fixture.scrub('1.000 [trace] 5 l'), '1.000 [trace] 5 l')
        self.assertEqual(fixture.scrub('1.000 [trace] 5 s'), '1.000 [trace] 5 s')
        self.assertEqual(fixture.scrub('1.000 [trace] 5 s 0001080102'), '1.000 [trace] 5 s 0001080102')
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 s 00010801'))
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 s 0801080102'))
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 s 0001080103'))
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 s 00000F0100'))
        self.assertEqual(fixture.scrub('1.000 [trace] 5 s 00FF0E0100'), '1.000 [trace] 5 s 00FF0E0100')
        self.assertEqual(fixture.scrub('1.000 [trace] 5 m 01'), '1.000 [trace] 5 m 01')
        self.assertEqual(fixture.scrub('1.000 [trace] 5 m 00'), '1.000 [trace] 5 m 00')
        self.assertIsNone(fixture.scrub('1.000 [trace] 5 m 02'))

    def test_wall_clock_times_are_read_as_seconds_are(self):
        self.assertEqual(fixture.scrub('09:27:15.813 >> usb ble off'), '09:27:15.813 >> usb ble off')
        self.assertEqual(fixture.scrub('09:27:15.870 [trace] 946160 r'), '09:27:15.870 [trace] 946160 r')
        self.assertIsNone(fixture.scrub('9:27:15.813 >> usb ble off'))


if __name__ == '__main__':
    unittest.main()
