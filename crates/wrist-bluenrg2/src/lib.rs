#![no_std]

//! The BlueNRG-2 as the Bluetooth core sees it: what it can do, the link-layer scheduler ST
//! documents in PM0257 Rev 4 §4, the codes it refuses with, and the timings that fit it.

use wrist_ble::{
    Activities, AdvParams, Admitted, Capabilities, CentralSlot, Change, ConnParams, Duration, Exclusion, GattLimits, Interval, Micros,
    Millis, OpKind, ParamAnswer, RoleSet, SchedKind, Schedule, ScanParams, Slots,
};
use wrist_ble_core::goals::Power;
use wrist_ble_core::plan::{Choice, Menu, When};

pub const CAPABILITIES: Capabilities = Capabilities {
    max_links: 8,
    max_central: 8,
    // Two centrals at once, measured.
    max_peripheral: 2,
    roles_concurrent: RoleSet::ALL,
    adv_event_len: Micros(14_600),
    guard: Micros(1_500),
    discovery_max: Some(Millis(10_200)),
    peer_param_requests: ParamAnswer::RejectOnly,
    gatt: GattLimits { services: 8, attributes: 68, mtu: 220 },
    op_deadline,
    exclusions: &[Exclusion::NoAdvertiseWhileInitiating],
    encryption_quiet: Millis(1_500),
};

/// The latest each kind of operation ends: a command's reply, or the procedure on air with the
/// Core Specification's own timeouts (ATT 30 s, Vol 3 Part F §3.3.3; SMP 30 s, Vol 3 Part H
/// §3.4).
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

/// The interval the chip advertises at: the mean of the two bounds, each plus 5 ms (§4.2.3).
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

/// PM0257 §4.2's slot allocator, with what the watch measured where ST is silent.
///
/// - A central link joins the time base by §4.2.2: the anchor period itself when the range holds
///   it, a multiple `m` with slot latency `m`, or the period divided by `k`, a multiple of
///   1.25 ms holding every slot, the other links' latency then multiplied by `k`.
/// - Slots follow each other with the guard between and after them; a link gets its connection
///   event length if it fits, and refuses with `LengthFailed` if not.
/// - With no central link, running advertising or a scan sets the period a first link must
///   fit: the stack picked 35 ms from 30 to 50 ms beside advertising every 105 ms (HARDWARE.md).
///   Otherwise the first link starts the time base at the mean of its range (§4.2.1).
/// - Advertising beside a central link must be a whole multiple of the anchor period (§4.2.3),
///   with room for its 14.6 ms event. A re-timed link keeps the anchor it holds unless nothing
///   else runs: measured, 35 to 500 ms was refused beside advertising every 980 ms, and taken
///   with advertising stopped.
/// - A scan adapts to whatever room is left (§4.2.4) and is never refused.
///
/// UNVERIFIED, for the probe matrix to settle: a connection event length of 0 taking all the
/// room left, which would explain a refusal measured beside a fresh 35 ms link but not an updated one, and advertising shorter than the anchor period refused
/// with `NewIntervalFailed`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scheduler;

impl Schedule for Scheduler {
    fn admit(&self, a: &Activities, change: &Change) -> Result<Admitted, SchedKind> {
        let caps = &CAPABILITIES;
        match *change {
            Change::Advertise(Some(p)) => {
                if a.initiating.is_some() {
                    return Err(SchedKind::Excluded(Exclusion::NoAdvertiseWhileInitiating));
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
                if a.initiating.is_some() || !room_for_central(a) {
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

/// A link with `p` joining the time base, or link `except` re-timed to `p` (§4.2.1, §4.2.2).
fn join(a: &Activities, p: &ConnParams, except: Option<usize>) -> Result<Admitted, SchedKind> {
    let (min, max) = (interval_ns(p.interval_min), interval_ns(p.interval_max));
    if min > max || min == 0 {
        return Err(SchedKind::NewIntervalFailed);
    }
    let others = a.central.iter().enumerate().any(|(i, s)| s.is_some() && Some(i) != except);
    let base = match except {
        Some(_) if !others && a.advertising.is_none() && a.scanning.is_none() => None,
        Some(_) => anchor(a),
        None => anchor(a).or_else(|| a.scanning.as_ref().map(scan_ns)).or_else(|| a.advertising.as_ref().map(adv_period)),
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
    let room = free(period, used);
    let want = len_ns(p.ce_len);
    if room < want.max(1) || used > period {
        return Err(SchedKind::LengthFailed);
    }
    let granted = if want == 0 { room } else { want };
    let slot = CentralSlot {
        interval: Duration::from_u16(u16::try_from(interval / UNIT).map_err(|_| SchedKind::IntervalTooLarge)?),
        latency: u16::try_from(latency).map_err(|_| SchedKind::IntervalTooLarge)?,
        ce_len: Duration::from_u16(u16::try_from(granted / SLOT).unwrap_or(u16::MAX)),
    };
    Ok(Admitted { slot: Some(slot), latency_scale: u8::try_from(scale).map_err(|_| SchedKind::NewIntervalFailed)? })
}

const fn adv(ms_x8: u16) -> AdvParams {
    AdvParams { interval_min: Duration::from_u16(ms_x8), interval_max: Duration::from_u16(ms_x8), connectable: true }
}

const fn link(min: u16, max: u16, latency: u16, timeout: u16) -> ConnParams {
    ConnParams { interval_min: Duration::from_u16(min), interval_max: Duration::from_u16(max), latency, timeout: Duration::from_u16(timeout), ce_len: Duration::from_u16(4) }
}

const fn when(power: Option<Power>, beside_central: Option<bool>, idle: Option<bool>) -> When {
    When { power, beside_central, idle }
}

/// The timings that fit this chip's scheduler, most preferred first in each situation: every
/// advertising interval plus 5 ms is a whole multiple of the 50 and 500 ms central links (§4.3,
/// guidelines 2b and 4b).
pub const MENU: Menu = Menu {
    advertising: &[
        Choice { params: adv(160), when: when(Some(Power::Cable), Some(false), Some(false)), cost_ua: 150 },
        Choice { params: adv(792), when: when(Some(Power::Cable), None, None), cost_ua: 30 },
        Choice { params: adv(1592), when: when(None, None, Some(false)), cost_ua: 15 },
        Choice { params: adv(3992), when: when(None, None, None), cost_ua: 6 },
    ],
    connecting: &[Choice { params: link(40, 40, 0, 400), when: when(None, None, None), cost_ua: 60 }],
    live: &[
        Choice { params: link(400, 400, 0, 600), when: when(None, None, None), cost_ua: 7 },
        Choice { params: link(200, 200, 0, 600), when: when(None, None, None), cost_ua: 14 },
    ],
    phone: &[
        Choice { params: link(12, 24, 0, 500), when: when(Some(Power::Cable), None, None), cost_ua: 120 },
        Choice { params: link(80, 160, 4, 600), when: when(Some(Power::Battery), None, None), cost_ua: 8 },
    ],
};
