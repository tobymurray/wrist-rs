#!/usr/bin/env python3
"""Host tests for wrist-time.py's lines and read-back, and wrist_port's link; no watch needed."""
import importlib.util
from datetime import datetime, timedelta, timezone
from pathlib import Path
import sys
import unittest
from zoneinfo import ZoneInfo

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wrist_port  # noqa: E402

spec = importlib.util.spec_from_file_location('wrist_time', Path(__file__).with_name('wrist-time.py'))
wrist_time = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrist_time)

TORONTO = ZoneInfo('America/Toronto')

# `time`'s report from a `ble` build, over the air.
REPORT = """\
  RTC_ICSR  = 0x00000037
  RTC_PRER  = 0x007F00FF
  RTC_CR    = 0x00000000
  zone      : -05:00
  RTC_TR    = 0x00112314
  RTC_DR    = 0x0026F108
  RTC_SSR   = 0x0000006A
  calendar set; shadow registers synchronised: 2026-11-08 11:23:14  (weekday 7, 1 = Monday)
"""


class Lines(unittest.TestCase):
    def test_the_time_and_the_zone_with_its_next_two_changes(self):
        now = datetime(2026, 11, 8, 16, 20, 4, tzinfo=timezone.utc)
        self.assertEqual(wrist_time.lines(TORONTO, now),
                         ('time set 2026-11-08 11:20:04', 'time zone -300 1805007600 -240 1825567200 -300'))

    def test_a_zone_without_changes_sends_only_its_offset(self):
        now = datetime(2026, 11, 8, 16, 20, 4, tzinfo=timezone.utc)
        self.assertEqual(wrist_time.lines(ZoneInfo('Asia/Kolkata'), now)[1], 'time zone 330')

    def test_replied_waits_for_both_rtc_lines_and_any_alarm(self):
        both = '[rtc] set; running from LSE\n[rtc] zone -05:00, 2 change(s) kept\n'
        self.assertFalse(wrist_time.replied('[rtc] set; running from LSE\n', ()))
        self.assertTrue(wrist_time.replied(both, ()))
        self.assertFalse(wrist_time.replied(both, ('alarm 07:30',)))
        self.assertTrue(wrist_time.replied(both + '[alarm] 07:30 on\n', ('alarm 07:30',)))


class ReadBack(unittest.TestCase):
    def test_the_calendar_to_its_subsecond(self):
        # SSR counts down from PREDIV_S = 255: 0x6A left is 149/256 of a second gone.
        self.assertEqual(wrist_time.watch_clock(REPORT),
                         datetime(2026, 11, 8, 11, 23, 14) + timedelta(seconds=149 / 256))

    def test_no_subsecond_without_the_registers(self):
        self.assertEqual(wrist_time.watch_clock(REPORT.replace('RTC_SSR', 'RTC_XXX')),
                         datetime(2026, 11, 8, 11, 23, 14))

    def test_an_unset_or_missing_calendar_reads_none(self):
        self.assertIsNone(wrist_time.watch_clock('  calendar never initialised (INITS = 0): no time is kept\n'))
        self.assertIsNone(wrist_time.watch_clock(''))

    def test_the_offset_is_taken_halfway_between_asking_and_hearing(self):
        sent = datetime(2026, 11, 8, 16, 23, 14, tzinfo=timezone.utc).timestamp()
        watch = datetime(2026, 11, 8, 11, 23, 14, 300000)
        ahead, within = wrist_time.offset(watch, TORONTO, sent, sent + 0.2)
        self.assertAlmostEqual(ahead, 0.2)
        self.assertAlmostEqual(within, 0.1)


class Failures(unittest.TestCase):
    def test_bluez_says_whether_it_holds_a_bond(self):
        self.assertIs(wrist_port.bonded({'props': {'Paired': True}}), True)
        self.assertIs(wrist_port.bonded({'props': {'Paired': False}}), False)
        self.assertIsNone(wrist_port.bonded({'props': {}}))
        self.assertIsNone(wrist_port.bonded(object()))

    def test_each_failure_is_one_line(self):
        cases = [('bleak', ''), ('host', 'BleakDBusError: org.freedesktop.DBus.Error.ServiceUnknown'),
                 ('host', 'BleakError: No Bluetooth adapters found.'), ('host', 'BleakError: NotReady'),
                 ('host', 'OSError: odd'), ('not found', ''), ('not bonded', ''), ('connect', 'TimeoutError: '),
                 ('connect', 'BleakError: Authentication Failed'), ('attach', 'no answer')]
        for kind, detail in cases:
            text = wrist_port.air_failure(kind, 'UNA', 'C2:3D:4E:5F:60:71', detail)
            self.assertNotIn('\n', text, kind)

    def test_the_host_side(self):
        self.assertIn('systemctl start bluetooth',
                      wrist_port.air_failure('host', detail='BleakDBusError: [org.freedesktop.DBus.Error.ServiceUnknown]'))
        self.assertNotIn('systemctl', wrist_port.air_failure(
            'host', detail='BleakDBusError: [org.freedesktop.DBus.Error.AccessDenied]'))
        self.assertIn('bluetoothctl power on', wrist_port.air_failure('host', detail='BleakError: NotReady', linux=True))
        self.assertEqual(wrist_port.air_failure('host', detail='BleakError: Bluetooth device is turned off', linux=False),
                         "this computer's Bluetooth is off: turn it on")

    def test_a_mac_is_given_no_linux_commands(self):
        for kind, detail, paired in [('not bonded', '', None), ('attach', 'no answer', None), ('attach', 'no answer', True),
                                     ('connect', 'BleakError: disconnected', None)]:
            text = wrist_port.air_failure(kind, 'UNA', 'C1A3F0D2-0000-0000-0000-000000000000', detail, paired, linux=False)
            self.assertNotIn('bluetoothctl', text, kind)
            self.assertIn('Bluetooth settings', text, kind)

    def test_an_unconfirmed_first_pairing_is_not_called_a_lost_key(self):
        text = wrist_port.air_failure('attach', 'UNA', 'X', 'a write was not answered (TimeoutError())', None, linux=False)
        self.assertIn('run again and confirm the six digits', text)
        self.assertNotIn('lost', text)

    def test_a_paired_mac_whose_link_merely_dropped_is_told_to_run_again(self):
        text = wrist_port.air_failure('connect', 'UNA', 'X', 'BleakError: disconnected', None, linux=False)
        self.assertTrue(text.endswith('otherwise run again'), text)

    def test_no_bond_is_paired_and_confirmed_on_the_watch_not_over_usb(self):
        text = wrist_port.air_failure('not bonded', 'UNA', 'C2:3D:4E:5F:60:71', linux=True)
        self.assertIn('bluetoothctl pair C2:3D:4E:5F:60:71', text)
        self.assertIn("on the watch's Bluetooth page", text)
        self.assertNotIn('USB', text)
        self.assertEqual(wrist_port.air_failure('attach', 'UNA', 'X', 'no answer', paired=False, linux=True), text.replace(
            'C2:3D:4E:5F:60:71', 'X'))

    def test_a_refused_key_says_to_forget_the_watch_and_pair_again(self):
        for kind, detail in [('attach', 'no answer'), ('connect', 'BleakError: PIN or Key Missing'),
                             ('attach', "a write was not answered (BleakDBusError('org.bluez.Error.NotPermitted'))")]:
            text = wrist_port.air_failure(kind, 'UNA', 'C2:3D:4E:5F:60:71', detail, paired=True, linux=True)
            self.assertIn('bluetoothctl remove C2:3D:4E:5F:60:71', text, detail)
            self.assertIn('see Bluetooth', text)
            self.assertIn('otherwise run again', text)

    def test_a_connection_that_merely_failed_is_tried_again(self):
        self.assertIn('run again', wrist_port.air_failure('connect', 'UNA', 'X', 'TimeoutError: ', paired=True))


class FakeSerial:
    def __init__(self, chunks):
        self.chunks = list(chunks)

    @property
    def in_waiting(self):
        return len(self.chunks[0]) if self.chunks else 0

    def read(self, n):
        return self.chunks.pop(0) if self.chunks else b''


class Links(unittest.TestCase):
    def test_air_console_splits_notifications_into_lines(self):
        con = wrist_port.AirConsole(echo=None)
        for part in [b'[rtc] se', b't\r\n[rtc] zone', b' -05:00\r\n[al']:
            con.on_notify(None, bytearray(part))
        self.assertEqual(con.lines, ['[rtc] set', '[rtc] zone -05:00'])

    def test_usb_read_stops_once_the_answer_is_whole(self):
        link = wrist_port.UsbLink.__new__(wrist_port.UsbLink)
        link.s = FakeSerial([b'[rtc] set\r\n[rtc] zo', b'ne -05:00\r\n', b'more\r\n'])
        text = link.read(2.0, lambda t: wrist_time.replied(t, ()))
        self.assertEqual(text, '[rtc] set\r\n[rtc] zone -05:00\r\n')

    def test_a_link_that_drops_after_connecting_is_one_line(self):
        import asyncio
        link = wrist_port.AirLink.__new__(wrist_port.AirLink)
        link.name, link.loop, link.client = 'UNA', asyncio.new_event_loop(), object()

        from bleak.exc import BleakError

        async def fail(e):
            raise e
        with self.assertRaisesRegex(wrist_port.LinkError, r'^the link to UNA dropped \(BleakError: Not connected\): run again$'):
            link.run(fail(BleakError('Not connected')))
        with self.assertRaisesRegex(wrist_port.LinkError, r'^UNA did not secure the link'):
            link.run(fail(wrist_port.LinkError('UNA did not secure the link')))
        with self.assertRaises(ZeroDivisionError):
            link.run(fail(ZeroDivisionError()))
        link.loop.close()

    def test_air_read_returns_each_line_once(self):
        import asyncio
        link = wrist_port.AirLink.__new__(wrist_port.AirLink)
        link.loop = asyncio.new_event_loop()
        link.con = wrist_port.AirConsole(echo=None)
        link.seen = 0
        link.con.on_notify(None, b'one\r\ntwo\r\n')
        self.assertEqual(link.read(1.0, lambda t: 'two' in t), 'one\ntwo\n')
        self.assertEqual(link.read(0.0), '')
        link.loop.close()


if __name__ == '__main__':
    unittest.main()
