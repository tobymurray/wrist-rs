"""Find the watch's USB console by its USB ID, so no other serial device is picked by mistake."""
import sys

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
