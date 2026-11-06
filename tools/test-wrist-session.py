#!/usr/bin/env python3
"""Host tests for wrist-session.py's command file and console; no watch needed."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import unittest.mock

spec = importlib.util.spec_from_file_location('wrist_session', Path(__file__).with_name('wrist-session.py'))
session = importlib.util.module_from_spec(spec)
spec.loader.exec_module(session)


class CommandFile(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name) / 'cmds'
        self.commands = session.Commands(self.path)

    def tearDown(self):
        self.dir.cleanup()

    def append(self, text):
        with open(self.path, 'a') as f:
            f.write(text)

    def test_a_line_appended_while_reading_is_sent_once(self):
        self.append('== host to the watch: ble yes\n')
        real_open, test = open, self

        class LateAppend:
            def __init__(self, *a):
                self.f = real_open(*a)

            def __enter__(self):
                return self

            def __exit__(self, *a):
                self.f.close()

            def __getattr__(self, name):
                return getattr(self.f, name)

            def read(self, *n):
                if not n:
                    test.append('ble yes\n')
                return self.f.read(*n)
        with unittest.mock.patch.object(session, 'open', LateAppend, create=True):
            sent = self.commands.new()
        sent += self.commands.new()
        self.assertEqual(sent, ['== host to the watch: ble yes', 'ble yes'])

    def test_a_file_left_from_an_earlier_session_and_rewritten_is_read_from_its_start(self):
        self.append('ble profile battery\nble profile cable\nble off\n')
        self.commands = session.Commands(self.path)
        self.path.write_text('ble on\n')
        self.assertEqual(self.commands.new(), ['ble on'])

    def test_a_rewritten_file_is_read_from_its_start(self):
        for before, after in (('ble profile battery\n', 'ble off\n'), ('ble off\n', 'ble profile battery\n'),
                              ('ble off\n', 'ble on\nble profile cable\n')):
            self.path.write_text('')
            self.commands = session.Commands(self.path)
            self.append(before)
            self.commands.new()
            self.path.write_text(after)
            self.assertEqual(self.commands.new(), after.splitlines(), (before, after))
        self.path.write_text('')
        self.assertEqual(self.commands.new(), [])
        self.append('ble on\n')
        self.assertEqual(self.commands.new(), ['ble on'])


class Gone:
    def read(self, n):
        raise OSError('device disconnected')


class Back:
    def read(self, n):
        return b'[usb] hello\n'


class Reopens(unittest.TestCase):
    def test_a_watch_that_resets_is_opened_again(self):
        opened, logged = [Gone(), Back()], []
        console = session.Console(logged.append, open_console=lambda: opened.pop(0), find=lambda: '/dev/x')
        self.assertEqual(console.read(), b'')
        self.assertEqual(console.read(), b'[usb] hello\n')
        self.assertEqual(logged, ['== console closed: device disconnected', '== console open'])

    def test_a_watch_gone_again_before_it_opens_is_waited_for(self):
        def gone_again():
            if opened:
                return opened.pop(0)
            raise SystemExit('no watch')
        opened, logged = [Gone()], []
        console = session.Console(logged.append, open_console=gone_again, find=lambda: '/dev/x')
        with unittest.mock.patch.object(session.time, 'sleep', side_effect=[None, None, StopIteration]):
            with self.assertRaises(StopIteration):
                console.read()
        self.assertEqual(logged, ['== console closed: device disconnected'])

    def test_a_command_while_the_watch_is_gone_is_logged_not_raised(self):
        logged = []
        console = session.Console(logged.append, open_console=lambda: None, find=lambda: None)
        console.send('ble on')
        self.assertTrue(logged[0].endswith(': ble on'), logged)


if __name__ == '__main__':
    unittest.main()
