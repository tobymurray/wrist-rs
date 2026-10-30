#!/usr/bin/env python3
"""A heart-rate strap simulated by a Linux host (BlueZ), for testing the watch as a central
without a strap on anyone's chest.

    hr-sim.py                        serve a Heart Rate Service and advertise it, until Ctrl-C
    hr-sim.py --seconds 60           ... for a minute

The simulated strap advertises as HR-SIM with service 0x180D and notifies a Heart Rate
Measurement once a second while subscribed. The watch finds it with `ble scan`, and reads it
with `hr strap <this host's address>`.

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

    # General discoverable, as a strap is.
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


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seconds", type=float, default=0, help="stop after this long (0: until Ctrl-C)")
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
        print(f"-- stopped after {sent} notifications", flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        sys.exit(0)
