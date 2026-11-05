#!/usr/bin/env python3
"""Hold the watch's USB console through a session: every line to <log> with the host's seconds,
the format `crates/wrist-ble-sim`'s replay reads, and each line appended to <commands> sent to the
watch (`== text` is written as a mark and not sent). Other tools drive the watch by appending to
<commands> while this holds the port (`wrist-ble.py --pair --command-file`).

    tools/wrist-session.py <log> <commands>
"""
import os
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wrist_port  # noqa: E402

log_path, cmd_path = sys.argv[1], sys.argv[2]
out = open(log_path, 'a')
con = wrist_port.open_console()
t0 = time.time()
lock = threading.Lock()


def log(line):
    with lock:
        out.write(f'{time.time() - t0:9.3f} {line}\n')
        out.flush()


def reader():
    buf = b''
    while True:
        try:
            buf += con.read(4096)
        except Exception as e:
            log(f'== console read failed: {e}')
            time.sleep(1)
            continue
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            log(line.decode('utf-8', 'replace').rstrip())


threading.Thread(target=reader, daemon=True).start()
open(cmd_path, 'a').close()
seen = os.path.getsize(cmd_path)
log('== session start')
while True:
    size = os.path.getsize(cmd_path)
    if size > seen:
        with open(cmd_path) as f:
            f.seek(seen)
            new = f.read()
        seen = size
        for c in (c.strip() for c in new.splitlines()):
            if c.startswith('== '):
                log(c)
            elif c:
                log(f'>> usb {c}')
                wrist_port.write_command(con, c)
    time.sleep(0.2)
