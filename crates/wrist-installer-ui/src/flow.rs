//! The confirm: R1 pressed on the ready screen and held for [`HOLD_MS`]; letting go, R2 or leaving cancels.

/// Long, since replacing the firmware is hard to undo.
pub const HOLD_MS: u32 = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No report from the service yet.
    Waiting,
    /// The service reported something other than a ready dry run.
    Blocked,
    Ready,
    Holding,
    /// The confirm has gone; buttons do nothing until the service reports.
    Sent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    ReportReady,
    ReportBlocked,
    ReportInstalling,
    R1Press,
    R1Release,
    R2Click,
    Tick,
    Suspend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Confirm,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmFlow {
    phase: Phase,
    since: u32,
}

impl Default for ConfirmFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfirmFlow {
    #[must_use]
    pub const fn new() -> Self {
        ConfirmFlow { phase: Phase::Waiting, since: 0 }
    }

    /// `now_ms` is a millisecond clock that may wrap.
    pub fn handle(&mut self, input: Input, now_ms: u32) -> Action {
        match input {
            Input::ReportReady => {
                if matches!(self.phase, Phase::Waiting | Phase::Blocked) {
                    self.enter(Phase::Ready, now_ms);
                }
                Action::None
            }
            Input::ReportBlocked => {
                self.enter(Phase::Blocked, now_ms);
                Action::None
            }
            Input::ReportInstalling => {
                self.enter(Phase::Sent, now_ms);
                Action::None
            }
            Input::R1Press => {
                if self.phase == Phase::Ready {
                    self.enter(Phase::Holding, now_ms);
                }
                Action::None
            }
            Input::R1Release | Input::Suspend => {
                if self.phase == Phase::Holding {
                    self.enter(Phase::Ready, now_ms);
                }
                Action::None
            }
            Input::R2Click => match self.phase {
                Phase::Holding => {
                    self.enter(Phase::Ready, now_ms);
                    Action::None
                }
                Phase::Sent => Action::None,
                _ => Action::Exit,
            },
            Input::Tick => {
                if self.phase == Phase::Holding && now_ms.wrapping_sub(self.since) >= HOLD_MS {
                    self.enter(Phase::Sent, now_ms);
                    Action::Confirm
                } else {
                    Action::None
                }
            }
        }
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// How long R1 has been held, up to [`HOLD_MS`]; 0 unless holding.
    #[must_use]
    pub const fn held(&self, now_ms: u32) -> u32 {
        if !matches!(self.phase, Phase::Holding) {
            return 0;
        }
        let elapsed = now_ms.wrapping_sub(self.since);
        if elapsed < HOLD_MS {
            elapsed
        } else {
            HOLD_MS
        }
    }

    /// The countdown's number while holding: 3, 2, then 1; the confirm goes before 0.
    #[must_use]
    pub const fn countdown(&self, now_ms: u32) -> u32 {
        let step = HOLD_MS / 3;
        let left = 3 - self.held(now_ms) / step;
        if left < 1 {
            1
        } else {
            left
        }
    }

    fn enter(&mut self, phase: Phase, now_ms: u32) {
        self.phase = phase;
        self.since = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: u32 = HOLD_MS;

    fn ready() -> ConfirmFlow {
        let mut f = ConfirmFlow::new();
        f.handle(Input::ReportReady, 0);
        f
    }

    fn holding(pressed_at: u32) -> ConfirmFlow {
        let mut f = ready();
        f.handle(Input::R1Press, pressed_at);
        f
    }

    #[test]
    fn a_full_hold_confirms_once_and_only_then() {
        let mut f = holding(200);
        assert_eq!(f.handle(Input::Tick, 200 + HOLD - 1), Action::None);
        assert_eq!(f.phase(), Phase::Holding);
        assert_eq!(f.handle(Input::Tick, 200 + HOLD), Action::Confirm);
        assert_eq!(f.phase(), Phase::Sent);
        assert_eq!(f.handle(Input::Tick, 99_999), Action::None);
    }

    #[test]
    fn the_countdown_reads_3_2_1_and_never_0() {
        let f = holding(0);
        assert_eq!(f.countdown(0), 3);
        assert_eq!(f.countdown(HOLD / 3), 2);
        assert_eq!(f.countdown(2 * HOLD / 3), 1);
        assert_eq!(f.countdown(HOLD - 1), 1);
        assert_eq!(f.countdown(HOLD), 1);
        assert_eq!(ready().countdown(0), 3);
    }

    #[test]
    fn letting_go_early_cancels_and_the_next_hold_starts_from_zero() {
        let mut f = holding(200);
        f.handle(Input::R1Release, 2000);
        assert_eq!(f.phase(), Phase::Ready);
        f.handle(Input::R1Press, 2100);
        assert_eq!(f.handle(Input::Tick, 200 + HOLD), Action::None);
        assert_eq!(f.held(2100 + 500), 500);
        assert_eq!(f.handle(Input::Tick, 2100 + HOLD), Action::Confirm);
    }

    #[test]
    fn r2_and_leaving_cancel_a_hold_and_r2_exits_only_when_nothing_is_held() {
        let mut a = holding(200);
        let mut b = holding(200);
        a.handle(Input::R2Click, 300);
        b.handle(Input::Suspend, 300);
        assert_eq!(a.phase(), Phase::Ready);
        assert_eq!(b.phase(), Phase::Ready);
        assert_eq!(b.handle(Input::Tick, 200 + HOLD), Action::None);
        assert_eq!(a.handle(Input::R2Click, 400), Action::Exit);
    }

    #[test]
    fn a_press_from_before_the_ready_screen_is_not_a_hold() {
        let mut f = ConfirmFlow::new();
        f.handle(Input::R1Press, 0);
        f.handle(Input::ReportReady, 100);
        assert_eq!(f.handle(Input::Tick, 100 + 10 * HOLD), Action::None);
        assert_eq!(f.phase(), Phase::Ready);
    }

    #[test]
    fn nothing_is_held_before_a_ready_dry_run_or_after_another_report() {
        let mut waiting = ConfirmFlow::new();
        waiting.handle(Input::R1Press, 0);
        let mut blocked = ConfirmFlow::new();
        blocked.handle(Input::ReportBlocked, 0);
        blocked.handle(Input::R1Press, 0);
        let mut late = holding(200);
        late.handle(Input::ReportBlocked, 300);
        assert_eq!(waiting.phase(), Phase::Waiting);
        assert_eq!(blocked.phase(), Phase::Blocked);
        assert_eq!(late.handle(Input::Tick, 200 + HOLD), Action::None);
    }

    #[test]
    fn once_the_confirm_has_gone_r2_does_not_leave_the_installing_screen() {
        let mut f = holding(200);
        f.handle(Input::Tick, 200 + HOLD);
        f.handle(Input::ReportInstalling, 3300);
        assert_eq!(f.handle(Input::R2Click, 3400), Action::None);
        assert_eq!(f.phase(), Phase::Sent);
    }

    #[test]
    fn the_millisecond_clock_may_wrap_during_a_hold() {
        let mut f = holding(0xFFFF_FF00);
        assert_eq!(f.handle(Input::Tick, HOLD - 0x100 - 1), Action::None);
        assert_eq!(f.handle(Input::Tick, HOLD), Action::Confirm);
    }
}
