"""Find the watch's USB console by its USB ID, so no other serial device is picked by mistake,
and open it."""
import sys
import time

# pid.codes; the firmware's console.rs and 60-wrist-rs.rules carry the same pair.
VID, PID = 0x1209, 0xD1A1


def find(explicit=None):
    """The console's device path, or None while the watch is not there."""
    if explicit:
        return explicit
    from serial.tools import list_ports
    for port in sorted(list_ports.comports(), key=lambda p: p.device):
        if port.vid == VID and port.pid == PID:
            return port.device
    return None


def find_or_exit(explicit=None):
    port = find(explicit)
    if port is None:
        sys.exit(f"no watch console found (USB {VID:04x}:{PID:04x}): is the watch on a data cable, "
                 "running wrist-rs, and allowed by the host? Or pass --port.")
    return port


def open_console(explicit=None, timeout=0.2):
    """The console, open and quiet."""
    import serial
    return settle(serial.Serial(find_or_exit(explicit), 115200, timeout=timeout))


def settle(s):
    """The open console, quiet: the host's tty can echo the watch's greeting back before the port
    is raw, so a newline ends that half line on either side of dropping what the watch sends."""
    timeout = s.timeout
    s.timeout = 0.05
    time.sleep(0.3)
    s.write(b"\r\n")
    s.flush()
    end = time.time() + 2
    last = time.time()
    while time.time() < end and time.time() - last < 0.3:
        if s.read(4096):
            last = time.time()
    s.write(b"\r\n")
    s.flush()
    s.reset_input_buffer()
    s.timeout = timeout
    return s
