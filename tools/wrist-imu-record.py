#!/usr/bin/env python3
"""Save the watch's IMU recording (`imu record dump`) as two CSV files.

    tools/wrist-imu-record.py [--port /dev/ttyACM0] OUT
    tools/wrist-imu-record.py --from console.log OUT   # a dump captured earlier

OUT-samples.csv holds sample, x_mg, y_mg, z_mg at 50 Hz; OUT-marks.csv holds sample, steps,
activity: the chip's count after that many samples (activity 0 still, 1 walking, 2 running,
3 unknown). Start a recording with `imu record start` and end it with `imu record stop`.
"""
import argparse
import struct
import sys
import time
import zlib


def check(header, chunks, marks, crc):
    """A dump's samples' bytes checked against its count and CRC-32, or why they fail."""
    data = b''.join(chunks[k] for k in sorted(chunks))
    if len(data) != 6 * header['samples'] or len(marks) != header['marks']:
        return None, f"incomplete: {len(data) // 6} of {header['samples']} samples, {len(marks)} of {header['marks']} marks"
    if zlib.crc32(data) != crc:
        return None, f'CRC mismatch: {zlib.crc32(data):08X}, the watch said {crc:08X}'
    return data, None


def decode(lines):
    """The header, samples' bytes and marks of the last dump in the console's `[rec]` lines that
    passes its checks."""
    header, chunks, marks, best, problem = None, {}, [], None, None
    for line in lines:
        at = line.find('[rec] ')
        if at < 0:
            continue
        words = line[at + 6:].split()
        if not words:
            continue
        if words[0] == 'begin':
            fields = dict(zip(words[1::2], words[2::2]))
            header = {k: int(v) for k, v in fields.items()}
            chunks, marks = {}, []
        elif words[0] == 'a' and len(words) == 3:
            chunks[int(words[1])] = bytes.fromhex(words[2])
        elif words[0] == 'm' and len(words) == 4:
            marks.append(tuple(int(w) for w in words[1:]))
        elif words[0] == 'end' and len(words) == 3 and header is not None:
            data, problem = check(header, chunks, marks, int(words[2], 16))
            if data is not None:
                best = header, data, marks
        elif words[0].startswith('abandoned'):
            problem = 'the watch abandoned the dump: ' + line.strip()
        elif words[:2] == ['nothing', 'recorded']:
            problem = 'the watch holds no recording'
        elif words[:2] == ['still', 'recording;']:
            problem = 'the watch is still recording: `imu record stop` first'
    if best is None:
        sys.exit(problem or 'no complete dump found')
    return best


def read_console(port, seconds):
    import wrist_port
    s = wrist_port.open_console(port)
    s.write(b'imu record dump\r\n')
    s.flush()
    buf, deadline = b'', time.time() + seconds
    while time.time() < deadline:
        buf += s.read(65536)
        at = max(buf.find(b'[rec] end'), buf.find(b'[rec] abandoned'), buf.find(b'[rec] nothing'), buf.find(b'[rec] still'))
        if at >= 0 and b'\n' in buf[at:]:
            break
    return buf.decode('utf-8', 'replace')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port')
    ap.add_argument('--from', dest='log', help='a console capture holding a dump')
    ap.add_argument('--seconds', type=float, default=300, help='how long to wait for the dump')
    ap.add_argument('out')
    args = ap.parse_args()
    text = open(args.log).read() if args.log else read_console(args.port, args.seconds)
    header, data, marks = decode(text.splitlines())
    if header['range'] > 3:
        sys.exit('the watch could not read the accelerometer range')
    # ACC_RANGE: 0 is ±2 g at 16384 LSB/g, each step doubles the range.
    lsb_per_g = 16384 >> header['range']
    with open(args.out + '-samples.csv', 'w') as f:
        f.write('sample,x_mg,y_mg,z_mg\n')
        for i, (x, y, z) in enumerate(struct.iter_unpack('<hhh', data)):
            f.write(f'{i},{x * 1000 // lsb_per_g},{y * 1000 // lsb_per_g},{z * 1000 // lsb_per_g}\n')
    with open(args.out + '-marks.csv', 'w') as f:
        f.write('sample,steps,activity\n')
        for m in marks:
            f.write(','.join(map(str, m)) + '\n')
    counted = marks[-1][1] - marks[0][1] if marks else 0
    print(f"{header['samples']} samples ({header['samples'] / header['hz']:.0f} s), {len(marks)} marks; "
          f"the chip counted {counted} steps; wrote {args.out}-samples.csv and {args.out}-marks.csv")


if __name__ == '__main__':
    main()
