#!/usr/bin/env python3
"""Host tests for wrist-update.py's revert over a stand-in port; no watch needed."""
import importlib.util
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).parent))
spec = importlib.util.spec_from_file_location('wrist_update', Path(__file__).with_name('wrist-update.py'))
wrist_update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrist_update)


class Port:
    """Answers each command line with what `answers` gives for it."""

    def __init__(self, answers):
        self.answers = answers
        self.out = b''

    def write(self, data):
        for command, answer in self.answers.items():
            if data.startswith(command) and answer:
                self.out += answer.encode() + b'\n'

    def read(self, _n):
        data, self.out = self.out, b''
        return data


class Revert(unittest.TestCase):
    def setUp(self):
        real = wrist_update.wait_for
        wrist_update.wait_for = lambda port, needles, timeout, echo=True: real(port, needles, min(timeout, 0.3), False)
        self.addCleanup(setattr, wrist_update, 'wait_for', real)

    def test_a_lost_result_line_asks_boot(self):
        swap = Port({b'update revert': None, b'boot': '[boot] state swap (just updated; not yet marked booted)'})
        self.assertIsNone(wrist_update.revert(swap))
        unmarked = Port({b'update revert': None, b'boot': '[boot] state boot'})
        self.assertIn('went quiet', wrist_update.revert(unmarked))


if __name__ == '__main__':
    unittest.main()
