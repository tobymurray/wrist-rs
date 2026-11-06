#!/usr/bin/env python3
"""Host tests for ble-drop.py, with a recording rfkill on PATH; no Bluetooth needed."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

DROP = Path(__file__).with_name('ble-drop.py')


class Unblocks(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        d = Path(self.dir.name)
        self.calls = d / 'calls'
        (d / 'rfkill').write_text(f'#!/bin/sh\necho "$1" >> {self.calls}\n')
        (d / 'rfkill').chmod(0o755)
        self.env = {**os.environ, 'PATH': f'{d}{os.pathsep}{os.environ["PATH"]}'}

    def tearDown(self):
        self.dir.cleanup()

    def run_drop(self, *args):
        return subprocess.Popen([sys.executable, DROP, *args], env=self.env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def calls_made(self):
        return self.calls.read_text().split()

    def test_a_command_file_that_cannot_be_written_still_unblocks(self):
        self.run_drop('0', '--command-file', f'{self.dir.name}/missing/cmds').wait(10)
        self.assertEqual(self.calls_made(), ['block', 'unblock'])

    def test_a_terminated_drop_unblocks(self):
        for sig in (signal.SIGTERM, signal.SIGHUP):
            self.calls.unlink(missing_ok=True)
            p = self.run_drop('30')
            end = time.time() + 10
            while not self.calls.exists() and time.time() < end:
                time.sleep(0.05)
            p.send_signal(sig)
            p.wait(10)
            self.assertEqual(self.calls_made(), ['block', 'unblock'], sig)


if __name__ == '__main__':
    unittest.main()
