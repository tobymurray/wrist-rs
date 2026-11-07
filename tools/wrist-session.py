#!/usr/bin/env python3
"""Hold the watch's USB console through a session: every line to <log> with the host's seconds,
the format `crates/wrist-ble-sim`'s replay reads, and each line appended to <commands> sent to the
watch (`== text` is written as a mark and not sent). Other tools drive the watch by appending to
<commands> while this holds the port (`wrist-ble.py --pair --command-file`).

    tools/wrist-session.py <log> <commands>
"""
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wrist_port  # noqa: E402


class Commands:
    def __init__(self, path):
        open(path, 'a').close()
        self.path = path
        with open(path, 'rb') as f:
            self.last = f.read()[-64:]
            self.seen = f.tell()

    def new(self):
        with open(self.path, 'rb') as f:
            f.seek(self.seen - len(self.last))
            if f.read(len(self.last)) != self.last:
                f.seek(0)
                self.last = b''
            data = f.read()
            self.seen = f.tell()
        self.last = (self.last + data)[-64:]
        return [c.strip() for c in data.decode('utf-8', 'replace').splitlines()]


class Console:
    def __init__(self, log, open_console=wrist_port.open_console, find=wrist_port.find):
        self.log, self.open_console, self.find = log, open_console, find
        self.con = open_console()

    def read(self):
        try:
            return self.con.read(4096)
        except Exception as e:
            self.log(f'== console closed: {e}')
            self.con = None
            while self.con is None:
                time.sleep(1)
                try:
                    self.con = self.find() and self.open_console()
                except (Exception, SystemExit):
                    pass
            self.log('== console open')
            return b''

    def send(self, text):
        try:
            wrist_port.write_command(self.con, text)
        except Exception as e:
            self.log(f'== not sent ({e}): {text}')


def main(log_path, cmd_path):
    out = open(log_path, 'a')
    t0 = time.time()
    lock = threading.Lock()

    def log(line):
        with lock:
            out.write(f'{time.time() - t0:9.3f} {line}\n')
            out.flush()

    console = Console(log)

    def reader():
        buf = b''
        while True:
            buf += console.read()
            while b'\n' in buf:
                line, buf = buf.split(b'\n', 1)
                log(line.decode('utf-8', 'replace').rstrip())

    threading.Thread(target=reader, daemon=True).start()
    commands = Commands(cmd_path)
    log('== session start')
    while True:
        for c in commands.new():
            if c.startswith('== '):
                log(c)
            elif c:
                log(f'>> usb {c}')
                console.send(c)
        time.sleep(0.2)


if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
