#!/usr/bin/env python3
"""Set the watch's clock to this machine's local time over the USB console, or with --ble over
Bluetooth to a bonded watch, with the zone's offset and its next two changes, so the watch follows
daylight saving for a year; and, given --alarm, the daily alarm. It then reads the clock back and
says how far it is from this machine's.

    tools/wrist-time.py [--port /dev/ttyACM0 | --ble [--name UNA]] [--print] [--alarm HH:MM|on|off]
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


def replied(text, alarm):
    """Whether the watch has answered `time set`, `time zone` and, if sent, `alarm`, in whole lines."""
    whole = text[:text.rfind('\n') + 1]
    return whole.count('[rtc]') >= 2 and (not alarm or '[alarm]' in whole)


def watch_clock(text):
    """The watch's local time in `time`'s report, to the RTC's subsecond, or None."""
    cal = re.search(r'calendar set;[^\n]*?(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d)', text)
    if not cal:
        return None
    at = datetime.strptime(cal.group(1), '%Y-%m-%d %H:%M:%S')
    prer = re.search(r'RTC_PRER\s*= 0x([0-9A-Fa-f]+)', text)
    ssr = re.search(r'RTC_SSR\s*= 0x([0-9A-Fa-f]+)', text)
    if prer and ssr:
        # The subsecond counter counts down from PREDIV_S each second.
        prediv = int(prer.group(1), 16) & 0x7FFF
        left = int(ssr.group(1), 16)
        if left <= prediv:
            at += timedelta(seconds=(prediv - left) / (prediv + 1))
    return at


def offset(watch, tz, sent, heard):
    """Seconds the watch's clock is ahead of this machine's, from a report asked for at `sent`
    and heard at `heard` (Unix seconds), taken as read halfway between; and that half."""
    mid = (sent + heard) / 2
    here = datetime.fromtimestamp(mid, timezone.utc).astimezone(tz).replace(tzinfo=None)
    return (watch - here).total_seconds(), (heard - sent) / 2


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--print', action='store_true',
                    help="print the lines it would send and exit")
    ap.add_argument('--alarm', metavar='HH:MM|on|off',
                    help="also set the daily alarm, in the watch's local time, or turn it on or off")
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import wrist_port
    wrist_port.add_link_arguments(ap)
    args = ap.parse_args()
    if args.alarm is not None and not re.fullmatch(r'([01]?\d|2[0-3]):[0-5]\d|on|off', args.alarm):
        ap.error("--alarm takes HH:MM on the 24-hour clock, on or off")
    if args.ble and args.port:
        ap.error("--port is the USB console; --ble does without it")
    extra = (f"alarm {args.alarm}",) if args.alarm is not None else ()
    tz = local_zone()
    if args.print:
        for line in lines(tz, datetime.now(timezone.utc)) + extra:
            print(line)
        return
    if not args.ble:
        try:
            import serial  # noqa: F401
        except ImportError:
            sys.exit("pyserial is needed: pip install pyserial")
    # Connect first: over the air that, and securing the link, take seconds.
    link = wrist_port.open_link(args)
    try:
        interval = link.hasten()
        if args.ble:
            print(f"-- {interval or 'interval unknown'}")
        # The lines for the next whole second, sent at it, so the watch's seconds start in step.
        at = int(time.time()) + 1
        if at - time.time() < 0.2:
            at += 1
        sent = lines(tz, datetime.fromtimestamp(at, timezone.utc)) + extra
        time.sleep(max(0.0, at - time.time()))
        for line in sent:
            link.send(line)
        text = link.read(4.0, lambda t: replied(t, extra))
        for line in text.splitlines():
            if line.startswith(('[rtc]', '[alarm]', '[usb] unknown')):
                print(line)
        for line in sent:
            print(f"sent {line}")
        asked = time.time()
        link.send('time')
        report = link.read(4.0, lambda t: re.search(r'calendar[^\n]*\n', t))
        heard = time.time()
    finally:
        link.close()
    watch = watch_clock(report)
    if watch is None:
        print("-- the clock was not read back")
    else:
        ahead, within = offset(watch, tz, asked, heard)
        print(f"-- read back: the watch is {ahead:+.2f} s from this machine, to within {within:.2f} s")


if __name__ == '__main__':
    main()
