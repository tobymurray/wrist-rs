//! One chosen peripheral, with this device as the central, whose stage the reconciler works towards.

use wrist_ble::{FailureKind, Status, Instant, ConnHandle, Millis, PeerAddr};

use crate::goals::Power;
use crate::plan::Conflict;
use crate::profile::{Cadence, Profile, Reading, SecurityNeed, Silence};

/// With no reading for this long the core stops looking, keyed on readings and not on the
/// link's life, so a sensor still delivering most of its readings is kept.
pub const GIVE_UP: Millis = Millis::mins(5);

/// A link that fails as it is set up (HCI 0x3E) is retried at once this many times, with no
/// backoff and no count towards the give-up's reason.
pub const QUICK_RETRIES: u8 = 3;

/// The wait after each failed attempt; on a cable the last step repeats.
pub const BACKOFF: [Millis; 6] = [Millis::secs(1), Millis::secs(2), Millis::secs(4), Millis::secs(8), Millis::secs(15), Millis::secs(30)];

/// On battery the backoff grows to this, jitter included.
pub const BATTERY_BACKOFF_CAP: Millis = Millis::secs(60);

/// How long one attempt to connect listens for the peer before it counts as failed.
#[must_use]
pub const fn attempt_window(power: Power) -> Millis {
    match power {
        Power::Cable => Millis::secs(10),
        Power::Battery => Millis::secs(20),
    }
}

/// A device without the service is let go and looked at again this much later, since a service
/// can appear after the device starts.
pub const REDISCOVER_AFTER: Millis = Millis::secs(10);

/// Looks for the service after the first, before the device is called incompatible.
pub const REDISCOVERIES: u8 = 3;

/// In a workout, a peripheral not found keeps being looked for, a short window at a time.
pub const SLOW_WINDOW: Millis = Millis::secs(3);
pub const SLOW_EVERY: Millis = Millis::secs(60);
pub const SLOW_FOR: Millis = Millis::mins(30);

/// Drops and quiet spells are counted over this window.
pub const UNSTABLE_WINDOW: Millis = Millis::mins(3);

/// This many drops or quiet spells in the window make a peripheral unstable.
pub const UNSTABLE_EVENTS: usize = 2;

/// Below this share of the expected readings over the window a peripheral is unstable.
pub const COVERAGE_PERCENT: u32 = 90;

/// Coverage is judged on whole blocks of this length, so it changes at most once a block.
pub const COVERAGE_BLOCK: Millis = Millis::secs(10);

const BLOCKS_U32: u32 = UNSTABLE_WINDOW.0 / COVERAGE_BLOCK.0;
const BLOCKS: usize = BLOCKS_U32 as usize;

/// Coverage is not judged on fewer whole blocks than this.
const COVERAGE_MIN_BLOCKS: u32 = 3;

/// What else the machine needs to know, none of it its own state.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub profile: &'a Profile,
    pub peer: PeerAddr,
    pub power: Power,
    pub workout: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stage {
    #[default]
    Off,
    /// Connecting to the peer; `quick` counts the immediate retries used.
    Seeking { since: Instant, quick: u8 },
    Backoff { until: Instant },
    /// Between slow-search windows.
    SlowWait { until: Instant },
    Linked { link: ConnHandle, phase: Phase, quick: u8, linked_before: bool },
    Live { link: ConnHandle, since: Instant },
    Dropping { link: ConnHandle, then: Then },
    /// Let go for lacking the service; looked at again at `until`.
    Waiting { until: Instant },
    Stopped(StopReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Securing,
    Discovering,
    Subscribing,
}

/// Where a link being dropped leads once it is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Then {
    Off,
    Reconnect,
    Rediscover,
    Stop(StopReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StopReason {
    NotFound,
    /// Connected since its last reading, but no reading came.
    KeptDropping,
    /// The service was absent after every rediscovery.
    Incompatible,
    PairingFailed,
    /// The planner could not fit it; it resumes when the conflict ends.
    RadioBusy(Conflict),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    Want,
    Unwant,
    /// A tap, a workout starting, or the goal asserted again.
    Retry,
    LinkUp(ConnHandle),
    ConnectFailed(FailureKind),
    LinkDown(ConnHandle, Status),
    Secured(bool),
    /// Discovery ended: whether the profile's service and characteristics were found.
    Discovered(bool),
    Subscribed(bool),
    /// A procedure on the link failed or timed out.
    ProcedureFailed,
    Reading(Reading),
    Busy(Conflict),
    Free,
    Tick,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Peripheral {
    pub stage: Stage,
    pub chosen_at: Instant,
    /// The last reading, or when the peripheral was chosen or retried: the give-up's clock.
    pub heard_at: Instant,
    pub last: Option<(Instant, Reading)>,
    /// When the link was last lost, until the next reading.
    pub lost_since: Option<Instant>,
    /// Connection attempts since it was chosen.
    pub attempts: u16,
    /// Searching slowly until then.
    pub slow_until: Option<Instant>,
    linked_since_heard: bool,
    rediscoveries: u8,
    backoff: u8,
    trouble: [Option<Instant>; 4],
    coverage: Coverage,
}

impl Peripheral {
    #[must_use]
    pub fn step(mut self, ctx: &Ctx<'_>, now: Instant, event: Event) -> Self {
        match (self.stage, event) {
            (Stage::Linked { link, .. } | Stage::Live { link, .. } | Stage::Dropping { link, .. }, Event::Unwant) => {
                self.drop_link(link, Then::Off);
            }
            (_, Event::Unwant) => self.stage = Stage::Off,
            (Stage::Off, Event::Want) | (Stage::Stopped(_), Event::Retry) => self.fresh(now),
            (Stage::Stopped(StopReason::RadioBusy(_)), Event::Free) => {
                self.heard_at = now;
                self.slow_until = None;
                self.seek(now, 0);
            }
            (Stage::Off | Stage::Stopped(_), _) | (_, Event::Want | Event::Retry | Event::Free) | (Stage::Dropping { .. }, Event::Busy(_)) => {}
            (Stage::Linked { link, .. } | Stage::Live { link, .. }, Event::Busy(c)) => self.drop_link(link, Then::Stop(StopReason::RadioBusy(c))),
            (_, Event::Busy(c)) => self.stage = Stage::Stopped(StopReason::RadioBusy(c)),
            (Stage::Seeking { quick, .. }, Event::LinkUp(link)) => {
                let phase = match ctx.profile.security {
                    SecurityNeed::None => Phase::Discovering,
                    SecurityNeed::Encrypted => Phase::Securing,
                };
                self.stage = Stage::Linked { link, phase, quick, linked_before: self.linked_since_heard };
                self.linked_since_heard = true;
            }
            (Stage::Seeking { quick, .. }, Event::ConnectFailed(FailureKind::NotEstablished)) if quick < QUICK_RETRIES => {
                self.seek(now, quick + 1);
            }
            (Stage::Seeking { .. }, Event::ConnectFailed(_)) => self.failed(ctx, now),
            (Stage::Linked { link, quick, linked_before, .. }, Event::LinkDown(id, reason)) if id == link => {
                if reason == Status::CONN_FAILED_SYNCHRONIZATION_TIMEOUT {
                    self.linked_since_heard = linked_before;
                    if quick < QUICK_RETRIES {
                        self.seek(now, quick + 1);
                    } else {
                        self.failed(ctx, now);
                    }
                } else {
                    self.lost(now);
                }
            }
            (Stage::Live { link, .. }, Event::LinkDown(id, _)) if id == link => self.lost(now),
            (Stage::Dropping { link, then }, Event::LinkDown(id, _)) if id == link => match then {
                Then::Off => self.stage = Stage::Off,
                Then::Reconnect => self.seek(now, 0),
                Then::Rediscover => self.stage = Stage::Waiting { until: now + REDISCOVER_AFTER },
                Then::Stop(r) => self.stage = Stage::Stopped(r),
            },
            (Stage::Linked { link, phase: Phase::Securing, quick, linked_before }, Event::Secured(ok)) => {
                if ok {
                    self.stage = Stage::Linked { link, phase: Phase::Discovering, quick, linked_before };
                } else {
                    self.drop_link(link, Then::Stop(StopReason::PairingFailed));
                }
            }
            (Stage::Linked { link, phase: Phase::Discovering, quick, linked_before }, Event::Discovered(found)) => {
                if found {
                    self.stage = Stage::Linked { link, phase: Phase::Subscribing, quick, linked_before };
                } else {
                    self.missing(now, link);
                }
            }
            (Stage::Linked { link, phase: Phase::Subscribing, .. }, Event::Subscribed(ok)) => {
                if ok {
                    self.stage = Stage::Live { link, since: now };
                    self.backoff = 0;
                    self.rediscoveries = 0;
                    self.coverage.begin(now);
                } else {
                    self.missing(now, link);
                }
            }
            (Stage::Linked { link, .. }, Event::ProcedureFailed) => {
                self.lost_since.get_or_insert(now);
                self.drop_link(link, Then::Reconnect);
            }
            (Stage::Live { since, .. }, Event::Reading(r)) => {
                let previous = self.last.map_or(since, |(at, _)| at.max(since));
                if ctx.profile.silence == Silence::Fault && now.since(previous) > ctx.profile.quiet {
                    self.trouble(now);
                }
                self.last = Some((now, r));
                self.heard_at = now;
                self.lost_since = None;
                self.linked_since_heard = false;
                self.slow_until = None;
                self.coverage.record(now);
            }
            (_, Event::Tick) => self.tick(ctx, now),
            _ => {}
        }
        self
    }

    /// When the machine's stage or its status next changes by time alone.
    #[must_use]
    pub fn deadline(&self, ctx: &Ctx<'_>, now: Instant) -> Option<Instant> {
        let p = ctx.profile;
        let give_up = self.give_up_applies(p).then(|| self.slow_until.filter(|_| ctx.workout).unwrap_or(self.heard_at + GIVE_UP));
        let stage = match self.stage {
            Stage::Seeking { since, .. } => Some(since + self.window(ctx)),
            Stage::Backoff { until } | Stage::SlowWait { until } | Stage::Waiting { until } => Some(until),
            Stage::Live { since, .. } => {
                let heard = self.last.map_or(since, |(at, _)| at.max(since));
                let stale = (p.silence == Silence::Fault).then_some(heard + p.stale);
                let marks = [heard + fresh_for(p), heard + p.quiet, heard + crate::status::HIDE_AFTER].map(|m| m + Millis(1));
                let trouble = self.trouble.iter().flatten().map(|&t| t + UNSTABLE_WINDOW);
                let block = match (p.silence, p.cadence) {
                    (Silence::Fault, Cadence::Every(_)) => self.coverage.next_block(now),
                    _ => None,
                };
                let status = marks.into_iter().chain(trouble).chain(block).filter(|&t| t > now).min();
                [stale, status].into_iter().flatten().min()
            }
            _ => None,
        };
        [give_up, stage].into_iter().flatten().min()
    }

    /// Drops and quiet spells within the window, with an ongoing spell counted if `ongoing`.
    #[must_use]
    pub fn unstable(&self, profile: &Profile, now: Instant, ongoing: bool) -> bool {
        let events = self.trouble.iter().flatten().filter(|&&t| now.since(t) < UNSTABLE_WINDOW).count() + usize::from(ongoing);
        let thin = match (profile.silence, profile.cadence) {
            (Silence::Fault, Cadence::Every(period)) => self.coverage.low(now, period),
            _ => false,
        };
        events >= UNSTABLE_EVENTS || thin
    }

    fn fresh(&mut self, now: Instant) {
        *self = Self { chosen_at: now, heard_at: now, attempts: 0, ..Self::default() };
        self.seek(now, 0);
    }

    fn seek(&mut self, now: Instant, quick: u8) {
        self.attempts = self.attempts.saturating_add(1);
        self.stage = Stage::Seeking { since: now, quick };
    }

    fn window(&self, ctx: &Ctx<'_>) -> Millis {
        if self.slow_until.is_some() {
            SLOW_WINDOW
        } else {
            attempt_window(ctx.power)
        }
    }

    fn failed(&mut self, ctx: &Ctx<'_>, now: Instant) {
        if self.slow_until.is_some() {
            self.stage = Stage::SlowWait { until: now + Millis(SLOW_EVERY.0 - SLOW_WINDOW.0) };
            return;
        }
        let step = usize::from(self.backoff);
        let base = match ctx.power {
            Power::Battery if step >= BACKOFF.len() => BATTERY_BACKOFF_CAP,
            _ => BACKOFF[step.min(BACKOFF.len() - 1)],
        };
        let mut wait = base + jitter(ctx.peer, self.backoff, base);
        if ctx.power == Power::Battery {
            wait = wait.min(BATTERY_BACKOFF_CAP);
        }
        self.backoff = self.backoff.saturating_add(1);
        self.stage = Stage::Backoff { until: now + wait };
    }

    fn lost(&mut self, now: Instant) {
        self.trouble(now);
        self.lost_since.get_or_insert(now);
        self.seek(now, 0);
    }

    fn missing(&mut self, now: Instant, link: ConnHandle) {
        self.lost_since.get_or_insert(now);
        self.rediscoveries = self.rediscoveries.saturating_add(1);
        let then = if self.rediscoveries > REDISCOVERIES { Then::Stop(StopReason::Incompatible) } else { Then::Rediscover };
        self.drop_link(link, then);
    }

    fn drop_link(&mut self, link: ConnHandle, then: Then) {
        self.stage = Stage::Dropping { link, then };
    }

    fn trouble(&mut self, now: Instant) {
        let oldest = self.trouble.iter_mut().min_by_key(|t| t.map_or(0, |i| i.0 + 1));
        if let Some(slot) = oldest {
            *slot = Some(now);
        }
    }

    /// A peripheral that may be idle by nature is not given up on while subscribed.
    fn give_up_applies(&self, p: &Profile) -> bool {
        match self.stage {
            Stage::Off | Stage::Stopped(_) | Stage::Dropping { .. } => false,
            Stage::Live { .. } => p.silence == Silence::Fault,
            _ => true,
        }
    }

    fn tick(&mut self, ctx: &Ctx<'_>, now: Instant) {
        let p = ctx.profile;
        if self.give_up_applies(p) && now >= self.heard_at + GIVE_UP {
            let reason = if self.linked_since_heard { StopReason::KeptDropping } else { StopReason::NotFound };
            let slow = ctx.workout && (reason == StopReason::NotFound || self.slow_until.is_some());
            match self.slow_until {
                None if slow => {
                    self.slow_until = Some(now + SLOW_FOR);
                    if let Stage::Backoff { .. } = self.stage {
                        self.seek(now, 0);
                    }
                    return;
                }
                Some(until) if slow && now < until => {}
                _ => return self.stop(reason),
            }
        }
        match self.stage {
            Stage::Seeking { since, .. } if now >= since + self.window(ctx) => self.failed(ctx, now),
            Stage::Backoff { until } | Stage::SlowWait { until } | Stage::Waiting { until } if now >= until => self.seek(now, 0),
            Stage::Live { link, since } if p.silence == Silence::Fault => {
                let heard = self.last.map_or(since, |(at, _)| at.max(since));
                if now >= heard + p.stale {
                    self.trouble(now);
                    self.lost_since.get_or_insert(now);
                    self.drop_link(link, Then::Reconnect);
                }
            }
            _ => {}
        }
    }

    fn stop(&mut self, reason: StopReason) {
        self.slow_until = None;
        match self.stage {
            Stage::Linked { link, .. } | Stage::Live { link, .. } => self.drop_link(link, Then::Stop(reason)),
            _ => self.stage = Stage::Stopped(reason),
        }
    }
}

/// A reading older than this is never shown as current: twice the cadence, at most
/// [`crate::status::CURRENT_FOR`].
#[must_use]
pub fn fresh_for(p: &Profile) -> Millis {
    match p.cadence {
        Cadence::Every(period) => Millis(period.0.saturating_mul(2)).min(crate::status::CURRENT_FOR),
        Cadence::Events => crate::status::CURRENT_FOR,
    }
}

/// Up to a quarter of `base`, fixed per peer and step so that peers do not retry in step.
fn jitter(peer: PeerAddr, step: u8, base: Millis) -> Millis {
    let mut h: u32 = 0x811c_9dc5;
    for &b in peer.addr.raw().iter().chain(&[step]) {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    Millis(h % (base.0 / 4 + 1))
}

/// Readings in whole blocks over the unstable window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Coverage {
    start: Option<Instant>,
    counts: [u8; BLOCKS],
    newest: u32,
}

impl Coverage {
    fn begin(&mut self, now: Instant) {
        if self.start.is_none() {
            *self = Self { start: Some(now), ..Self::default() };
        }
    }

    fn block(start: Instant, at: Instant) -> u32 {
        at.since(start).0 / COVERAGE_BLOCK.0
    }

    fn record(&mut self, now: Instant) {
        let Some(start) = self.start else { return };
        let b = Self::block(start, now);
        if b > self.newest {
            let from = (self.newest + 1).max(b.saturating_sub(BLOCKS_U32 - 1));
            for i in from..=b {
                self.counts[i as usize % BLOCKS] = 0;
            }
            self.newest = b;
        }
        let c = &mut self.counts[b as usize % BLOCKS];
        *c = c.saturating_add(1);
    }

    fn next_block(&self, now: Instant) -> Option<Instant> {
        self.start.map(|start| start + Millis((Self::block(start, now) + 1) * COVERAGE_BLOCK.0))
    }

    fn low(&self, now: Instant, period: Millis) -> bool {
        let Some(start) = self.start else { return false };
        let current = Self::block(start, now);
        let first = current.saturating_sub(BLOCKS_U32);
        let blocks = current - first;
        if blocks < COVERAGE_MIN_BLOCKS || period.0 == 0 {
            return false;
        }
        let got: u32 = (first..current)
            .filter(|&i| i <= self.newest && self.newest - i < BLOCKS_U32)
            .map(|i| u32::from(self.counts[i as usize % BLOCKS]))
            .sum();
        let expected = blocks * (COVERAGE_BLOCK.0 / period.0);
        got * 100 < expected * COVERAGE_PERCENT
    }
}
