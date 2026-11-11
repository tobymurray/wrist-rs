//! A read decoded packet by packet, each advertising report and vendor event's list through its
//! decoders: nothing may panic, and nothing decoded may lie outside its packet.

#![no_main]

use bluenrg2::{
    ad_name, ad_uuid128s, ad_uuid16s, packet_len, packet_whole, parse, reports, Event, VendorEvent,
};
use libfuzzer_sys::fuzz_target;

fn within(outer: &[u8], inner: &[u8]) {
    let (o, i) = (outer.as_ptr_range(), inner.as_ptr_range());
    assert!(o.start <= i.start && i.end <= o.end);
}

fn vendor(packet: &[u8], e: VendorEvent<'_>) {
    match e {
        VendorEvent::AttributeModified { data, .. } => within(packet, data),
        VendorEvent::ReadResponse { value, .. }
        | VendorEvent::Notification { value, .. }
        | VendorEvent::Indication { value, .. } => within(packet, value),
        VendorEvent::Unknown { params, .. } => within(packet, params),
        VendorEvent::ReadByGroupTypeResponse { services, .. } => services.iter().for_each(drop),
        VendorEvent::ReadByTypeResponse { characteristics, .. } => {
            characteristics.iter().for_each(drop)
        }
        VendorEvent::FindInfoResponse { descriptors, .. } => descriptors.iter().for_each(drop),
        VendorEvent::FindByTypeValueResponse { ranges, .. } => ranges.iter().for_each(drop),
        _ => {}
    }
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
        let Some(packet) = packet_len(rest).and_then(|len| rest.get(..len)) else {
            // Nothing says where a packet ends, so the read ends.
            assert!(!packet_whole(rest));
            assert_eq!(parse(rest), Event::Malformed(rest));
            break;
        };
        assert!(!packet.is_empty());
        match parse(rest) {
            Event::Malformed(p) => within(rest, p),
            Event::Acl { data, .. } => within(packet, data),
            Event::CommandComplete { return_param_bytes, .. } => within(packet, return_param_bytes),
            Event::LeAdvertisingReport { data, .. } => {
                within(packet, data);
                advertising(data);
            }
            Event::Vendor(e) => vendor(packet, e),
            Event::LeDataLengthChange { .. } => assert!(packet.len() >= 14),
            Event::LeConnectionComplete { timing, .. } => {
                assert!(packet.len() >= 15);
                // The interval comes only with a whole event (Core Vol 4 Part E §7.7.65.1, §7.7.65.10).
                assert!(
                    timing.is_none() || packet.len() >= if packet[3] == 0x01 { 22 } else { 34 }
                );
            }
            Event::LeConnectionUpdateComplete { .. } => assert!(packet.len() >= 13),
            _ => assert!(packet_whole(rest)),
        }
        at += packet.len();
    }
});
