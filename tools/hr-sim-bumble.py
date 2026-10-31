#!/usr/bin/env python3
"""A heart-rate strap on Bumble's own host stack, which misbehaves on request in ways a BlueZ
peer can't.

    hr-sim-bumble.py hci-socket:0                  a strap, as hr-sim.py, on hci0
    hr-sim-bumble.py hci-socket:0 --vanish-after 40
                                     40 s after the central subscribes, reset the controller and
                                     exit. How the link ends depends on the controller: one that
                                     drops it silently gives a supervision timeout (08) on the
                                     central; an Intel AX200 sends a terminate (13) first
    ... --hang-up-after 25           end each link 25 s after the central subscribes (13)
    ... --att-delay 3                answer every ATT request 3 s late
    ... --drop-att 4                 leave the 4th ATT request on each link unanswered
    ... --late-service 20            serve no Heart Rate Service until 20 s after connecting, then
                                     add it and indicate Service Changed to a central that
                                     subscribed to it, in this run or, bonded, in an earlier one
    ... --refuse-updates             refuse the central's connection parameter requests (LE Remote
                                     Connection Parameter Request, negative reply 0x3B)
    ... --ask 400,650                ask the central for a 400-650 ms interval over L2CAP, 2 s after
                                     it subscribes
    ... --forget-bonds               hold keys in memory only, so a central bonded to an earlier run
                                     meets a strap that has lost its bond

The transport is any Bumble transport spec. `hci-socket:0` takes hci0 from BlueZ for as long as
this runs: power it off first (`sudo btmgmt power off`), turn ssp off after (`sudo btmgmt ssp
off`), and run as root or with cap_net_admin. `usb:0` is a dongle. Bonds persist in Bumble's JSON
key store, with the static address as the strap's identity, so a central stays bonded across
runs; a bonded central's Service Changed subscription persists in a file beside it.

Needs bumble (pip install bumble).
"""
import argparse
import asyncio
import json
from pathlib import Path
import struct
import sys
import time

from bumble import att, gatt, hci
from bumble.core import AdvertisingData
from bumble.device import Device, DeviceConfiguration
from bumble.keys import MemoryKeyStore
from bumble.transport import open_transport
from bumble.utils import AsyncRunner

ASK_AFTER_S = 2.0


def log(text):
    print(f"-- {time.strftime('%H:%M:%S')} {text}", flush=True)


def measurement(t):
    """A rate drifting 60..100 bpm with its RR interval: flags 0x16, 8-bit rate, one RR."""
    phase = (t % 80) / 40
    bpm = int(60 + 40 * (phase if phase <= 1 else 2 - phase))
    rr = round(60 / bpm * 1024)
    return bytes([0x16, bpm, rr & 0xFF, rr >> 8]), bpm


def heart_rate_service():
    hrm = gatt.Characteristic(
        gatt.GATT_HEART_RATE_MEASUREMENT_CHARACTERISTIC,
        gatt.Characteristic.Properties.NOTIFY,
        gatt.Characteristic.Permissions(0),
        measurement(0)[0],
    )
    bsl = gatt.Characteristic(
        gatt.GATT_BODY_SENSOR_LOCATION_CHARACTERISTIC,
        gatt.Characteristic.Properties.READ,
        gatt.Characteristic.Permissions.READABLE,
        bytes([1]),
    )
    return gatt.Service(gatt.GATT_HEART_RATE_SERVICE, [hrm, bsl]), hrm


def misbehave_on_att(device, args):
    """Delay or drop ATT requests to the server; responses the server sends are untouched."""
    server = device.gatt_server
    handle = server.on_gatt_pdu
    seen = {}

    def on_gatt_pdu(bearer, pdu):
        if pdu.op_code not in att.ATT_REQUESTS:
            return handle(bearer, pdu)
        seen[bearer] = seen.get(bearer, 0) + 1
        if seen[bearer] == args.drop_att:
            log(f"leaving ATT request {seen[bearer]} unanswered: {pdu.name}")
            return None
        if args.att_delay:
            asyncio.get_running_loop().call_later(args.att_delay, handle, bearer, pdu)
            return None
        return handle(bearer, pdu)

    server.on_gatt_pdu = on_gatt_pdu
    device.on(device.EVENT_CONNECTION, lambda connection: seen.pop(connection, None))


def refuse_parameter_requests(device):
    def refuse(event):
        log(f"refusing the central's request for {event.interval_min * 1.25:.2f}-"
            f"{event.interval_max * 1.25:.2f} ms, latency {event.max_latency}")
        AsyncRunner.spawn(device.host.send_sync_command(
            hci.HCI_LE_Remote_Connection_Parameter_Request_Negative_Reply_Command(
                connection_handle=event.connection_handle,
                reason=hci.HCI_UNACCEPTABLE_CONNECTION_PARAMETERS_ERROR,
            )))

    device.host.on_hci_le_remote_connection_parameter_request_event = refuse


def log_refused_keys(device):
    """Log a central's request to encrypt with a key the strap doesn't hold, which no event reports."""
    provide = device.host.long_term_key_provider

    async def provider(handle, rand, ediv):
        key = await provide(handle, rand, ediv)
        if key is None:
            connection = device.lookup_connection(handle)
            log(f"no key for {connection.peer_address if connection else handle}: encryption refused (a lost bond)")
        return key

    device.host.long_term_key_provider = provider


def remember_service_changed(device):
    """Keep a bonded central's Service Changed subscription across runs, in a file beside the key store."""
    path = getattr(device.keystore, "filename", None)
    if path is None:
        return
    path = Path(path).with_suffix(".service-changed.json")
    try:
        peers = set(json.loads(path.read_text()))
    except (OSError, ValueError):
        peers = set()
    changed = device.gatt_service.service_changed_characteristic

    def remember(peer, keep):
        if keep == (peer in peers):
            return
        (peers.add if keep else peers.discard)(peer)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(sorted(peers)))

    def on_subscription(bearer, characteristic, _notify, indicate):
        if characteristic is not changed:
            return
        connection = getattr(bearer, "connection", bearer)
        remember(str(connection.peer_address), indicate and connection.is_encrypted)

    def on_connection(connection):
        state = {"pairing": False, "restored": False}

        def restore():
            if connection.is_encrypted and not state["pairing"] and str(connection.peer_address) in peers:
                device.gatt_server.subscribers.setdefault(connection, {})[changed.handle] = bytes([2, 0])
                state["restored"] = True
                log("the central is subscribed to Service Changed from an earlier run")

        def paired(_keys):
            if state["restored"]:
                state["restored"] = False
                device.gatt_server.subscribers.get(connection, {}).pop(changed.handle, None)
            if str(connection.peer_address) in peers:
                remember(str(connection.peer_address), False)
                log("a new bond: the earlier run's Service Changed subscription is dropped")

        connection.on("connection_encryption_change", restore)
        connection.on("pairing_start", lambda: state.update(pairing=True))
        connection.on("pairing", paired)
        connection.on("pairing_failure", lambda _reason: state.update(pairing=False))

    device.gatt_server.on(device.gatt_server.EVENT_CHARACTERISTIC_SUBSCRIPTION, on_subscription)
    device.on(device.EVENT_CONNECTION, on_connection)


async def serve(hci_source, hci_sink, args):
    config = DeviceConfiguration()
    config.load_from_dict({"name": "HR-SIM", "address": args.address, "keystore": "JsonKeyStore",
                           "identity_address_type": 1})
    device = Device.from_config_with_hci(config, hci_source, hci_sink)
    if args.forget_bonds:
        device.keystore = MemoryKeyStore()
    service, hrm = heart_rate_service()
    if not args.late_service:
        device.add_service(service)
    device.advertising_data = bytes(AdvertisingData([
        (AdvertisingData.FLAGS, bytes([AdvertisingData.LE_GENERAL_DISCOVERABLE_MODE_FLAG
                                       | AdvertisingData.BR_EDR_NOT_SUPPORTED_FLAG])),
        (AdvertisingData.COMPLETE_LOCAL_NAME, b"HR-SIM"),
        (AdvertisingData.INCOMPLETE_LIST_OF_16_BIT_SERVICE_CLASS_UUIDS, struct.pack("<H", 0x180D)),
    ]))
    if args.att_delay or args.drop_att:
        misbehave_on_att(device, args)
    if args.refuse_updates:
        refuse_parameter_requests(device)
    log_refused_keys(device)

    done = asyncio.get_running_loop().create_future()
    subscribed = set()

    async def add_service_later():
        await asyncio.sleep(args.late_service)
        if service in device.gatt_server.services:
            return
        device.add_service(service)
        changed = device.gatt_service.service_changed_characteristic
        listening = any(cccd.get(changed.handle, b"\0")[0] & 0x02 for cccd in device.gatt_server.subscribers.values())
        log(f"Heart Rate Service added at handles {service.handle:#06x}-{service.end_group_handle:#06x}; "
            + ("indicating Service Changed" if listening else "the central isn't subscribed to Service Changed"))
        await device.indicate_subscribers(changed, struct.pack("<HH", service.handle, service.end_group_handle))

    async def after_subscribe(connection):
        if args.ask:
            await asyncio.sleep(ASK_AFTER_S)
            low, high = args.ask
            log(f"asking for {low}-{high} ms")
            try:
                await connection.update_parameters(low, high, 0, max(6 * high, 4000), use_l2cap=True)
                log("the central accepted")
            except Exception as e:
                log(f"the central refused: {e}")
        if args.hang_up_after:
            await asyncio.sleep(args.hang_up_after)
            log("hanging up (13)")
            await connection.disconnect(hci.HCI_REMOTE_USER_TERMINATED_CONNECTION_ERROR)
        elif args.vanish_after:
            await asyncio.sleep(args.vanish_after)
            log("resetting the controller")
            await device.host.send_sync_command(hci.HCI_Reset_Command())
            done.set_result(None)

    def on_subscription(connection, notify, _indicate):
        if notify and connection not in subscribed:
            subscribed.add(connection)
            log("the central subscribed")
            AsyncRunner.spawn(after_subscribe(connection))
        elif not notify:
            subscribed.discard(connection)
            log("the central unsubscribed")

    hrm.on("subscription", on_subscription)

    def on_connection(connection):
        log(f"connected to {connection.peer_address}, interval {connection.parameters.connection_interval:.2f} ms")
        connection.on("disconnection", lambda reason: (
            subscribed.discard(connection), log(f"link down, reason {reason:02X}")))
        connection.on("connection_parameters_update", lambda: log(
            f"interval {connection.parameters.connection_interval:.2f} ms, "
            f"latency {connection.parameters.peripheral_latency}"))
        connection.on("connection_encryption_change", lambda: log("encrypted" if connection.is_encrypted else "encryption off"))
        connection.on("connection_encryption_failure", lambda error: log(f"encryption failed: {error}"))
        connection.on("pairing", lambda _keys: log("paired"))
        connection.on("pairing_failure", lambda reason: log(f"pairing failed: {reason}"))
        if args.late_service:
            AsyncRunner.spawn(add_service_later())

    device.on(device.EVENT_CONNECTION, on_connection)

    await device.power_on()
    remember_service_changed(device)
    await device.start_advertising(auto_restart=True)
    log(f"advertising HR-SIM (0x180D) as {device.random_address}; Ctrl-C stops")

    start = time.time()
    sent = 0
    while not done.done() and (not args.seconds or time.time() - start < args.seconds):
        value, bpm = measurement(time.time() - start)
        hrm.value = value
        if subscribed:
            await device.notify_subscribers(hrm, value)
            sent += 1
            if sent == 1 or sent % 10 == 0:
                log(f"notified {bpm} bpm ({sent} so far)")
        await asyncio.sleep(1)
    log(f"stopped after {sent} notifications")


def interval_pair(text):
    low, high = (float(x) for x in text.split(","))
    return low, high


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("transport", help="a Bumble transport: hci-socket:0, usb:0, ...")
    ap.add_argument("--address", default="C2:48:52:53:49:4D", help="the strap's static random address")
    ap.add_argument("--seconds", type=float, default=0, help="stop after this long (0: until Ctrl-C)")
    ends = ap.add_mutually_exclusive_group()
    ends.add_argument("--vanish-after", type=float, metavar="S")
    ends.add_argument("--hang-up-after", type=float, metavar="S")
    ap.add_argument("--att-delay", type=float, default=0, metavar="S")
    ap.add_argument("--drop-att", type=int, default=0, metavar="N")
    ap.add_argument("--late-service", type=float, default=0, metavar="S")
    ap.add_argument("--refuse-updates", action="store_true")
    ap.add_argument("--ask", type=interval_pair, metavar="MIN,MAX")
    ap.add_argument("--forget-bonds", action="store_true")
    args = ap.parse_args()

    async with await open_transport(args.transport) as transport:
        await serve(transport.source, transport.sink, args)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        sys.exit(0)
