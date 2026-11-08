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
`ble yes` there if they agree, and accepts. No button on the watch is pressed. For tests,
--host-answer and --watch-answer give either side's answer (yes, no, none for the watch,
never or cancel for the host), and --command-file sends the watch's into a logger's command file
(as `tools/wrist-session.py` reads it) with each step as a `== host` mark.

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

import wrist_port
from wrist_port import RX, AirConsole as Console, air_send as send

DEFAULT_MTU = 23
ATT_HEADER = 3
# The characteristics' value size, NUS_VALUE_LEN in the image's ble.rs.
VALUE_LEN = 244


async def acquire_mtu(client):
    """BlueZ reports 23 for a bonded watch until asked, and only `AcquireWrite` on a
    characteristic we may write without response answers; the other backends know already."""
    backend = getattr(client, "_backend", None)
    if not hasattr(backend, "_acquire_mtu"):
        return
    from bleak.backends.bluezdbus import defs
    from dbus_fast import Message
    import os
    char = client.services.get_characteristic(RX)
    for _ in range(5):
        try:
            reply = await backend._bus.call(Message(
                destination=defs.BLUEZ_SERVICE, path=char.obj[0],
                interface=defs.GATT_CHARACTERISTIC_INTERFACE,
                member="AcquireWrite", signature="a{sv}", body=[{}]))
            if reply.message_type.name == "ERROR":
                raise RuntimeError(reply.body)
            os.close(reply.unix_fds[0])
            backend._mtu_size = reply.body[1]
            return
        except Exception as e:
            err = e
            await asyncio.sleep(1)
    print(f"-- could not read the MTU ({err}); using {DEFAULT_MTU}", flush=True)


def mtu_of(client):
    return client.mtu_size or DEFAULT_MTU


async def fast_interval(client, con):
    """BlueZ connects at 30 ms and never moves; ask for 15 ms on this connection only."""
    if not hasattr(getattr(client, "_backend", None), "_acquire_mtu"):
        return
    print(f"-- {await wrist_port.air_hasten(client, con) or 'interval unknown'}", flush=True)


async def transfer(client, con, path, chunk_pace, chunk_max=VALUE_LEN):
    image = open(path, "rb").read()
    crc = zlib.crc32(image) & 0xFFFFFFFF
    mtu = mtu_of(client)
    chunk = max(DEFAULT_MTU - ATT_HEADER, min(chunk_max, mtu - ATT_HEADER))
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
    """Subscribe to TX and ping (`wrist_port.air_attach`); False, said why, when nothing answers."""
    why = await wrist_port.air_attach(client, con)
    if why and why != "no answer":
        print(f"-- {why}", flush=True)
    if why:
        print("-- no answer over the air: is this host bonded? (--pair)", flush=True)
    return why is None


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
    return wrist_port.open_console(port)


def usb_command(s, text, seconds=1.0):
    s.write((text + "\r\n").encode())
    s.flush()
    end = time.time() + seconds
    buf = b""
    while time.time() < end:
        buf += s.read(4096)
    return buf.decode("utf-8", "replace")


def pairing_answers(host_answer, watch_answer, shown, theirs):
    """What to send the watch (`ble yes`, `ble no` or None) and what the host's agent does (`yes`,
    `no`, `never` or `cancel`) once BlueZ shows `shown`. `match` answers by comparing it with the
    watch's own number, `theirs` (None when unknown)."""
    agree = theirs is not None and theirs == shown
    watch = {"match": "yes" if agree else "no", "none": None}.get(watch_answer, watch_answer)
    host = {"match": "yes" if agree else "no"}.get(host_answer, host_answer)
    return (f"ble {watch}" if watch else None), host


async def pair(name, port, host_answer="match", watch_answer="match", commands=None, hold=0.0):
    """Pair through BlueZ with an agent of our own. The watch's half goes over USB or, with
    `commands`, into a logger's command file, where every step is also written as a `== host` mark."""
    from dbus_fast import BusType, DBusError, Message, Variant
    from dbus_fast.aio import MessageBus
    from dbus_fast.service import ServiceInterface, method

    usb = None if commands else usb_console(port)
    loop = asyncio.get_running_loop()
    t0 = time.time()

    def mark(text):
        print(f"-- {time.time() - t0:8.3f} {text}", flush=True)
        if commands:
            with open(commands, "a") as f:
                f.write(f"== host {text}\n")

    async def tell_watch(text):
        mark(f"to the watch: {text}")
        if commands:
            with open(commands, "a") as f:
                f.write(text + "\n")
        else:
            await loop.run_in_executor(None, usb_command, usb, text, 0.5)

    async def watch_number():
        if usb is None:
            return None
        for _ in range(20):
            text = await loop.run_in_executor(None, usb_command, usb, "ble", 0.5)
            found = re.search(r"COMPARE with the central's number, then `ble yes` or `ble no`: (\d{6})", text)
            if found:
                return int(found.group(1))
        return None

    dev = None
    never = asyncio.Event()

    class Agent(ServiceInterface):
        def __init__(self):
            super().__init__("org.bluez.Agent1")

        @method()
        def Release(self):
            pass

        @method()
        async def RequestConfirmation(self, device: "o", passkey: "u"):
            mark(f"BlueZ shows {passkey:06d}")
            theirs = await watch_number()
            if usb is not None:
                mark(f"the watch shows {theirs if theirs is None else f'{theirs:06d}'}")
            watch, host = pairing_answers(host_answer, watch_answer, passkey, theirs)
            if watch:
                await tell_watch(watch)
            mark(f"the host answers {host}")
            if host == "no":
                raise DBusError("org.bluez.Error.Rejected", "refused")
            if host == "cancel":
                await dev.call_cancel_pairing()
            if host in ("never", "cancel"):
                await never.wait()

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
            mark("BlueZ cancelled the pairing")

    if usb is not None:
        status = usb_command(usb, "ble", 0.5)
        if "advertising" not in status and "connected" not in status:
            print(f"-- the watch's radio is not advertising: {status.strip()}", flush=True)
            return 1
    device = await find(name)
    if device is None:
        return 1
    path = device.details["path"]
    bus = await MessageBus(bus_type=BusType.SYSTEM).connect()

    def changed(msg):
        if msg.member == "PropertiesChanged" and msg.path == path and msg.body[0] == "org.bluez.Device1":
            seen = {k: v.value for k, v in msg.body[1].items() if k in ("Connected", "Paired", "Bonded")}
            if seen:
                mark(f"BlueZ {seen}")
    bus.add_message_handler(changed)
    await bus.call(Message(destination="org.freedesktop.DBus", path="/org/freedesktop/DBus",
                           interface="org.freedesktop.DBus", member="AddMatch", signature="s",
                           body=[f"type='signal',interface='org.freedesktop.DBus.Properties',path='{path}'"]))
    agent_path = "/rs/wrist/agent"
    bus.export(agent_path, Agent())
    bluez = bus.get_proxy_object("org.bluez", "/org/bluez", await bus.introspect("org.bluez", "/org/bluez"))
    manager = bluez.get_interface("org.bluez.AgentManager1")
    await manager.call_register_agent(agent_path, "DisplayYesNo")
    await manager.call_request_default_agent(agent_path)
    obj = bus.get_proxy_object("org.bluez", path, await bus.introspect("org.bluez", path))
    dev = obj.get_interface("org.bluez.Device1")
    props = obj.get_interface("org.freedesktop.DBus.Properties")

    async def connected():
        try:
            return (await props.call_get("org.bluez.Device1", "Connected")).value
        except DBusError:  # BlueZ drops a device it didn't pair
            return False
    try:
        if (await props.call_get("org.bluez.Device1", "Paired")).value:
            print("-- already paired; remove the watch from this host's devices first to pair again", flush=True)
            return 0
        mark(f"pairing with {device.address} (host: {host_answer}, watch: {watch_answer})")
        try:
            await asyncio.wait_for(dev.call_pair(), 90)
            await props.call_set("org.bluez.Device1", "Trusted", Variant("b", True))
            mark("paired, bonded and trusted")
            ok = True
        except (DBusError, asyncio.TimeoutError) as e:
            mark(f"pairing failed: {e!r}")
            ok = False
        if hold:
            mark(f"holding {hold:g} s")
            await asyncio.sleep(hold)
        if await connected():
            await dev.call_disconnect()
            mark("disconnected")
        if usb is not None:
            print(usb_command(usb, "ble bonds" if ok else "ble", 1.0).strip(), flush=True)
        return 0 if ok else 1
    finally:
        never.set()
        await manager.call_unregister_agent(agent_path)
        bus.disconnect()


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("commands", nargs="?", default="", help="commands separated by ;")
    ap.add_argument("--name", default=wrist_port.NAME)
    ap.add_argument("--pair", action="store_true", help="pair and bond (BlueZ), confirmed over USB")
    ap.add_argument("--port", help="the watch's USB console, for --pair")
    ap.add_argument("--host-answer", choices=["match", "yes", "no", "never", "cancel"], default="match",
                    help="--pair: the host's answer to the number (match: yes when it is the watch's)")
    ap.add_argument("--watch-answer", choices=["match", "yes", "no", "none"], default="match",
                    help="--pair: what to send the watch when the number shows (none: nothing)")
    ap.add_argument("--command-file", help="--pair: a logger's command file for the watch's answers and the marks, "
                                       "in place of the USB console")
    ap.add_argument("--hold", type=float, default=0.0, help="--pair: seconds to keep the link after pairing")
    ap.add_argument("--update", help="application image to send")
    ap.add_argument("--listen", type=float, default=2.0, help="seconds to keep listening at the end")
    ap.add_argument("--no-reset", action="store_true", help="after an update, do not send reset")
    ap.add_argument("--keep-interval", action="store_true", help="do not ask for 15 ms before a transfer")
    ap.add_argument("--chunk", type=int, default=VALUE_LEN, help="largest write of a transfer, in bytes")
    ap.add_argument("--pace", type=float, default=0.0, help="extra seconds between chunks of a transfer")
    args = ap.parse_args()

    if args.pair:
        if args.command_file and "match" in (args.host_answer, args.watch_answer):
            ap.error("--command-file can't read the watch's number: give --host-answer and --watch-answer")
        return await pair(args.name, args.port, args.host_answer, args.watch_answer, args.command_file, args.hold)
    return await run(args.name, args.commands, args.update, args.listen, not args.no_reset,
                     args.keep_interval, args.chunk, args.pace)


async def run(name, commands="", update=None, listen=2.0, reset=True, keep_interval=False, chunk=VALUE_LEN, pace=0.0):
    """Commands, then an update if one is given; 0 once it is done (with `reset`, once kept)."""
    device = await find(name)
    if device is None:
        return 1
    con = Console()
    async with BleakClient(device, timeout=20) as client:
        if not await attach(client, con):
            return 1
        await acquire_mtu(client)
        print(f"-- mtu {mtu_of(client)}", flush=True)
        ok = True
        for cmd in [c.strip() for c in commands.split(";") if c.strip()]:
            await send(client, cmd)
            await asyncio.sleep(0.5)
        if update and not keep_interval:
            await fast_interval(client, con)
        if update:
            ok = await transfer(client, con, update, pace, chunk)
        if ok and update and reset:
            print("-- reset sent", flush=True)
            await send(client, "reset")
            await asyncio.sleep(0.5)
        else:
            await asyncio.sleep(listen)
    if ok and update and reset:
        return 0 if await wait_kept(name, 120) else 1
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
