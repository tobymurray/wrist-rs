#!/usr/bin/env python3
"""Set the watch's clock to this machine's local time over the USB console,
with the zone's offset and its next two changes, so the watch follows
daylight saving for a year; and, given --alarm, the daily alarm.

    tools/wrist-time.py [--port /dev/ttyACM0] [--print] [--alarm HH:MM|on|off]
"""
import argparse
import os
import re
import sys
import time
from datetime import datetime, timedelta, timezone

DAYS_AHEAD = 396  # 13 months
MINUTE = timedelta(minutes=1)


def local_zone():
    """The host's zone: `TZ`, else /etc/localtime, else today's fixed offset."""
    try:
        from zoneinfo import ZoneInfo
        name = os.environ.get('TZ', '').lstrip(':')
        if name:
            return ZoneInfo(name)
        target = os.path.realpath('/etc/localtime')
        if 'zoneinfo/' in target:
            return ZoneInfo(target.split('zoneinfo/', 1)[1])
        with open('/etc/localtime', 'rb') as f:
            return ZoneInfo.from_file(f)
    except Exception:
        return datetime.now().astimezone().tzinfo


def offset_min(tz, utc):
    return int(utc.astimezone(tz).utcoffset() // MINUTE)


def changes(tz, now, limit=2):
    """The next offset changes after `now` (UTC), each as (UTC, new offset)."""
    found = []
    day = now.replace(second=0, microsecond=0)
    before = offset_min(tz, day)
    for _ in range(DAYS_AHEAD):
        nxt = day + timedelta(days=1)
        if offset_min(tz, nxt) != before:
            lo, hi = day, nxt  # the offset changes in (lo, hi]
            while hi - lo > MINUTE:
                mid = lo + (hi - lo) // 2
                mid -= timedelta(seconds=mid.second)
                if offset_min(tz, mid) == before:
                    lo = mid
                else:
                    hi = mid
            before = offset_min(tz, hi)
            found.append((hi, before))
            if len(found) == limit:
                break
        day = nxt
    return found


def lines(tz, now):
    stamp = now.astimezone(tz).strftime('%Y-%m-%d %H:%M:%S')
    zone = f"time zone {offset_min(tz, now)}"
    for at, minutes in changes(tz, now):
        zone += f" {int(at.timestamp())} {minutes}"
    return f"time set {stamp}", zone


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port')
    ap.add_argument('--print', action='store_true',
                    help="print the lines it would send and exit")
    ap.add_argument('--alarm', metavar='HH:MM|on|off',
                    help="also set the daily alarm, in the watch's local time, or turn it on or off")
    args = ap.parse_args()
    if args.alarm is not None and not re.fullmatch(r'([01]?\d|2[0-3]):[0-5]\d|on|off', args.alarm):
        ap.error("--alarm takes HH:MM on the 24-hour clock, on or off")
    extra = (f"alarm {args.alarm}",) if args.alarm is not None else ()
    tz = local_zone()
    if args.print:
        for line in lines(tz, datetime.now(timezone.utc)) + extra:
            print(line)
        return
    try:
        import serial
    except ImportError:
        sys.exit("pyserial is needed: pip install pyserial")
    import wrist_port
    s = wrist_port.open_console(args.port, timeout=0.5)
    # Send on a second boundary, so the watch's seconds start in step.
    now = time.time()
    time.sleep(1.0 - (now % 1.0))
    sent = lines(tz, datetime.now(timezone.utc).replace(microsecond=0)) + extra
    for line in sent:
        s.write(f"{line}\r\n".encode())
    deadline = time.time() + 4.0
    buf = b''
    while time.time() < deadline and (buf.count(b'[rtc]') < 2 or extra and b'[alarm]' not in buf):
        buf += s.read(4096)
    for line in buf.decode('ascii', 'replace').splitlines():
        if line.startswith(('[rtc]', '[alarm]', '[usb] unknown')):
            print(line)
    for line in sent:
        print(f"sent {line}")


if __name__ == '__main__':
    main()
