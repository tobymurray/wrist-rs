#![no_std]

//! The BlueNRG-2 as the Bluetooth core sees it: what it can do, the link-layer scheduler ST
//! documents in PM0257 Rev 4 §4, and the codes it refuses with.

pub mod commands;
pub mod events;

use wrist_ble::{
    Activities, AdvParams, Admitted, Anchor, Capabilities, CentralSlot, Change, ConnParams, Duration, Exclusion, GattLimits, Interval, Micros,
    Millis, OpKind, ParamAnswer, RoleSet, SchedKind, Schedule, ScanParams, Slots, MAX_LINKS,
};

pub const CAPABILITIES: Capabilities = Capabilities {
    max_links: 8,
    max_central: 8,
    // A peripheral of up to two centrals (PM0257 §4.4).
    max_peripheral: 2,
    roles_concurrent: RoleSet::ALL,
    adv_event_len: Micros(14_600),
    guard: Micros(1_500),
    discovery_max: Some(Millis(10_200)),
    peer_param_requests: ParamAnswer::RejectOnly,
    // ST's DTM_config.h; the 8 services measured by `ble probe gatt` (HARDWARE.md).
    gatt: GattLimits { services: 8, attributes: 68, mtu: 220 },
    op_deadline,
    exclusions: &[Exclusion::NoAdvertiseWhileInitiating, Exclusion::NoInitiateWhileInitiating],
    encryption_quiet: Millis(1_500),
    // Up within 3 s of reset (HARDWARE.md, the Bluetooth module), then configured.
    start_within: Millis::secs(5),
};

/// The latest each kind of operation ends: a command's reply, or the procedure on air within the
/// Core Specification's own timeouts (Vol 3 Part F §3.3.3, Vol 3 Part H §3.4).
#[must_use]
pub const fn op_deadline(kind: OpKind) -> Millis {
    match kind {
        OpKind::Connect => Millis::secs(30),
        OpKind::Disconnect => Millis::secs(10),
        OpKind::UpdateParams | OpKind::RequestSecurity | OpKind::Discover | OpKind::Read | OpKind::Write | OpKind::Subscribe => {
            Millis::secs(30)
        }
        OpKind::Advertise
        | OpKind::Scan
        | OpKind::CancelConnect
        | OpKind::AnswerParamRequest
        | OpKind::AllowRebond
        | OpKind::ConfirmNumeric
        | OpKind::Passkey
        | OpKind::Notify
        | OpKind::Bonds => Millis::secs(1),
    }
}

/// The chip's own code for a scheduler refusal (ST's `ble_status.h`).
#[must_use]
pub const fn native(kind: SchedKind) -> u16 {
    match kind {
        SchedKind::NoSlot => 0x82,
        SchedKind::NewIntervalFailed => 0x84,
        SchedKind::IntervalTooLarge => 0x85,
        SchedKind::LengthFailed => 0x86,
        SchedKind::Excluded(_) => 0x0C,
    }
}

/// Time in the scheduler's arithmetic, in nanoseconds so that every period it meets is exact.
pub type Ns = u64;

const UNIT: Ns = 1_250_000;
const SLOT: Ns = 625_000;
const MS: Ns = 1_000_000;

/// The interval the chip advertises at (PM0257 §4.2.3).
#[must_use]
pub fn adv_period(p: &AdvParams) -> Ns {
    (u64::from(p.interval_min.as_u16()) + u64::from(p.interval_max.as_u16())) * SLOT / 2 + 5 * MS
}

#[must_use]
pub fn interval_ns(i: Interval) -> Ns {
    u64::from(i.as_u16()) * UNIT
}

fn scan_ns(p: &ScanParams) -> Ns {
    u64::from(p.interval.as_u16()) * SLOT
}

fn len_ns(s: Slots) -> Ns {
    u64::from(s.as_u16()) * SLOT
}

fn micros(m: Micros) -> Ns {
    u64::from(m.0) * 1_000
}

/// The period of the central time base, while a central link holds it.
#[must_use]
pub fn anchor(a: &Activities) -> Option<Ns> {
    a.central.iter().flatten().map(|s| interval_ns(s.interval) / u64::from(s.latency.max(1))).min()
}

/// What `ACI_HAL_GET_ANCHOR_PERIOD` reads beside `a`: the time base a first central link would
/// join, and its longest free slot in whole 0.625 ms slots, as the chip counts it; `None` where the
/// chip reads 0 for both (idle, or only links as peripheral). HARDWARE.md, `ble probe`.
#[must_use]
pub fn anchor_reading(a: &Activities) -> Option<Anchor> {
    let period = anchor(a).or_else(|| a.advertising.as_ref().map(adv_period))?;
    let slots = free(period, occupied(a, None, false)) / SLOT;
    let us = |ns: Ns| Micros(u32::try_from(ns / 1_000).unwrap_or(u32::MAX));
    Some(Anchor { period: us(period), max_free_slot: us(slots * SLOT) })
}

/// PM0257 §4.2's slot allocator, taking `ce_len` as a slot's minimum and maximum length alike.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scheduler;

impl Schedule for Scheduler {
    fn admit(&self, a: &Activities, change: &Change) -> Result<Admitted, SchedKind> {
        let caps = &CAPABILITIES;
        match *change {
            Change::Advertise(Some(p)) => {
                if a.initiating.is_some() {
                    excluded(Exclusion::NoAdvertiseWhileInitiating)?;
                }
                let Some(base) = anchor(a).or_else(|| a.scanning.as_ref().map(scan_ns)) else { return Ok(Admitted::AS_ASKED) };
                let period = adv_period(&p);
                if !period.is_multiple_of(base) {
                    return Err(if period < base { SchedKind::NewIntervalFailed } else { SchedKind::IntervalTooLarge });
                }
                if anchor(a).is_some() && free(base, occupied(a, None, true)) < micros(caps.adv_event_len) {
                    return Err(SchedKind::LengthFailed);
                }
                Ok(Admitted::AS_ASKED)
            }
            Change::Advertise(None) | Change::Scan(_) | Change::Initiate(None) | Change::Close { .. } | Change::PeripheralDown => {
                Ok(Admitted::AS_ASKED)
            }
            Change::Initiate(Some(p)) => {
                if a.initiating.is_some() {
                    excluded(Exclusion::NoInitiateWhileInitiating)?;
                }
                if !room_for_central(a) {
                    return Err(SchedKind::NoSlot);
                }
                join(a, &p, None)
            }
            Change::Connected => {
                let p = a.initiating.ok_or(SchedKind::NoSlot)?;
                if !room_for_central(a) {
                    return Err(SchedKind::NoSlot);
                }
                join(a, &p, None)
            }
            Change::Retime { slot, params } => {
                let i = usize::from(slot);
                a.central.get(i).copied().flatten().ok_or(SchedKind::NoSlot)?;
                join(a, &params, Some(i))
            }
            Change::PeripheralUp => {
                let links = a.centrals() + usize::from(a.peripheral);
                if a.peripheral >= caps.max_peripheral || links >= usize::from(caps.max_links) {
                    return Err(SchedKind::NoSlot);
                }
                Ok(Admitted::AS_ASKED)
            }
        }
    }
}

fn excluded(e: Exclusion) -> Result<(), SchedKind> {
    if CAPABILITIES.exclusions.contains(&e) {
        return Err(SchedKind::Excluded(e));
    }
    Ok(())
}

fn room_for_central(a: &Activities) -> bool {
    let caps = &CAPABILITIES;
    a.centrals() < usize::from(caps.max_central) && a.centrals() + usize::from(a.peripheral) < usize::from(caps.max_links)
}

/// The time the slots in `a` take from each period, each with the guard after it; `except` a
/// link being re-timed, advertising left out with `without_adv`.
fn occupied(a: &Activities, except: Option<usize>, without_adv: bool) -> Ns {
    let guard = micros(CAPABILITIES.guard);
    let links: Ns = a.central.iter().enumerate().filter(|&(i, _)| Some(i) != except).filter_map(|(_, s)| *s).map(|s| len_ns(s.ce_len) + guard).sum();
    let adv = a.advertising.filter(|_| !without_adv).map_or(0, |_| micros(CAPABILITIES.adv_event_len) + guard);
    links + adv
}

/// The longest slot that still fits in `period`, with the guard after it.
fn free(period: Ns, occupied: Ns) -> Ns {
    period.saturating_sub(occupied + micros(CAPABILITIES.guard))
}

/// The offset and room for a slot of `want` (§4.2.2), or `None` when the slots already placed overrun `period`.
fn place(a: &Activities, except: Option<usize>, period: Ns, want: Ns) -> Option<(Ns, Ns)> {
    let guard = micros(CAPABILITIES.guard);
    let mut spans = [(0, 0); MAX_LINKS];
    let mut n = 0;
    for (i, s) in a.central.iter().enumerate() {
        if let Some(s) = s.filter(|_| Some(i) != except) {
            let start = micros(s.offset);
            spans[n] = (start, start + len_ns(s.ce_len));
            n += 1;
        }
    }
    let spans = &mut spans[..n];
    spans.sort_unstable();
    if spans.last().is_some_and(|&(_, end)| end + guard > period) {
        return None;
    }
    let mut at = 0;
    for &(start, end) in spans.iter() {
        if want > 0 && at + want + guard <= start {
            return Some((at, want));
        }
        at = end + guard;
    }
    Some((at, period.saturating_sub(at + guard)))
}

/// A link with `p` joining the time base, or link `except` re-timed to `p` (§4.2.1, §4.2.2).
fn join(a: &Activities, p: &ConnParams, except: Option<usize>) -> Result<Admitted, SchedKind> {
    let (min, max) = (interval_ns(p.interval_min), interval_ns(p.interval_max));
    if min > max || min == 0 {
        return Err(SchedKind::NewIntervalFailed);
    }
    let others = a.central.iter().enumerate().any(|(i, s)| s.is_some() && Some(i) != except);
    let base = match except {
        Some(_) if !others && a.advertising.is_none() => None,
        Some(_) => anchor(a),
        None => anchor(a).or_else(|| a.advertising.as_ref().map(adv_period)),
    };
    let used = occupied(a, except, false);
    let (period, interval, latency, scale) = match base {
        None => {
            let first = u64::midpoint(min, max) / UNIT * UNIT;
            (first, first, 1, 1)
        }
        Some(b) if (min..=max).contains(&b) && b % UNIT == 0 => (b, b, 1, 1),
        Some(b) if b < min => {
            let m = (min.div_ceil(b)..=max / b).find(|m| (b * m) % UNIT == 0).ok_or(SchedKind::IntervalTooLarge)?;
            (b, b * m, m, 1)
        }
        Some(b) => {
            let k = (b.div_ceil(max).max(1)..=b / min).find(|&k| b % k == 0 && (b / k) % UNIT == 0).ok_or(SchedKind::NewIntervalFailed)?;
            (b / k, b / k, 1, k)
        }
    };
    let want = len_ns(p.ce_len);
    let (offset, room) = place(a, except, period, want).ok_or(SchedKind::LengthFailed)?;
    let room = room.min(free(period, used));
    if room < want.max(1) || used > period {
        return Err(SchedKind::LengthFailed);
    }
    let granted = if want == 0 { room } else { want };
    let slot = CentralSlot {
        interval: Duration::from_u16(u16::try_from(interval / UNIT).map_err(|_| SchedKind::IntervalTooLarge)?),
        latency: u16::try_from(latency).map_err(|_| SchedKind::IntervalTooLarge)?,
        ce_len: Duration::from_u16(u16::try_from(granted / SLOT).unwrap_or(u16::MAX)),
        offset: Micros(u32::try_from(offset / 1_000).map_err(|_| SchedKind::LengthFailed)?),
    };
    Ok(Admitted { slot: Some(slot), latency_scale: u16::try_from(scale).map_err(|_| SchedKind::NewIntervalFailed)? })
}
