"""Find the watch's USB console by its USB ID, so no other serial device is picked by mistake,
and open it; or reach the same console over Bluetooth. `open_link` gives a tool either, as
`--port`, `--ble` and `--name` choose (`add_link_arguments`), behind `send`, `read` and `close`."""
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


def write_command(s, text):
    """One console command, in packets the watch's receive path keeps up with."""
    data = (text.strip() + '\r\n').encode()
    for i in range(0, len(data), 60):
        s.write(data[i:i + 60]); s.flush(); time.sleep(0.005)


# A `ble` build's Nordic-UART-shaped console service; `bleak` is imported only when it is used.
NAME = "UNA"
RX = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
TX = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"
# The fastest interval asked for, in the 1.25 ms units of `ble interval now`.
FAST_INTERVAL = 12


class AirConsole:
    """The console's lines as TX notifies them, each also passed to `echo` if given."""

    def __init__(self, echo=lambda text: print(text, flush=True)):
        self.buf = b""
        self.lines = []
        self.echo = echo

    def on_notify(self, _handle, data):
        self.buf += bytes(data)
        while b"\n" in self.buf:
            line, self.buf = self.buf.split(b"\n", 1)
            text = line.decode("utf-8", "replace").rstrip("\r")
            self.lines.append(text)
            if self.echo:
                self.echo(text)

    async def wait_for(self, needles, timeout):
        import asyncio
        start = len(self.lines)
        end = time.time() + timeout
        while time.time() < end:
            for line in self.lines[start:]:
                for n in needles:
                    if n in line:
                        return line
            await asyncio.sleep(0.05)
        return None


async def air_send(client, text):
    await client.write_gatt_char(RX, (text + "\r\n").encode(), response=True)


async def air_attach(client, con):
    """Subscribe to TX and ping: None once the watch answers, else why not."""
    import asyncio
    await client.start_notify(TX, con.on_notify)
    for _ in range(10):
        try:
            await asyncio.wait_for(air_send(client, "ping"), 5)
        except Exception as e:
            return f"a write was not answered ({e!r})"
        if await con.wait_for(["pong"], 1):
            return None
    return "no answer"


async def air_hasten(client, con):
    """Ask for `FAST_INTERVAL` on this connection: the interval the watch then reports, or None."""
    import asyncio
    import re
    await air_send(client, f"ble interval now {FAST_INTERVAL} {FAST_INTERVAL}")
    line = None
    for _ in range(5):
        await asyncio.sleep(0.6)
        await air_send(client, "ble")
        line = await con.wait_for(["[ble] connected"], 2)
        if line and f"interval {FAST_INTERVAL * 1.25:.2f} ms" in line:
            break
    found = re.search(r"interval [0-9.]+ ms", line or "")
    return found.group(0) if found else None


def bonded(details):
    """Whether BlueZ holds a bond for a scanned device, or None where the backend does not say."""
    props = details.get("props") if isinstance(details, dict) else None
    return None if props is None or "Paired" not in props else bool(props["Paired"])


def air_failure(kind, name=NAME, address=None, detail="", paired=None):
    """The one line a tool exits with when the console over the air cannot be reached."""
    remove = f"bluetoothctl remove {address}" if address else "bluetoothctl remove <address>"
    pair = (f"pair it from this computer's Bluetooth settings (or `bluetoothctl pair {address or '<address>'}`) "
            f"and confirm the six digits on the watch's Bluetooth page")
    if kind == "bleak":
        return "--ble needs bleak: pip install bleak"
    if kind == "host":
        if any(s in detail for s in ("NameHasNoOwner", "ServiceUnknown", "org.freedesktop.DBus.Error")):
            return "Bluetooth is not running on this computer: sudo systemctl start bluetooth"
        if "No Bluetooth adapters" in detail:
            return "this computer has no Bluetooth adapter"
        if any(s in detail for s in ("NotReady", "not powered", "turned off", "powered off")):
            return "this computer's Bluetooth is off: turn it on (bluetoothctl power on)"
        return f"Bluetooth on this computer failed: {detail}"
    if kind == "not found":
        return (f"no watch advertising as {name}: turn its radio on (Select on its Bluetooth page); "
                f"if it is on, another phone or computer holds its one connection")
    if kind == "not bonded":
        return f"this computer is not bonded to {name}: {pair}"
    refused = any(s in detail for s in ("Authentication", "PIN or Key Missing", "Insufficient", "NotPermitted",
                                        "NotAuthorized", "EOFError", "Not connected", "disconnected"))
    if kind == "connect" and not refused:
        return f"could not connect to {name} ({detail}): run again"
    # A write refused or unanswered, or the link dropped while it was secured.
    if paired is False:
        return f"this computer is not bonded to {name}: {pair}"
    return (f"{name} does not accept this computer's key: if it shows `see Bluetooth` it has lost the bond, "
            f"so `{remove}` here, then {pair}")


class LinkError(Exception):
    pass


class UsbLink:
    """The console on USB, behind the same calls as `AirLink`."""

    def __init__(self, port=None, timeout=0.5):
        self.s = open_console(port, timeout)

    def hasten(self):
        return None

    def send(self, text):
        write_command(self.s, text)

    def read(self, seconds, until=None):
        """What arrives in `seconds`, returned as soon as `until(text so far)` holds."""
        end = time.time() + seconds
        buf = b""
        while time.time() < end:
            buf += self.s.read(max(1, self.s.in_waiting))
            if until and until(buf.decode("utf-8", "replace")):
                break
        return buf.decode("utf-8", "replace")

    def close(self):
        self.s.close()


class AirLink:
    """The console over Bluetooth, driven from synchronous code through its own event loop; raises
    LinkError with one line when it cannot connect."""

    def __init__(self, name=NAME):
        self.name = name
        self.loop = None
        self.client = None
        self.con = AirConsole(echo=None)
        self.seen = 0
        import asyncio
        import importlib.util
        if importlib.util.find_spec("bleak") is None:
            raise LinkError(air_failure("bleak"))
        self.loop = asyncio.new_event_loop()
        try:
            self.run(self.connect())
        except BaseException:
            self.close()
            raise

    def run(self, coro):
        return self.loop.run_until_complete(coro)

    async def connect(self):
        from bleak import BleakClient, BleakScanner
        try:
            device = await BleakScanner.find_device_by_name(self.name, timeout=15)
        except Exception as e:
            raise LinkError(air_failure("host", detail=f"{type(e).__name__}: {e}"))
        if device is None:
            raise LinkError(air_failure("not found", self.name))
        paired = bonded(device.details)
        if paired is False:
            raise LinkError(air_failure("not bonded", self.name, device.address))
        client = BleakClient(device, timeout=20)
        try:
            await client.connect()
        except Exception as e:
            raise LinkError(air_failure("connect", self.name, device.address, f"{type(e).__name__}: {e}", paired))
        self.client = client
        try:
            why = await air_attach(client, self.con)
        except Exception as e:
            why = f"{type(e).__name__}: {e}"
        if why:
            raise LinkError(air_failure("attach", self.name, device.address, why, paired))
        self.seen = len(self.con.lines)

    def hasten(self):
        return self.run(air_hasten(self.client, self.con))

    def send(self, text):
        self.run(air_send(self.client, text.strip()))

    def read(self, seconds, until=None):
        """What arrives in `seconds`, as lines, returned as soon as `until(text so far)` holds."""
        import asyncio

        async def gather():
            end = time.time() + seconds
            while True:
                text = "".join(line + "\n" for line in self.con.lines[self.seen:])
                if time.time() >= end or (until and until(text)):
                    self.seen = len(self.con.lines)
                    return text
                await asyncio.sleep(0.01)
        return self.run(gather())

    def close(self):
        if self.loop is None:
            return
        if self.client is not None:
            try:
                self.run(self.client.disconnect())
            except Exception:
                pass
        self.loop.close()
        self.loop = None


def add_link_arguments(ap):
    """--port, --ble and --name, for a tool that takes either link."""
    ap.add_argument("--port", help="the watch's USB console (default: found by its USB ID)")
    ap.add_argument("--ble", action="store_true", help="over Bluetooth to a bonded watch, in place of USB")
    ap.add_argument("--name", default=NAME, help=f"--ble: the watch's advertised name (default {NAME})")


def open_link(args, timeout=0.5):
    """The console the arguments name, USB unless --ble; a one-line exit if it cannot be reached."""
    if not args.ble:
        return UsbLink(args.port, timeout)
    try:
        return AirLink(args.name)
    except LinkError as e:
        sys.exit(str(e))
