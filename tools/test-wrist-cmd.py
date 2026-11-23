#!/usr/bin/env python3
"""Host tests for wrist-cmd.py's relay; no watch needed."""
import importlib.util
from pathlib import Path
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
spec = importlib.util.spec_from_file_location('wrist_cmd', Path(__file__).with_name('wrist-cmd.py'))
wrist_cmd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrist_cmd)


class Relay(unittest.TestCase):
    def test_each_read_is_shown_as_it_comes_and_a_split_character_joined(self):
        reads = [b'[ble] pairing: if it is 012345\r\n', b'', b'[usb] caf\xc3', b'\xa9\r\n']
        shown = []
        wrist_cmd.relay(lambda: reads.pop(0), shown.append, lambda: not reads)
        self.assertEqual(shown[0], '[ble] pairing: if it is 012345\r\n')
        self.assertEqual(shown[1:], ['[usb] caf', '\xe9\r\n', ''])

    def test_follow_keeps_reading_past_the_seconds(self):
        port = mock.Mock()
        port.read.side_effect = [b'a\n', b'b\n', KeyboardInterrupt]
        with mock.patch.object(wrist_cmd.wrist_port, 'open_console', return_value=port), \
                mock.patch.object(wrist_cmd.wrist_port, 'write_command'), mock.patch.object(wrist_cmd.time, 'sleep'), \
                mock.patch.object(sys, 'argv', ['wrist-cmd.py', '--follow', 'ble', '0']), \
                mock.patch('sys.stdout') as out:
            wrist_cmd.main()
        self.assertEqual([c.args[0] for c in out.write.call_args_list if c.args[0]], ['a\n', 'b\n'])


if __name__ == '__main__':
    unittest.main()
