//! A read decoded packet by packet, each vendor event and advertising report through its
//! decoders: nothing may panic, and nothing decoded may lie outside its packet.

#![no_main]

use libfuzzer_sys::fuzz_target;
use wrist_aci::{
    ad_name, ad_uuid128s, ad_uuid16s, att_mtu, characteristics, descriptors, gap_proc_complete, gatt_error,
    notification, numeric_comparison, packet_len, packet_whole, pairing_complete, parse, proc_complete, read_value,
    reports, service_range, services, AttributeModified, Event, L2capUpdateRequest,
};

fn within(outer: &[u8], inner: &[u8]) {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    assert!(o.start <= i.start && i.end <= o.end);
}

fn vendor(params: &[u8]) {
    if let Some(w) = AttributeModified::decode(params) {
        within(params, w.data);
    }
    let _ = numeric_comparison(params);
    let _ = pairing_complete(params);
    let _ = att_mtu(params);
    let _ = gap_proc_complete(params);
    let _ = proc_complete(params);
    let _ = gatt_error(params);
    let _ = service_range(params);
    let _ = L2capUpdateRequest::decode(params);
    if let Some(v) = read_value(params) {
        within(params, v);
    }
    if let Some((_, v)) = notification(params) {
        within(params, v);
    }
    services(params).for_each(drop);
    characteristics(params).for_each(drop);
    descriptors(params).for_each(drop);
}

fn advertising(data: &[u8]) {
    for r in reports(data) {
        within(data, r.data);
        if let Some(name) = ad_name(r.data) {
            within(r.data, name);
        }
        ad_uuid16s(r.data).for_each(drop);
        ad_uuid128s(r.data).for_each(drop);
    }
}

fuzz_target!(|read: &[u8]| {
    let mut at = 0;
    while at < read.len() {
        let rest = &read[at..];
        let len = packet_len(rest);
        assert!(len > 0 && len <= rest.len());
        let packet = &rest[..len];
        match parse(rest) {
            Event::Malformed(p) => within(rest, p),
            Event::CommandComplete { return_param_bytes, .. } => within(packet, return_param_bytes),
            Event::LeAdvertisingReport { data } => {
                within(packet, data);
                advertising(data);
            }
            Event::Vendor { params, .. } => {
                within(packet, params);
                vendor(params);
            }
            _ => assert!(packet_whole(rest)),
        }
        // A packet cut short ends the read.
        if !packet_whole(rest) {
            break;
        }
        at += len;
    }
});
