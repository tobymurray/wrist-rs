#!/usr/bin/env python3
"""Read the frame the face last drew over the USB console (`page dump`) and write it as a PNG.

    tools/wrist-frame.py [--port /dev/ttyACM0] [--scale N] [--raw-out F] [--compare F] out.png
    tools/wrist-frame.py --from console.log [...] out.png   # a dump captured earlier
    tools/wrist-frame.py --raw frame.raw [...] out.png      # 240 x 240 ABGR2222 bytes

--compare takes a raw frame, such as the host's render of the same face
(`cargo run -p wrist-glass --example face`), and counts the pixels that differ.
"""
import argparse
import struct
import sys
import time
import zlib

W = H = 240


def decode(lines):
    """The frame's bytes from the console's `[frame]` lines, checked against its CRC-32."""
    rows, crc, began = {}, None, False
    for line in lines:
        line = line.strip()
        at = line.find('[frame] ')
        if at < 0:
            continue
        words = line[at + 8:].split()
        if not words:
            continue
        if words[0] == 'begin':
            if words[1:4] != ['240', '240', 'abgr2222']:
                sys.exit(f"unexpected frame format: {' '.join(words)}")
            rows, crc, began = {}, None, True
        elif words[0] == 'end' and began:
            crc = int(words[2], 16)
            print(f"watch: {' '.join(words)}")
            break
        elif words[0] == 'abandoned':
            sys.exit(f"the watch abandoned the dump: {line}")
        elif began and words[0].isdigit() and len(words) == 2:
            code = bytes.fromhex(words[1])
            row = b''.join(bytes([code[i + 1]]) * code[i] for i in range(0, len(code), 2))
            if len(row) != W:
                sys.exit(f"row {words[0]} decodes to {len(row)} pixels, not {W}")
            rows[int(words[0])] = row
    if crc is None:
        sys.exit("no complete `[frame] begin` ... `[frame] end` in the input")
    missing = [y for y in range(H) if y not in rows]
    if missing:
        sys.exit(f"{len(missing)} rows missing, the first {missing[0]}")
    frame = b''.join(rows[y] for y in range(H))
    if zlib.crc32(frame) != crc:
        sys.exit(f"CRC-32 {zlib.crc32(frame):08X} does not match the watch's {crc:08X}")
    return frame


def read_console(port, seconds):
    import serial
    import wrist_port
    s = serial.Serial(wrist_port.find_or_exit(port), 115200, timeout=0.2)
    time.sleep(0.3)
    s.reset_input_buffer()
    s.write(b'page dump\r\n')
    s.flush()
    buf, end = b'', time.time() + seconds
    while time.time() < end:
        buf += s.read(65536)
        end = max(buf.find(b'[frame] end'), buf.find(b'[frame] abandoned'))
        if end >= 0 and b'\n' in buf[end:]:
            break
    return buf.decode('utf-8', 'replace')


def rgb(p):
    """ABGR2222 to 8-bit RGB: two bits a channel, red lowest."""
    return bytes(((p >> shift) & 3) * 85 for shift in (0, 2, 4))


def write_png(path, frame, scale):
    lut = [rgb(p) for p in range(256)]
    raw = bytearray()
    for y in range(H):
        line = b''.join(lut[p] * scale for p in frame[y * W:(y + 1) * W])
        raw += (b'\0' + line) * scale

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    with open(path, 'wb') as f:
        f.write(b'\x89PNG\r\n\x1a\n')
        f.write(chunk(b'IHDR', struct.pack('>IIBBBBB', W * scale, H * scale, 8, 2, 0, 0, 0)))
        f.write(chunk(b'IDAT', zlib.compress(bytes(raw), 9)))
        f.write(chunk(b'IEND', b''))


def compare(frame, other):
    if len(other) != len(frame):
        sys.exit(f"the comparison frame has {len(other)} bytes, not {len(frame)}")
    diff = [i for i in range(len(frame)) if frame[i] != other[i]]
    if not diff:
        print("identical to the comparison frame")
        return True
    xs, ys = [i % W for i in diff], [i // W for i in diff]
    print(f"{len(diff)} pixels differ, within x {min(xs)}..{max(xs)}, y {min(ys)}..{max(ys)}")
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port')
    src = ap.add_mutually_exclusive_group()
    src.add_argument('--from', dest='log', help='a console capture holding a dump')
    src.add_argument('--raw', help='a raw 240 x 240 ABGR2222 frame')
    ap.add_argument('--seconds', type=float, default=20, help='how long to wait for the dump')
    ap.add_argument('--scale', type=int, default=2)
    ap.add_argument('--raw-out', help='also write the frame as raw bytes')
    ap.add_argument('--log-out', help='also write the console text the dump came in')
    ap.add_argument('--compare', help='a raw frame to compare against')
    ap.add_argument('png')
    args = ap.parse_args()
    if args.raw:
        frame = open(args.raw, 'rb').read()
    else:
        text = open(args.log).read() if args.log else read_console(args.port, args.seconds)
        if args.log_out:
            open(args.log_out, 'w').write(text)
        frame = decode(text.splitlines())
    write_png(args.png, frame, args.scale)
    print(f"wrote {args.png}")
    if args.raw_out:
        open(args.raw_out, 'wb').write(frame)
    if args.compare and not compare(frame, open(args.compare, 'rb').read()):
        sys.exit(1)


if __name__ == '__main__':
    main()
