#!/usr/bin/env python3
"""The watch's console over Bluetooth: pairing, commands and application updates, from a
Linux or Mac host. The watch needs a build with the `ble` feature and `ble on`.

    wrist-ble.py --pair                     pair and bond, confirmed over the USB console
    wrist-ble.py "ble;boot"                 send commands, print the answers
    wrist-ble.py --update app.bin           stage, verify, write, reset, wait until kept
    wrist-ble.py --listen 30                just print the console for 30 s

Needs `bleak` (pip install bleak) and, for --update over USB-less links, a bond. Finds the
watch by its advertised name (--name, default UNA); the watch takes one central at a time.

The console answers only a bonded central over an encrypted link. BlueZ keeps the watch's
GATT table for a bonded watch; after a build that changes the table, remove the watch from
this host (`bluetoothctl remove <address>`) and pair again. --pair (Linux, BlueZ)
registers a D-Bus agent that displays and confirms, asks BlueZ to pair, and when BlueZ asks
whether the number matches, reads the watch's own number from its USB console (`ble`), sends
`ble yes` there if they agree, and accepts. No button on the watch is pressed.

The service is Nordic-UART-shaped: RX (we write, with response) 6E400002-..., TX (it
notifies) 6E400003-.... An update is the console's own `update <len> <crc>` followed by the
raw bytes in writes of the negotiated MTU minus three, each acknowledged before the next. On
BlueZ, which connects at 30 ms, the tool first asks the watch for 15 ms on this connection
(`ble interval now 12 12`). After the reset it connects again and waits for `state boot`:
the new image keeps itself once an encrypted link is up, which is how an update without a
cable is kept.
"""
import argparse
import asyncio
import re
import sys
import time
import zlib

from bleak import BleakClient, BleakScanner

RX = "6e400002-b5a3-f393-e0a9-e50e24dcca9e"
TX = "6e400003-b5a3-f393-e0a9-e50e24dcca9e"


class Console:
    def __init__(self):
        self.buf = b""
        self.lines = []

    def on_notify(self, _handle, data: bytearray):
        self.buf += bytes(data)
        while b"\n" in self.buf:
            line, self.buf = self.buf.split(b"\n", 1)
            text = line.decode("utf-8", "replace").rstrip("\r")
            self.lines.append(text)
            print(text, flush=True)

    async def wait_for(self, needles, timeout):
        start = len(self.lines)
        end = time.time() + timeout
        while time.time() < end:
            for line in self.lines[start:]:
                for n in needles:
                    if n in line:
                        return line
            await asyncio.sleep(0.05)
        return None


def mtu_of(client):
    """BlueZ 5.62 and later report the MTU on each characteristic; other backends know it."""
    char = client.services.get_characteristic(RX)
    size = getattr(char, "max_write_without_response_size", None)
    if hasattr(getattr(client, "_backend", None), "_acquire_mtu") and size:
        return size + 3
    return client.mtu_size or 23


async def send(client, text):
    await client.write_gatt_char(RX, (text + "\r\n").encode(), response=True)


async def fast_interval(client, con):
    """BlueZ connects at 30 ms and never moves; ask for 15 ms on this connection only."""
    if not hasattr(getattr(client, "_backend", None), "_acquire_mtu"):
        return
    await send(client, "ble interval now 12 12")
    line = None
    for _ in range(5):
        await asyncio.sleep(0.6)
        await send(client, "ble")
        line = await con.wait_for(["[ble] connected"], 2)
        if line and "interval 15.00 ms" in line:
            break
    found = re.search(r"interval [0-9.]+ ms", line or "")
    print(f"-- {found.group(0) if found else 'interval unknown'}", flush=True)


async def transfer(client, con, path, chunk_pace, chunk_max=244):
    image = open(path, "rb").read()
    crc = zlib.crc32(image) & 0xFFFFFFFF
    mtu = mtu_of(client)
    chunk = max(20, min(chunk_max, mtu - 3))
    print(f"-- update {len(image)} bytes, crc {crc:08X}, mtu {mtu}, chunks of {chunk}", flush=True)
    await send(client, f"update {len(image)} {crc:08X}")
    if not await con.wait_for(["send "], 5):
        print("-- the watch did not open a transfer", flush=True)
        return False
    t0 = time.time()
    # Writes with response: each chunk is acknowledged before the next, so the watch's event
    # queue is never outrun.
    for i in range(0, len(image), chunk):
        await client.write_gatt_char(RX, image[i:i + chunk], response=True)
        if chunk_pace:
            await asyncio.sleep(chunk_pace)
    print(f"-- sent in {time.time() - t0:.1f} s; waiting for the watch to verify and write", flush=True)
    line = await con.wait_for(["[update] ok", "error", "mismatch", "abandoned"], 90)
    if line and " ok " in line:
        return True
    if line is None:
        # The ok line can be lost while the core stalls on flash; the loader's state is the truth.
        await send(client, "boot")
        state = await con.wait_for(["[boot] state"], 5)
        if state and "swap" in state:
            print("-- the watch reports the swap marked; taking that as success", flush=True)
            return True
    print(f"-- transfer failed: {line}", flush=True)
    return False


async def find(name, timeout=15):
    print(f"-- looking for {name}", flush=True)
    device = await BleakScanner.find_device_by_name(name, timeout=timeout)
    if device is None:
        print("-- not found (`ble` over USB says whether it advertises; a stale link needs `ble disconnect`)",
              flush=True)
    return device


async def attach(client, con):
    """Subscribe to TX; the watch mirrors its console once the link is encrypted. A host that
    is not bonded gets no answer (BlueZ may try to pair, which the watch times out)."""
    await client.start_notify(TX, con.on_notify)
    for _ in range(10):
        try:
            await asyncio.wait_for(send(client, "ping"), 5)
        except (asyncio.TimeoutError, Exception) as e:
            print(f"-- a write was not answered ({e!r})", flush=True)
            break
        if await con.wait_for(["pong"], 1):
            return True
    print("-- no answer over the air: is this host bonded? (--pair)", flush=True)
    return False


async def wait_kept(name, timeout):
    """After the reset: connect again and wait for the loader's state to read `boot`."""
    end = time.time() + timeout
    await asyncio.sleep(5)
    while time.time() < end:
        device = await find(name, timeout=10)
        if device is None:
            continue
        con = Console()
        try:
            async with BleakClient(device, timeout=20) as client:
                if not await attach(client, con):
                    continue
                while time.time() < end:
                    await send(client, "boot")
                    line = await con.wait_for(["[boot] state"], 3)
                    if line and "state boot" in line:
                        print("-- kept: state boot", flush=True)
                        return True
                    await asyncio.sleep(2)
        except Exception as e:  # a reset under the link, say
            print(f"-- {e}; trying again", flush=True)
    print("-- not kept in time; do not reset the watch, it will revert", flush=True)
    return False


def usb_console(port):
    import serial
    sys.path.insert(0, __file__.rsplit("/", 1)[0])
    import wrist_port
    s = serial.Serial(wrist_port.find_or_exit(port), 115200, timeout=0.2)
    time.sleep(0.3)
    s.reset_input_buffer()
    return s


def usb_command(s, text, seconds=1.0):
    s.write((text + "\r\n").encode())
    s.flush()
    end = time.time() + seconds
    buf = b""
    while time.time() < end:
        buf += s.read(4096)
    return buf.decode("utf-8", "replace")


async def pair(name, port):
    """Pair through BlueZ with an agent of our own, the watch's half answered over USB."""
    from dbus_fast import BusType, DBusError, Variant
    from dbus_fast.aio import MessageBus
    from dbus_fast.service import ServiceInterface, method

    usb = usb_console(port)
    loop = asyncio.get_running_loop()

    async def watch_number():
        for _ in range(20):
            text = await loop.run_in_executor(None, usb_command, usb, "ble", 0.5)
            found = re.search(r"COMPARE with the central's number, then `ble yes` or `ble no`: (\d{6})", text)
            if found:
                return int(found.group(1))
        return None

    class Agent(ServiceInterface):
        def __init__(self):
            super().__init__("org.bluez.Agent1")

        @method()
        def Release(self):
            pass

        @method()
        async def RequestConfirmation(self, device: "o", passkey: "u"):
            theirs = await watch_number()
            print(f"-- BlueZ shows {passkey:06d}, the watch {theirs if theirs is None else f'{theirs:06d}'}",
                  flush=True)
            agree = theirs == passkey
            await loop.run_in_executor(None, usb_command, usb, "ble yes" if agree else "ble no", 0.5)
            if not agree:
                raise DBusError("org.bluez.Error.Rejected", "the numbers differ")

        @method()
        def RequestAuthorization(self, device: "o"):
            pass

        @method()
        def AuthorizeService(self, device: "o", uuid: "s"):
            pass

        @method()
        def RequestPinCode(self, device: "o") -> "s":
            raise DBusError("org.bluez.Error.Rejected", "numeric comparison only")

        @method()
        def DisplayPinCode(self, device: "o", pincode: "s"):
            pass

        @method()
        def RequestPasskey(self, device: "o") -> "u":
            raise DBusError("org.bluez.Error.Rejected", "numeric comparison only")

        @method()
        def DisplayPasskey(self, device: "o", passkey: "u", entered: "q"):
            pass

        @method()
        def Cancel(self):
            print("-- BlueZ cancelled the pairing", flush=True)

    status = usb_command(usb, "ble", 0.5)
    if "advertising" not in status and "connected" not in status:
        print(f"-- the watch's radio is not advertising: {status.strip()}", flush=True)
        return 1
    device = await find(name)
    if device is None:
        return 1
    path = device.details["path"]
    bus = await MessageBus(bus_type=BusType.SYSTEM).connect()
    agent_path = "/rs/wrist/agent"
    bus.export(agent_path, Agent())
    bluez = bus.get_proxy_object("org.bluez", "/org/bluez", await bus.introspect("org.bluez", "/org/bluez"))
    manager = bluez.get_interface("org.bluez.AgentManager1")
    await manager.call_register_agent(agent_path, "DisplayYesNo")
    await manager.call_request_default_agent(agent_path)
    obj = bus.get_proxy_object("org.bluez", path, await bus.introspect("org.bluez", path))
    dev = obj.get_interface("org.bluez.Device1")
    props = obj.get_interface("org.freedesktop.DBus.Properties")
    try:
        if (await props.call_get("org.bluez.Device1", "Paired")).value:
            print("-- already paired; remove the watch from this host's devices first to pair again", flush=True)
            return 0
        print(f"-- pairing with {device.address}", flush=True)
        await asyncio.wait_for(dev.call_pair(), 90)
        await props.call_set("org.bluez.Device1", "Trusted", Variant("b", True))
        print("-- paired, bonded and trusted", flush=True)
        await dev.call_disconnect()
        print(usb_command(usb, "ble bonds", 1.0).strip(), flush=True)
        return 0
    except Exception as e:
        print(f"-- pairing failed: {e}", flush=True)
        print(usb_command(usb, "ble", 0.5).strip(), flush=True)
        return 1
    finally:
        await manager.call_unregister_agent(agent_path)
        bus.disconnect()


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("commands", nargs="?", default="", help="commands separated by ;")
    ap.add_argument("--name", default="UNA")
    ap.add_argument("--pair", action="store_true", help="pair and bond (BlueZ), confirmed over USB")
    ap.add_argument("--port", help="the watch's USB console, for --pair")
    ap.add_argument("--update", help="application image to send")
    ap.add_argument("--listen", type=float, default=2.0, help="seconds to keep listening at the end")
    ap.add_argument("--no-reset", action="store_true", help="after an update, do not send reset")
    ap.add_argument("--keep-interval", action="store_true", help="do not ask for 15 ms before a transfer")
    ap.add_argument("--chunk", type=int, default=244, help="largest write of a transfer, in bytes")
    ap.add_argument("--pace", type=float, default=0.0, help="extra seconds between chunks of a transfer")
    args = ap.parse_args()

    if args.pair:
        return await pair(args.name, args.port)
    device = await find(args.name)
    if device is None:
        return 1
    con = Console()
    async with BleakClient(device, timeout=20) as client:
        if not await attach(client, con):
            return 1
        print(f"-- mtu {mtu_of(client)}", flush=True)
        ok = True
        for cmd in [c.strip() for c in args.commands.split(";") if c.strip()]:
            await send(client, cmd)
            await asyncio.sleep(0.5)
        if args.update and not args.keep_interval:
            await fast_interval(client, con)
        if args.update:
            ok = await transfer(client, con, args.update, args.pace, args.chunk)
        if ok and args.update and not args.no_reset:
            print("-- reset sent", flush=True)
            await send(client, "reset")
            await asyncio.sleep(0.5)
        else:
            await asyncio.sleep(args.listen)
    if ok and args.update and not args.no_reset:
        return 0 if await wait_kept(args.name, 120) else 1
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
