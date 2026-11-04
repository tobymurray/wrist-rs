use crate::{AdvParams, ConnParams, Interval, Micros, Millis, OpKind, ScanParams, Slots, MAX_LINKS};

/// What a chip can do, declared by its driver; fixed once the chip has started.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub max_links: u8,
    pub max_central: u8,
    pub max_peripheral: u8,
    /// The roles the chip runs at once.
    pub roles_concurrent: RoleSet,
    /// One advertising event, which no payload shortens.
    pub adv_event_len: Micros,
    /// Kept between central slots.
    pub guard: Micros,
    /// A general discovery the chip ends by itself after this long.
    pub discovery_max: Option<Millis>,
    pub peer_param_requests: ParamAnswer,
    pub gatt: GattLimits,
    /// The latest an accepted operation of each kind reaches its terminal outcome.
    pub op_deadline: fn(OpKind) -> Millis,
    pub exclusions: &'static [Exclusion],
    /// No link-layer procedure on a link this soon after it connects unless it has encrypted.
    pub encryption_quiet: Millis,
}

/// A set of radio roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoleSet(pub u8);

impl RoleSet {
    pub const ADVERTISE: Self = Self(0x01);
    pub const SCAN: Self = Self(0x02);
    pub const CENTRAL: Self = Self(0x04);
    pub const PERIPHERAL: Self = Self(0x08);
    pub const ALL: Self = Self(0x0F);

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// How the chip answers a peer's connection parameter request as the central.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamAnswer {
    Either,
    /// Every answer goes on air as a refusal; only the central's own update moves the link.
    RejectOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GattLimits {
    pub services: u8,
    pub attributes: u8,
    pub mtu: u16,
}

/// A combination the chip refuses, beyond its scheduler's arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exclusion {
    NoAdvertiseWhileInitiating,
    NoScanWhileInitiating,
}

/// A pure model of the chip's scheduler, shared by the planner, the fake chip and the probe
/// fixture's check.
pub trait Schedule {
    /// Whether the chip would accept `change` beside `current`, and what it would then set.
    ///
    /// # Errors
    ///
    /// The refusal the chip would give.
    fn admit(&self, current: &Activities, change: &Change) -> Result<Admitted, SchedKind>;
}

/// The radio's activities at one moment, as the scheduler sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Activities {
    pub advertising: Option<AdvParams>,
    pub scanning: Option<ScanParams>,
    pub initiating: Option<ConnParams>,
    pub central: [Option<CentralSlot>; MAX_LINKS],
    /// Links where this device is the peripheral, which run outside the central time base.
    pub peripheral: u8,
}

/// A central link's place in the time base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CentralSlot {
    pub interval: Interval,
    /// The link has an event once in this many periods of the time base.
    pub latency: u16,
    pub ce_len: Slots,
}

/// One step from one [`Activities`] to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    Advertise(Option<AdvParams>),
    Scan(Option<ScanParams>),
    Initiate(Option<ConnParams>),
    /// The pending initiation became a link, in the slot [`Admitted::slot`] gives.
    Connected,
    Retime { slot: u8, params: ConnParams },
    Close { slot: u8 },
    PeripheralUp,
    PeripheralDown,
}

/// What the chip sets when it accepts a [`Change`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Admitted {
    /// For `Initiate`, `Connected` and `Retime`: the slot the link takes.
    pub slot: Option<CentralSlot>,
    /// The other central links' latency is multiplied by this when a shorter interval divides
    /// the anchor period (PM0257 §4.2.2); 1 otherwise.
    pub latency_scale: u8,
}

impl Admitted {
    /// For a change that takes no slot.
    pub const AS_ASKED: Self = Self { slot: None, latency_scale: 1 };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchedKind {
    /// No anchor-compatible interval in the range asked.
    NewIntervalFailed,
    IntervalTooLarge,
    /// The connection event length does not fit beside the other slots.
    LengthFailed,
    Excluded(Exclusion),
    /// Every link of that role is taken.
    NoSlot,
}

impl Activities {
    pub const IDLE: Self = Self { advertising: None, scanning: None, initiating: None, central: [None; MAX_LINKS], peripheral: 0 };

    #[must_use]
    pub fn centrals(&self) -> usize {
        self.central.iter().flatten().count()
    }

    /// The activities after `change`, as the chip set them when it admitted it.
    ///
    /// # Errors
    ///
    /// [`SchedKind::NoSlot`] for a link the admission gave no slot, or one with nowhere to go.
    pub fn with(&self, change: &Change, admitted: &Admitted) -> Result<Self, SchedKind> {
        let mut next = *self;
        let scale = |next: &mut Self, except: Option<usize>| {
            for (i, slot) in next.central.iter_mut().enumerate() {
                if let Some(s) = slot.as_mut().filter(|_| Some(i) != except) {
                    s.latency = s.latency.saturating_mul(u16::from(admitted.latency_scale));
                }
            }
        };
        match *change {
            Change::Advertise(p) => next.advertising = p,
            Change::Scan(p) => next.scanning = p,
            Change::Initiate(p) => next.initiating = p,
            Change::Connected => {
                let slot = admitted.slot.ok_or(SchedKind::NoSlot)?;
                let free = next.central.iter().position(Option::is_none).ok_or(SchedKind::NoSlot)?;
                next.initiating = None;
                scale(&mut next, None);
                next.central[free] = Some(slot);
            }
            Change::Retime { slot, .. } => {
                let granted = admitted.slot.ok_or(SchedKind::NoSlot)?;
                let i = usize::from(slot);
                if next.central.get(i).copied().flatten().is_none() {
                    return Err(SchedKind::NoSlot);
                }
                scale(&mut next, Some(i));
                next.central[i] = Some(granted);
            }
            Change::Close { slot } => {
                if let Some(s) = next.central.get_mut(usize::from(slot)) {
                    *s = None;
                }
            }
            Change::PeripheralUp => {
                // A connection ends advertising (PM0257 §4.2.3).
                next.advertising = None;
                next.peripheral = next.peripheral.saturating_add(1);
            }
            Change::PeripheralDown => next.peripheral = next.peripheral.saturating_sub(1),
        }
        Ok(next)
    }

    /// Apply `steps` in order, each admitted by `schedule` beside the state before it.
    ///
    /// # Errors
    ///
    /// The index of the first step refused, and the refusal.
    pub fn walk(&self, schedule: &dyn Schedule, steps: &[Change]) -> Result<Self, (usize, SchedKind)> {
        steps.iter().enumerate().try_fold(*self, |at, (i, step)| {
            let admitted = schedule.admit(&at, step).map_err(|e| (i, e))?;
            at.with(step, &admitted).map_err(|e| (i, e))
        })
    }
}
