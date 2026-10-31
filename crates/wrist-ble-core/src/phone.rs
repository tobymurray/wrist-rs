//! The phone's link, with this device as the peripheral.

use wrist_ble::{Instant, ConnHandle, Millis, PairingStep};

/// A link left unencrypted this long is asked to pair: well past a bonded central's own
/// encryption start (HARDWARE.md).
pub const ASK_TO_PAIR_AFTER: Millis = Millis::secs(5);

/// A link neither encrypted nor pairing may be asked for parameters after this long; never at
/// connect, where it ended a bonded macOS central's link (HARDWARE.md).
pub const ASK_PARAMS_AFTER: Millis = Millis(1_500);

/// A numeric comparison the user has not answered is rejected after this long.
pub const NUMERIC_DEADLINE: Millis = Millis::secs(60);

/// A pairing with no outcome this long after its comparison was answered or timed out has failed:
/// the Security Manager's own timeout (Core Vol 3 Part H §3.4).
pub const PAIRING_OUTCOME: Millis = Millis::secs(30);

/// No central start this soon after the phone connects or its parameters change.
pub const SETTLE: Millis = Millis::secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Phone {
    /// The radio is off.
    #[default]
    Off,
    /// No phone link.
    Discoverable,
    Up(PhoneLink),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhoneLink {
    pub link: ConnHandle,
    pub since: Instant,
    pub security: Security,
    /// When the link's parameters last changed, or when it connected.
    pub params_at: Instant,
    ask_pair: Option<Instant>,
    params: Params,
    bond_lost: bool,
    pairing_failed: bool,
    told_bond_lost: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Security {
    Unsecured,
    Pairing { value: u32, deadline: Instant, answered: bool },
    Secure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Params {
    Until(Instant),
    Pairing,
    Open,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    RadioOn,
    RadioOff,
    LinkUp(ConnHandle),
    LinkDown(ConnHandle),
    Encryption(ConnHandle, bool),
    Pairing(ConnHandle, PairingStep),
    BondLost(ConnHandle),
    ParamsChanged(ConnHandle),
    /// The user's answer to a numeric comparison.
    Answer(bool),
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    RequestSecurity(ConnHandle),
    ConfirmNumeric(ConnHandle, bool),
    AllowRebond(ConnHandle),
    /// Tell the user the phone lost its bond: once a link, whichever of the chip's bond-lost
    /// event and the failed pairing came first.
    TellBondLost(ConnHandle),
}

/// At most two actions from one step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Actions([Option<Action>; 2]);

impl Actions {
    fn push(&mut self, a: Action) {
        if let Some(slot) = self.0.iter_mut().find(|s| s.is_none()) {
            *slot = Some(a);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Action> + '_ {
        self.0.iter().flatten().copied()
    }
}

impl Phone {
    #[must_use]
    pub fn step(self, now: Instant, event: Event) -> (Self, Actions) {
        let mut out = Actions::default();
        let next = match (self, event) {
            (_, Event::RadioOff) => Phone::Off,
            (Phone::Off, Event::RadioOn) => Phone::Discoverable,
            (Phone::Discoverable, Event::LinkUp(link)) => Phone::Up(PhoneLink {
                link,
                since: now,
                security: Security::Unsecured,
                params_at: now,
                ask_pair: Some(now + ASK_TO_PAIR_AFTER),
                params: Params::Until(now + ASK_PARAMS_AFTER),
                bond_lost: false,
                pairing_failed: false,
                told_bond_lost: false,
            }),
            (Phone::Up(l), Event::LinkDown(id)) if l.link == id => Phone::Discoverable,
            (Phone::Up(mut l), e) => {
                l.on(now, e, &mut out);
                Phone::Up(l)
            }
            (p, _) => p,
        };
        (next, out)
    }

    /// When the machine next acts by time alone.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let Phone::Up(l) = self else { return None };
        let numeric = match l.security {
            Security::Pairing { deadline, answered: false, .. } => Some(deadline),
            Security::Pairing { deadline, answered: true, .. } => Some(deadline + PAIRING_OUTCOME),
            _ => None,
        };
        let params = match l.params {
            Params::Until(at) => Some(at),
            _ => None,
        };
        [l.ask_pair, numeric, params].into_iter().flatten().min()
    }

    /// Whether this device may ask the phone for new parameters now.
    #[must_use]
    pub fn may_ask_params(&self, now: Instant) -> bool {
        matches!(self, Phone::Up(l) if l.params == Params::Open || matches!(l.params, Params::Until(at) if now >= at))
    }

    /// No central start before this.
    #[must_use]
    pub fn settled_at(&self) -> Option<Instant> {
        match self {
            Phone::Up(l) => Some(l.params_at + SETTLE),
            _ => None,
        }
    }

    #[must_use]
    pub fn link(&self) -> Option<ConnHandle> {
        match self {
            Phone::Up(l) => Some(l.link),
            _ => None,
        }
    }
}

impl PhoneLink {
    fn on(&mut self, now: Instant, event: Event, out: &mut Actions) {
        let link = self.link;
        match event {
            Event::Encryption(id, on) if id == link => {
                if on {
                    self.security = Security::Secure;
                    self.ask_pair = None;
                    self.params = Params::Open;
                } else {
                    self.security = Security::Unsecured;
                }
            }
            Event::Pairing(id, step) if id == link => match step {
                PairingStep::Numeric(value) => {
                    self.security = Security::Pairing { value, deadline: now + NUMERIC_DEADLINE, answered: false };
                    self.ask_pair = None;
                    if matches!(self.params, Params::Until(_)) {
                        self.params = Params::Pairing;
                    }
                }
                PairingStep::Complete | PairingStep::PasskeyWanted => self.ask_pair = None,
                PairingStep::Failed => {
                    self.security = Security::Unsecured;
                    self.ask_pair = None;
                    self.params = Params::Open;
                    self.pairing_failed = true;
                    self.tell_bond_lost(out);
                }
            },
            Event::BondLost(id) if id == link => {
                self.bond_lost = true;
                out.push(Action::AllowRebond(link));
                self.tell_bond_lost(out);
            }
            Event::ParamsChanged(id) if id == link => self.params_at = now,
            Event::Answer(yes) => {
                if let Security::Pairing { answered: ref mut a @ false, .. } = self.security {
                    *a = true;
                    out.push(Action::ConfirmNumeric(link, yes));
                }
            }
            Event::Tick => {
                if let Security::Pairing { deadline, answered: ref mut a @ false, .. } = self.security {
                    if now >= deadline {
                        *a = true;
                        out.push(Action::ConfirmNumeric(link, false));
                    }
                }
                if matches!(self.security, Security::Pairing { deadline, answered: true, .. } if now >= deadline + PAIRING_OUTCOME) {
                    self.security = Security::Unsecured;
                    self.params = Params::Open;
                }
                if self.ask_pair.is_some_and(|at| now >= at) {
                    self.ask_pair = None;
                    out.push(Action::RequestSecurity(link));
                }
                if matches!(self.params, Params::Until(at) if now >= at) {
                    self.params = Params::Open;
                }
            }
            _ => {}
        }
    }

    fn tell_bond_lost(&mut self, out: &mut Actions) {
        if self.bond_lost && self.pairing_failed && !self.told_bond_lost {
            self.told_bond_lost = true;
            out.push(Action::TellBondLost(self.link));
        }
    }
}
