#!/usr/bin/env python3
"""Host tests for wrist-ble.py's pairing answers; no watch needed."""
import asyncio
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('wrist_ble', Path(__file__).with_name('wrist-ble.py'))
wrist_ble = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrist_ble)
answers = wrist_ble.pairing_answers


class FastAnswers(unittest.TestCase):
    """A watch that answers before the write's own response returns."""

    def client(self, con, answers):
        class Client:
            mtu_size = 247

            async def start_notify(self, _uuid, _callback):
                pass

            async def write_gatt_char(self, _uuid, data, response):
                for command, answer in answers.items():
                    if data.startswith(command.encode()):
                        con.on_notify(None, (answer + '\n').encode())

        return Client()

    def test_attach_sees_a_fast_pong(self):
        import asyncio
        con = wrist_ble.Console(echo=lambda text: None)
        client = self.client(con, {'ping': '[console] pong'})
        self.assertIsNone(asyncio.run(asyncio.wait_for(wrist_ble.wrist_port.air_attach(client, con), 5)))

    def test_hasten_sees_a_fast_interval(self):
        import asyncio
        con = wrist_ble.Console(echo=lambda text: None)
        interval = f'{wrist_ble.wrist_port.FAST_INTERVAL * 1.25:.2f}'
        client = self.client(con, {'ble\r': f'[ble] connected, interval {interval} ms'})
        self.assertEqual(asyncio.run(asyncio.wait_for(wrist_ble.wrist_port.air_hasten(client, con), 5)),
                         f'interval {interval} ms')

    def test_kept_sees_a_fast_boot_state(self):
        import asyncio
        from unittest import mock
        made = []
        answers = {'ping': '[console] pong', 'boot': '[boot] state boot'}
        test = self

        class Console(wrist_ble.Console):
            def __init__(self):
                super().__init__(echo=lambda text: None)
                made.append(self)

        class Link:
            def __init__(self, *_args, **_kwargs):
                pass

            async def __aenter__(self):
                return test.client(made[-1], answers)

            async def __aexit__(self, *_exc):
                return False

        async def find(_name, timeout, adapter=None):
            return object()

        real_sleep = asyncio.sleep

        async def no_wait(_seconds):
            await real_sleep(0)

        with mock.patch.object(wrist_ble, 'Console', Console), \
                mock.patch.object(wrist_ble, 'BleakClient', Link), \
                mock.patch.object(wrist_ble, 'find', find), \
                mock.patch.object(wrist_ble.asyncio, 'sleep', no_wait):
            self.assertTrue(asyncio.run(asyncio.wait_for(wrist_ble.wait_kept('watch', 2), 5)))


class PairingAnswers(unittest.TestCase):
    def test_match_confirms_both_sides_only_when_the_numbers_agree(self):
        self.assertEqual(answers('match', 'match', 123456, 123456), ('ble yes', 'yes'))
        self.assertEqual(answers('match', 'match', 123456, 654321), ('ble no', 'no'))

    def test_an_unknown_watch_number_never_matches(self):
        self.assertEqual(answers('match', 'match', 123456, None), ('ble no', 'no'))

    def test_given_answers_ignore_the_comparison(self):
        self.assertEqual(answers('no', 'yes', 123456, 123456), ('ble yes', 'no'))
        self.assertEqual(answers('yes', 'no', 123456, None), ('ble no', 'yes'))

    def test_the_watch_can_be_told_nothing_and_the_host_can_stall_or_cancel(self):
        self.assertEqual(answers('never', 'none', 1, 1), (None, 'never'))
        self.assertEqual(answers('cancel', 'yes', 1, None), ('ble yes', 'cancel'))


class Port:
    """A watch's USB console: each command's answer, and after `connect()` the pairing as the watch says it."""

    def __init__(self, answers):
        import threading
        self.answers, self.sent, self.out = answers, [], b''
        self.lock = threading.Lock()

    def say(self, text):
        with self.lock:
            self.out += (text + '\r\n').encode()

    def write(self, data):
        command = data.decode().strip()
        if command:
            self.sent.append(command)
            for line in self.answers.get(command, []):
                self.say(line)

    def flush(self):
        pass

    def read(self, _n):
        import time
        with self.lock:
            data, self.out = self.out, b''
        if not data:
            time.sleep(0.01)
        return data


class ByHand(unittest.TestCase):
    """macOS: the operator compares the numbers; the tool answers the watch."""

    def pair(self, port, attach, answer='ask', reply='y'):
        from unittest import mock
        import io
        asked = []

        class Client:
            def __init__(self, *_args, **_kwargs):
                pass

            async def __aenter__(self):
                return self

            async def __aexit__(self, *_exc):
                return False

        async def find(_name, timeout=15, adapter=None):
            return object()

        def ask(prompt):
            asked.append(prompt)
            return reply
        with mock.patch.object(wrist_ble.wrist_port, 'open_console', lambda _port: port), \
                mock.patch.object(wrist_ble, 'find', find), mock.patch.object(wrist_ble, 'BleakClient', Client), \
                mock.patch.object(wrist_ble, 'attach', attach), mock.patch('sys.stdout', io.StringIO()) as out:
            code = asyncio.run(asyncio.wait_for(wrist_ble.pair_by_hand('UNA', None, answer, ask, timeout=5), 10))
        return code, asked, out.getvalue()

    def test_the_watch_s_number_is_shown_and_the_operator_s_answer_sent(self):
        port = Port({'ble': ['[ble] advertising as UNA, timing cable'], 'ble yes': ['[ble] paired'],
                     'ble bonds': ['[ble] bonded devices: 1 80:D1:CE:10:81:FD']})

        async def attach(_client, _con):
            port.say("[ble] pairing: the central shows a number; if it is 012345, `ble yes` on the USB console")
            while 'ble yes' not in port.sent:
                await asyncio.sleep(0.01)
            return True
        code, asked, out = self.pair(port, attach)
        self.assertEqual(code, 0)
        self.assertEqual(asked, ['   Does the host show 012345? [y/n] '])
        self.assertEqual(port.sent, ['ble', 'ble yes', 'ble bonds'])
        self.assertIn("The watch's number is 012345", out)
        self.assertIn('bonded devices: 1', out)

    def test_no_from_the_operator_is_no_to_the_watch(self):
        port = Port({'ble': ['[ble] advertising as UNA, timing cable'],
                     'ble no': ['[ble] pairing failed: status 02 (1 timeout, 2 failed, 3/4 LTK missing), reason 0C']})

        async def attach(_client, _con):
            port.say("[ble] pairing: the central shows a number; if it is 999999, `ble yes` on the USB console")
            while 'ble no' not in port.sent:
                await asyncio.sleep(0.01)
            return False
        code, _, out = self.pair(port, attach, reply='n')
        self.assertEqual(code, 1)
        self.assertIn('pairing failed', out)

    def test_a_first_write_that_gave_up_is_tried_again_once_paired(self):
        port = Port({'ble': ['[ble] advertising as UNA, timing cable'], 'ble yes': ['[ble] paired'],
                     'ble bonds': ['[ble] bonded devices: 1 80:D1:CE:10:81:FD']})
        tries = []

        async def attach(_client, _con):
            tries.append(1)
            if len(tries) == 1:
                port.say("[ble] pairing: the central shows a number; if it is 012345, `ble yes` on the USB console")
                raise RuntimeError('Insufficient authentication')
            return True
        code, asked, _ = self.pair(port, attach, answer='yes')
        self.assertEqual((code, asked, len(tries)), (0, [], 2))

    def test_already_bonded_needs_no_number(self):
        port = Port({'ble': ['[ble] advertising as UNA, timing cable'], 'ble bonds': ['[ble] bonded devices: 1']})

        async def attach(_client, _con):
            return True
        code, asked, out = self.pair(port, attach)
        self.assertEqual((code, asked), (0, []))
        self.assertIn('already bonded', out)

    def test_a_radio_that_is_off_is_said_and_nothing_connects(self):
        port = Port({'ble': ['[ble] off']})

        async def attach(_client, _con):
            raise AssertionError('connected')
        code, _, out = self.pair(port, attach)
        self.assertEqual(code, 1)
        self.assertIn('not advertising: [ble] off', out)


@unittest.skipUnless(importlib.util.find_spec('dbus_fast'), '--pair on BlueZ needs dbus-fast (Linux)')
class PairOnAdapter(unittest.TestCase):
    def test_pairing_looks_for_the_watch_on_the_adapter_named(self):
        from unittest import mock
        import io
        asked = []

        async def find(_name, timeout=15, adapter=None):
            asked.append(adapter)
            return None
        with mock.patch.object(wrist_ble, 'find', find), mock.patch('sys.stdout', io.StringIO()):
            code = asyncio.run(wrist_ble.pair('UNA', None, 'yes', 'none', commands='/dev/null', adapter='hci1'))
        self.assertEqual((code, asked), (1, ['hci1']))


class Revert(unittest.TestCase):
    """A watch that answers before the write's own response returns."""

    def outcome(self, answer):
        con = wrist_ble.Console(echo=lambda text: None)

        class Client:
            async def write_gatt_char(self, _uuid, data, response):
                if data.startswith(b'update revert'):
                    con.on_notify(None, (answer + '\n').encode())

        return asyncio.run(asyncio.wait_for(wrist_ble.revert(Client(), con), 2))

    def test_a_refusal_is_seen(self):
        self.assertFalse(self.outcome('[update] revert: the other slot is empty; nothing written'))

    def test_success_is_seen(self):
        self.assertTrue(self.outcome('[update] revert ok 1 bytes, crc 0x0, swap marked; reset to apply'))


class Transfer(unittest.TestCase):
    """A watch that answers the last chunk, or `boot`, before the write's own response returns."""

    def outcome(self, last_chunk_answer, boot_answer=None):
        import tempfile
        con = wrist_ble.Console(echo=lambda text: None)
        image = bytes(range(256)) * 4

        class Client:
            mtu_size = 247
            got = 0

            async def write_gatt_char(self, _uuid, data, response):
                if data.startswith(b'update '):
                    con.on_notify(None, b'[update] send 1024 bytes\n')
                elif data.startswith(b'boot'):
                    if boot_answer:
                        con.on_notify(None, (boot_answer + '\n').encode())
                else:
                    Client.got += len(data)
                    if Client.got == len(image) and last_chunk_answer:
                        con.on_notify(None, (last_chunk_answer + '\n').encode())

        real = con.wait_for

        async def quick(needles, timeout, since=None):
            return await real(needles, min(timeout, 0.3), since)

        con.wait_for = quick
        with tempfile.NamedTemporaryFile() as f:
            f.write(image)
            f.flush()
            return asyncio.run(asyncio.wait_for(wrist_ble.transfer(Client(), con, f.name, 0), 5))

    def test_an_ok_before_the_last_write_returns_is_seen(self):
        self.assertTrue(self.outcome('[update] ok 1024 bytes, crc 0, swap marked; reset to apply'))

    def test_a_lost_ok_falls_back_to_a_fast_boot_answer(self):
        self.assertTrue(self.outcome(None, '[boot] state swap (just updated; not yet marked booted)'))
        self.assertFalse(self.outcome(None, '[boot] state boot'))


if __name__ == '__main__':
    unittest.main()
