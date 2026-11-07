#!/usr/bin/env python3
"""Host tests for wrist-ble.py's pairing answers; no watch needed."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('wrist_ble', Path(__file__).with_name('wrist-ble.py'))
wrist_ble = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrist_ble)
answers = wrist_ble.pairing_answers


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


if __name__ == '__main__':
    unittest.main()
