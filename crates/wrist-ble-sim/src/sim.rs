//! The core against the fake chip on virtual time, every step checked.

use std::collections::VecDeque;

use wrist_ble::{Instant, Millis, RadioEvent};
use wrist_ble_core::chip::Action;
use wrist_ble_core::engine::{Core, Input, Outputs, Tell};
use wrist_ble_core::goals::{Goals, Power};
use wrist_ble_core::status::{status, Status};
use wrist_bluenrg2::{Scheduler, CAPABILITIES, MENU};

use crate::checks::Checks;
use crate::fake::Fake;

/// Steps at one instant beyond which the core is spinning.
const SPIN: u32 = 200;

/// Lines of trace kept for a failure's report.
const TRACE: usize = 300;

pub struct Sim {
    pub now: Instant,
    pub seed: u64,
    pub core: Core<'static>,
    pub fake: Fake,
    pub checks: Checks,
    /// Everything the core said beyond the status.
    pub tells: Vec<(Instant, Tell)>,
    next: Option<Instant>,
    spins: (Instant, u32),
    trace: VecDeque<String>,
}

/// The radio on, discoverable, on `power`.
#[must_use]
pub fn on(power: Power) -> Goals {
    Goals { radio_on: true, power, ..Goals::OFF }
}

impl Sim {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            now: Instant(0),
            seed,
            core: Core::new(&CAPABILITIES, &Scheduler, &MENU),
            fake: Fake::new(seed),
            checks: Checks::default(),
            tells: Vec::new(),
            next: None,
            spins: (Instant(0), 0),
            trace: VecDeque::new(),
        }
    }

    /// Started with `goals` and run until the radio has settled.
    #[must_use]
    pub fn started(seed: u64, goals: Goals) -> Self {
        let mut s = Self::new(seed);
        s.goals(goals);
        s.run_for(Millis::secs(2));
        s
    }

    #[must_use]
    pub fn status(&self) -> Status {
        status(&self.core.state, self.now)
    }

    /// The console's status line now.
    #[must_use]
    pub fn line(&self) -> String {
        let mut s = String::new();
        self.status().write_line(&mut s).unwrap_or(());
        s
    }

    pub fn goals(&mut self, g: Goals) {
        self.checks.disturbed(self.now);
        self.feed(Input::Goals(g));
    }

    pub fn power(&mut self, p: Power) {
        let g = Goals { power: p, ..*self.core.goals() };
        self.goals(g);
    }

    pub fn input(&mut self, i: Input) {
        self.feed(i);
    }

    pub fn run_for(&mut self, d: Millis) {
        self.run_until(self.now + d);
    }

    /// # Panics
    ///
    /// A check fails, or the fake owes something it never gets past: this would spin forever.
    pub fn run_until(&mut self, until: Instant) {
        loop {
            let core = self.next.filter(|&t| t <= until);
            let fake = self.fake.next_at().filter(|&t| t <= until);
            match (core, fake) {
                (Some(c), f) if f.is_none_or(|f| c < f) => {
                    self.now = self.now.max(c);
                    self.feed(Input::Tick);
                }
                (_, Some(f)) => {
                    if let Some((at, e)) = self.fake.advance(f) {
                        self.now = self.now.max(at);
                        if matches!(e, RadioEvent::LinkUp { .. } | RadioEvent::LinkDown { .. } | RadioEvent::ChipReset(_) | RadioEvent::EventsLost) {
                            self.checks.disturbed(self.now);
                        }
                        self.feed(Input::Radio(e));
                    } else {
                        // The fake owed something by `f`; it must have moved past it, or this loop spins.
                        assert!(self.fake.next_at().is_none_or(|n| n > f), "seed {}: the fake stands still at {f:?}\n{}", self.seed, self.report());
                        self.now = self.now.max(f);
                    }
                }
                (None, None) => break,
                (Some(_), None) => unreachable!(),
            }
        }
        self.now = self.now.max(until);
        self.fake.advance(until);
        self.check();
    }

    fn feed(&mut self, input: Input) {
        if self.spins.0 == self.now {
            self.spins.1 += 1;
            assert!(self.spins.1 < SPIN, "seed {}: the core spins at {:?}\n{}", self.seed, self.now, self.report());
        } else {
            self.spins = (self.now, 0);
        }
        if let Err(e) = self.checks.before(&self.core.state, self.now) {
            panic!("seed {}: {e}\n{}", self.seed, self.report());
        }
        self.fake.sync(self.now);
        let out = self.core.step(self.now, input);
        self.log(format!("{:>9} {input:?} -> {}", self.now.0, brief(&out)));
        self.next = out.next;
        self.check();
        self.act(&out);
    }

    fn act(&mut self, out: &Outputs) {
        self.tells.extend(out.tell.iter().flatten().map(|&t| (self.now, t)));
        match out.chip {
            Some(Action::Start) => self.fake.start(),
            Some(Action::Hold) => self.fake.stop(),
            None => {}
        }
        if let Some(op) = out.op {
            let r = self.fake.submit(op);
            if r.is_err() {
                self.checks.disturbed(self.now);
            }
            self.feed(Input::Submitted(r));
        }
        if out.snapshot {
            let s = self.fake.snapshot();
            self.feed(Input::Snapshot(Ok(s)));
        }
        self.check();
    }

    fn check(&mut self) {
        if let Err(e) = self.checks.after(&self.core, &self.fake, self.now) {
            panic!("seed {}: {e} at {:?}\n{}\n{}", self.seed, self.now, self.line(), self.report());
        }
    }

    fn log(&mut self, line: String) {
        if self.trace.len() == TRACE {
            self.trace.pop_front();
        }
        self.trace.push_back(line);
    }

    #[must_use]
    pub fn report(&self) -> String {
        self.trace.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn brief(out: &Outputs) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if let Some(op) = out.op {
        let _ = write!(s, "{op:?} ");
    }
    if let Some(c) = out.chip {
        let _ = write!(s, "{c:?} ");
    }
    for t in out.tell.iter().flatten() {
        let _ = write!(s, "{t:?} ");
    }
    if out.snapshot {
        s += "snapshot ";
    }
    s
}
