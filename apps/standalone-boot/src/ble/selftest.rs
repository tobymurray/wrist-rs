//! `ble selftest`: the steps of [`wrist_bluenrg2::selftest`] carried out on the chip, the core
//! told it does not have the chip meanwhile, then one result line.

use core::fmt::Write as _;
use core::sync::atomic::Ordering;

use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use wrist_ble::{Fault, Millis, Snapshot};
use wrist_bluenrg2::selftest::{Answer, Outcome, Selftest, Step, Why};
use wrist_bluenrg2::snapshot::{Snap, Snapping};
use wrist_bluenrg2::transport;

use super::{now_ms, trace, trace_command, Link, Text, CONN_HANDLE, SERVER};

impl Link<'_> {
    pub(super) async fn selftest(&mut self) {
        let refused = if CONN_HANDLE.load(Ordering::Relaxed) != 0 || self.shadow.state().phone.link().is_some() {
            Some("needs no link; end it first")
        } else if self.scanner.until().is_some() || self.transfer.is_some() {
            Some("a scan or an update is running; try after it")
        } else if self.shown.is_some() || self.shadow.issuing() || self.shadow.snapshot_wanted() || self.shadow.chip_acting() || self.chip.due().is_some() {
            Some("the core has work in flight; try again")
        } else {
            None
        };
        let restore = self.shadow.state().radio.advertising;
        let (None, Some(restore)) = (refused, restore) else {
            self.uart.write_str("[ble] selftest: ");
            return self.uart.write_line(refused.unwrap_or("runs only while advertising"));
        };
        trace::packet(trace::Dir::Selftest, &[1]);
        self.shadow.testing(now_ms(), true);
        self.test = Some(Selftest::new(&SERVER, restore));
        let outcome = loop {
            let Some(step) = self.test.as_ref().map(Selftest::next) else { break None };
            let answer = match step {
                Step::Start => {
                    self.chip_gone();
                    self.bring_up().await;
                    let handles = self.shown.is_none().then(|| self.chip.server().copied()).flatten();
                    Answer::Started { handles, central: self.scanner.central() }
                }
                Step::Command(c) => Answer::Replied(self.exchange(c.opcode, c.params()).await),
                Step::Op(op) => Answer::Settled(self.settled(op).await.1.map(|_| ())),
                Step::Snapshot => Answer::Snapshot(self.test_snapshot().await),
                Step::Wait(within) => {
                    self.test_wait(within).await;
                    Answer::Waited
                }
                Step::Over(o) => break Some(o),
            };
            if let Some(t) = self.test.as_mut() {
                t.answer(answer);
            }
            embassy_futures::yield_now().await;
        };
        self.test = None;
        trace::packet(trace::Dir::Selftest, &[0]);
        self.shadow.testing(now_ms(), false);
        // The core restarts a chip whose start stopped short, as after its own.
        if matches!(outcome, Some(Outcome::Fail(_, Why::StartStopped))) {
            trace::packet(trace::Dir::StartFailed, &[]);
            self.shadow.start_failed(now_ms());
        }
        if let Some(o) = outcome {
            let _ = write!(Text(self.uart), "[ble] selftest {o}");
            self.uart.write_line("");
        }
    }

    /// The driver's snapshot, the core not told of it.
    async fn test_snapshot(&mut self) -> Result<Snapshot, Fault> {
        let mut snap = Snap::default();
        let mut packet = [0u8; transport::MAX_READ];
        let mut taken = Err(Fault::Silent);
        while let Some(note) = self.chip.snapshotting(&mut snap, &mut packet).await {
            match note {
                Snapping::Got(got) => self.got(&packet, got),
                Snapping::Sending(c) => {
                    trace_command(c.opcode, c.params());
                    self.shadow.command(now_ms(), c.opcode, c.params());
                }
                Snapping::Taken(r) => taken = r,
            }
        }
        taken
    }

    /// The chip's packets handled until the selftest waits no more, for up to `within`.
    async fn test_wait(&mut self, within: Millis) {
        let end = Instant::now() + Duration::from_millis(u64::from(within.0));
        while self.test.as_ref().is_some_and(Selftest::waiting) {
            match select(self.chip.wait_pending(), Timer::at(end)).await {
                Either::First(()) => self.drain().await,
                Either::Second(()) => break,
            }
        }
    }
}
