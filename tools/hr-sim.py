#!/usr/bin/env python3
"""A heart-rate strap simulated by a Linux host (BlueZ), for testing the watch as a central
without a strap on anyone's chest.

    hr-sim.py                        serve a Heart Rate Service and advertise it, until Ctrl-C
    hr-sim.py --seconds 60           ... for a minute
    hr-sim.py --consume 02:80:E1:..  also subscribe to the watch's own heart rate (the relay)
                                     once the watch connects, and print what it sends
    hr-sim.py --consume ADDR --connect   ... connecting to the watch ourselves, as a rower would

The simulated strap advertises as HR-SIM with service 0x180D and notifies a Heart Rate
Measurement once a second while subscribed: flags 0x16 (skin contact, RR intervals), a rate
that drifts between 60 and 100 bpm, and the matching RR interval in 1/1024 s. The watch finds
it with `ble scan`.

With --consume, the host is also the watch's client over the same link: when the watch
connects, the host discovers the watch's GATT and subscribes to its Heart Rate Measurement,
so a watch relaying heart rate sends the host's own readings back to it.

Needs dbus-fast (pip install bleak brings it).
"""
import argparse
import asyncio
import sys
import time

from dbus_fast import BusType, Variant
from dbus_fast.aio import MessageBus
from dbus_fast.service import PropertyAccess, ServiceInterface, dbus_property, method

BASE = "/rs/wrist/hrsim"
HRS = "0000180d-0000-1000-8000-00805f9b34fb"
HRM = "00002a37-0000-1000-8000-00805f9b34fb"
BSL = "00002a38-0000-1000-8000-00805f9b34fb"


class Advertisement(ServiceInterface):
    def __init__(self):
        super().__init__("org.bluez.LEAdvertisement1")

    @method()
    def Release(self):
        pass

    @dbus_property(access=PropertyAccess.READ)
    def Type(self) -> "s":
        return "peripheral"

    @dbus_property(access=PropertyAccess.READ)
    def ServiceUUIDs(self) -> "as":
        return ["180d"]

    @dbus_property(access=PropertyAccess.READ)
    def LocalName(self) -> "s":
        return "HR-SIM"

    @dbus_property(access=PropertyAccess.READ)
    def Includes(self) -> "as":
        return []

    # General discoverable, as a strap is: GAP discovery (the watch's `ble scan`) reports only
    # discoverable advertisers' advertisements, and their scan responses otherwise.
    @dbus_property(access=PropertyAccess.READ)
    def Discoverable(self) -> "b":
        return True


class Service(ServiceInterface):
    def __init__(self):
        super().__init__("org.bluez.GattService1")

    @dbus_property(access=PropertyAccess.READ)
    def UUID(self) -> "s":
        return HRS

    @dbus_property(access=PropertyAccess.READ)
    def Primary(self) -> "b":
        return True


class Characteristic(ServiceInterface):
    def __init__(self, uuid, flags, value):
        super().__init__("org.bluez.GattCharacteristic1")
        self.uuid = uuid
        self.flags = flags
        self.value = value
        self.notifying = False

    @dbus_property(access=PropertyAccess.READ)
    def UUID(self) -> "s":
        return self.uuid

    @dbus_property(access=PropertyAccess.READ)
    def Service(self) -> "o":
        return f"{BASE}/service0"

    @dbus_property(access=PropertyAccess.READ)
    def Flags(self) -> "as":
        return self.flags

    @dbus_property(access=PropertyAccess.READ)
    def Value(self) -> "ay":
        return self.value

    @method()
    def ReadValue(self, options: "a{sv}") -> "ay":
        return self.value

    @method()
    def StartNotify(self):
        self.notifying = True
        print(f"-- {time.strftime('%H:%M:%S')} the watch subscribed", flush=True)

    @method()
    def StopNotify(self):
        self.notifying = False
        print(f"-- {time.strftime('%H:%M:%S')} the watch unsubscribed", flush=True)

    def notify(self, value):
        self.value = value
        if self.notifying:
            self.emit_properties_changed({"Value": value})


class Application(ServiceInterface):
    def __init__(self, objects):
        super().__init__("org.freedesktop.DBus.ObjectManager")
        self.objects = objects

    @method()
    def GetManagedObjects(self) -> "a{oa{sa{sv}}}":
        out = {}
        for path, iface in self.objects:
            props = {}
            for name in ("UUID", "Primary", "Service", "Flags", "Value"):
                if hasattr(type(iface), name):
                    sig = {"UUID": "s", "Primary": "b", "Service": "o", "Flags": "as", "Value": "ay"}[name]
                    props[name] = Variant(sig, getattr(iface, name))
            out[path] = {iface.name: props}
        return out


def measurement(t):
    """A rate drifting 60..100 bpm with its RR interval: flags 0x16, 8-bit rate, one RR."""
    phase = (t % 80) / 40
    bpm = int(60 + 40 * (phase if phase <= 1 else 2 - phase))
    rr = round(60 / bpm * 1024)
    return bytes([0x16, bpm, rr & 0xFF, rr >> 8]), bpm


async def consume(bus, adapter, address, connect, stop):
    """Subscribe to the watch's Heart Rate Measurement over whichever link joins us, and print
    what it sends. BlueZ is this host's GATT client on a link in either role, so when the
    watch connects to the simulated strap, the host reads the watch's heart rate back over
    that same link (two links between the same two devices are refused). With `connect`,
    the host opens the link itself, as a rower or a bike would."""
    dev_path = f"/org/bluez/{adapter}/dev_{address.upper().replace(':', '_')}"
    root = bus.get_proxy_object("org.bluez", "/", await bus.introspect("org.bluez", "/"))
    manager = root.get_interface("org.freedesktop.DBus.ObjectManager")
    asked = False
    while not stop.is_set():
        objects = await manager.call_get_managed_objects()
        dev = objects.get(dev_path, {}).get("org.bluez.Device1")
        if dev is None:
            await asyncio.sleep(1)
            continue
        if not dev["Connected"].value or not dev["ServicesResolved"].value:
            if connect and not asked:
                asked = True
                obj = bus.get_proxy_object("org.bluez", dev_path, await bus.introspect("org.bluez", dev_path))
                try:
                    await obj.get_interface("org.bluez.Device1").call_connect()
                except Exception as e:
                    print(f"-- consume: connect: {e}", flush=True)
                    asked = False
            await asyncio.sleep(1)
            continue
        chars = [path for path, ifaces in objects.items()
                 if path.startswith(dev_path + "/")
                 and ifaces.get("org.bluez.GattCharacteristic1", {}).get("UUID", Variant("s", "")).value == HRM]
        if not chars:
            print("-- consume: the watch is connected but shows no Heart Rate Measurement", flush=True)
            await asyncio.sleep(3)
            continue
        obj = bus.get_proxy_object("org.bluez", chars[0], await bus.introspect("org.bluez", chars[0]))
        props = obj.get_interface("org.freedesktop.DBus.Properties")

        def changed(iface, values, _invalid):
            if iface == "org.bluez.GattCharacteristic1" and "Value" in values:
                data = bytes(values["Value"].value)
                print(f"-- {time.strftime('%H:%M:%S')} the watch relayed {data.hex(' ')} "
                      f"({data[1] if len(data) > 1 else '?'} bpm)", flush=True)

        props.on_properties_changed(changed)
        await obj.get_interface("org.bluez.GattCharacteristic1").call_start_notify()
        print(f"-- {time.strftime('%H:%M:%S')} subscribed to the watch's heart rate ({chars[0]})", flush=True)
        while not stop.is_set():
            await asyncio.sleep(1)
            dev = (await manager.call_get_managed_objects()).get(dev_path, {}).get("org.bluez.Device1")
            if dev is None or not dev["Connected"].value:
                print(f"-- {time.strftime('%H:%M:%S')} the watch's link is down", flush=True)
                props.off_properties_changed(changed)
                asked = False
                break


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=0, help="stop after this long (0: until Ctrl-C)")
    ap.add_argument("--consume", metavar="WATCH", help="also subscribe to the watch's heart rate (relay test)")
    ap.add_argument("--connect", action="store_true", help="with --consume, open the link to the watch ourselves")
    ap.add_argument("--adapter", default="hci0")
    args = ap.parse_args()

    bus = await MessageBus(bus_type=BusType.SYSTEM).connect()
    hrm = Characteristic(HRM, ["notify"], measurement(0)[0])
    bsl = Characteristic(BSL, ["read"], bytes([1]))
    objects = [(f"{BASE}/service0", Service()), (f"{BASE}/service0/char0", hrm), (f"{BASE}/service0/char1", bsl)]
    for path, iface in objects:
        bus.export(path, iface)
    bus.export(BASE, Application(objects))
    adv_path = f"{BASE}/adv0"
    bus.export(adv_path, Advertisement())

    hci = f"/org/bluez/{args.adapter}"
    obj = bus.get_proxy_object("org.bluez", hci, await bus.introspect("org.bluez", hci))
    await obj.get_interface("org.bluez.GattManager1").call_register_application(BASE, {})
    await obj.get_interface("org.bluez.LEAdvertisingManager1").call_register_advertisement(adv_path, {})
    print("-- advertising HR-SIM (0x180D); Ctrl-C stops", flush=True)

    stop = asyncio.Event()
    task = asyncio.create_task(consume(bus, args.adapter, args.consume, args.connect, stop)) if args.consume else None
    start = time.time()
    sent = 0
    try:
        while not args.seconds or time.time() - start < args.seconds:
            value, bpm = measurement(time.time() - start)
            hrm.notify(value)
            if hrm.notifying:
                sent += 1
                if sent == 1 or sent % 10 == 0:
                    print(f"-- {time.strftime('%H:%M:%S')} notified {bpm} bpm ({sent} so far)", flush=True)
            await asyncio.sleep(1)
    except asyncio.CancelledError:
        pass
    finally:
        stop.set()
        if task:
            task.cancel()
        print(f"-- stopped after {sent} notifications", flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        sys.exit(0)
