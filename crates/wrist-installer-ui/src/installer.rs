//! What the screen shows and what it asks the shell to do, from the service's reports, the
//! buttons and the clock.

use crate::flow::{Action, ConfirmFlow, Input, Phase};
use crate::label::Label;

/// `InstallerMessage::State` in `apps/image-installer/Libs/Header/Commands.hpp`, by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Checking,
    /// The firmware isolates apps; nothing was attempted.
    Isolated,
    /// `image.bin` is missing, empty or unreadable.
    NoImage,
    /// A check refused the image; the code says which.
    Refused,
    /// The flash unlock self-test failed; the code is its result.
    UnlockFailed,
    /// The scratch-page erase and program self-test failed; the code is its result.
    FlashTestFailed,
    /// The dry run passed for the image with this CRC.
    Ready,
    /// Confirmed: staging, then the commit, which restarts the watch.
    Installing,
}

impl State {
    /// A value the service does not send is shown as a refusal with its code, as anything
    /// unexpected should be.
    #[must_use]
    pub const fn from_wire(value: u8) -> State {
        match value {
            0 => State::Checking,
            1 => State::Isolated,
            2 => State::NoImage,
            4 => State::UnlockFailed,
            5 => State::FlashTestFailed,
            6 => State::Ready,
            7 => State::Installing,
            _ => State::Refused,
        }
    }
}

/// The parts of the service's report the screen uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub state: State,
    pub code: i32,
    pub crc: u32,
}

impl Verdict {
    pub const CHECKING: Verdict = Verdict { state: State::Checking, code: 0, crc: 0 };
}

/// What the shell does after an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// Draw and push a frame.
    pub redraw: bool,
    /// Send the service the confirm naming this CRC.
    pub confirm: Option<u32>,
    /// Leave the app.
    pub exit: bool,
}

/// The colour of a button's hint arc at the rim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Off,
    White,
    Amber,
    Green,
}

/// Which hints are lit. L1 and L2 do nothing here and are never lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hints {
    /// R1, Select, upper right.
    pub r1: Hint,
    /// R2, Back, lower right.
    pub r2: Hint,
}

/// The verdict, its CRC and what to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub headline: Label,
    /// Under the headline; empty for none.
    pub caption: &'static str,
    /// Wrapped to the glass; a newline starts a new line.
    pub detail: &'static str,
    pub hint: &'static str,
    pub hints: Hints,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hold {
    /// 3, 2, then 1.
    pub number: u32,
    pub crc: Label,
    /// Milliseconds held, up to [`crate::flow::HOLD_MS`]: how far round the ring is.
    pub held_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Info(Info),
    Hold(Hold),
}

pub const TITLE: &str = "wrist-rs installer";
pub const HOLD_PROMPT: &str = "Hold to install";
pub const HOLD_NAME: &str = "wrist-rs";
pub const BACK_TO_EXIT: &str = "Back to exit";

const NOTHING: Hints = Hints { r1: Hint::Off, r2: Hint::Off };
const BACK: Hints = Hints { r1: Hint::Off, r2: Hint::White };

fn refusal(headline: Label, detail: &'static str) -> Screen {
    Screen::Info(Info { headline, caption: "", detail, hint: BACK_TO_EXIT, hints: BACK })
}

/// The installer's one screen: the flow, and the last verdict the service reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Installer {
    flow: ConfirmFlow,
    verdict: Verdict,
}

impl Default for Installer {
    fn default() -> Self {
        Self::new()
    }
}

impl Installer {
    #[must_use]
    pub const fn new() -> Self {
        Installer { flow: ConfirmFlow::new(), verdict: Verdict::CHECKING }
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.flow.phase()
    }

    /// A report from the service.
    pub fn report(&mut self, verdict: Verdict, now_ms: u32) -> Reply {
        self.verdict = verdict;
        match verdict.state {
            State::Checking => Reply { redraw: true, ..Reply::default() },
            State::Ready => self.input(Input::ReportReady, now_ms),
            State::Installing => self.input(Input::ReportInstalling, now_ms),
            _ => self.input(Input::ReportBlocked, now_ms),
        }
    }

    /// A button, a GUI tick, or the screen being suspended.
    pub fn input(&mut self, input: Input, now_ms: u32) -> Reply {
        let before = self.flow.phase();
        let mut reply = Reply::default();
        match self.flow.handle(input, now_ms) {
            // The CRC on screen is the one the service checks the confirm against.
            Action::Confirm => reply.confirm = Some(self.verdict.crc),
            Action::Exit => {
                reply.exit = true;
                return reply;
            }
            Action::None => {}
        }
        // While holding, every tick moves the ring.
        let phase = self.flow.phase();
        reply.redraw = input != Input::Tick || phase != before || phase == Phase::Holding;
        reply
    }

    /// What to draw at `now_ms`.
    #[must_use]
    pub fn screen(&self, now_ms: u32) -> Screen {
        let v = self.verdict;
        let crc = Label::hex(v.crc);
        let phase = self.flow.phase();
        match phase {
            Phase::Holding => {
                return Screen::Hold(Hold {
                    number: self.flow.countdown(now_ms),
                    crc,
                    held_ms: self.flow.held(now_ms),
                })
            }
            Phase::Waiting => {
                return Screen::Info(Info {
                    headline: Label::text("Checking"),
                    caption: "",
                    detail: "Reading the wrist-rs image and testing flash. Nothing is replaced.",
                    hint: BACK_TO_EXIT,
                    hints: BACK,
                })
            }
            Phase::Ready => {
                return Screen::Info(Info {
                    headline: crc,
                    caption: "wrist-rs image CRC-32",
                    detail: "Dry run passed.\nCompare with\nINSTALL-CRC.txt.",
                    hint: "Hold Select to install",
                    hints: Hints { r1: Hint::Amber, r2: Hint::White },
                })
            }
            Phase::Sent => {
                return Screen::Info(Info {
                    headline: Label::text("Installing"),
                    caption: "",
                    detail: "Replacing the stock firmware with wrist-rs.\nDo not interrupt power.",
                    hint: "",
                    hints: NOTHING,
                })
            }
            Phase::Blocked => {}
        }
        match v.state {
            State::Isolated => refusal(
                Label::text("Refused"),
                "The firmware isolates apps. Nothing was attempted.",
            ),
            State::NoImage => refusal(
                Label::text("No image"),
                "The wrist-rs image.bin is missing, empty or unreadable.",
            ),
            State::UnlockFailed => refusal(
                Label::text("Unlock ").then_decimal(v.code),
                "The flash unlock self-test failed. Nothing is replaced.",
            ),
            State::FlashTestFailed => refusal(
                Label::text("Flash ").then_hex2(v.code.to_le_bytes()[0]),
                "The flash self-test failed. Nothing is replaced.",
            ),
            _ => refusal(
                Label::text("Refused ").then_decimal(v.code),
                "Stock is unchanged.\nKeep install.log\nand stage.log.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::HOLD_MS;

    const CRC: u32 = 0x1A2B_3C4D;

    fn ready() -> Verdict {
        Verdict { state: State::Ready, code: 0, crc: CRC }
    }

    fn info(screen: Screen) -> Info {
        match screen {
            Screen::Info(info) => info,
            Screen::Hold(_) => panic!("the hold, not a verdict"),
        }
    }

    #[test]
    fn the_confirm_names_the_crc_on_screen() {
        let mut ui = Installer::new();
        ui.report(ready(), 0);
        assert_eq!(info(ui.screen(0)).headline.as_str(), "1A2B3C4D");
        assert_eq!(ui.input(Input::R1Press, 100), Reply { redraw: true, ..Reply::default() });
        let reply = ui.input(Input::Tick, 100 + HOLD_MS);
        assert_eq!(reply.confirm, Some(CRC));
        assert!(reply.redraw);
        assert_eq!(info(ui.screen(100 + HOLD_MS)).headline.as_str(), "Installing");
    }

    #[test]
    fn every_tick_of_a_hold_redraws_and_an_idle_tick_does_not() {
        let mut ui = Installer::new();
        ui.report(ready(), 0);
        assert!(!ui.input(Input::Tick, 50).redraw);
        ui.input(Input::R1Press, 100);
        for t in (200..100 + HOLD_MS).step_by(100) {
            let reply = ui.input(Input::Tick, t);
            assert!(reply.redraw && reply.confirm.is_none(), "{t}");
            match ui.screen(t) {
                Screen::Hold(h) => assert_eq!(h.held_ms, t - 100),
                Screen::Info(_) => panic!("{t}: not the hold"),
            }
        }
    }

    #[test]
    fn back_exits_without_drawing() {
        let mut ui = Installer::new();
        assert_eq!(ui.input(Input::R2Click, 0), Reply { exit: true, ..Reply::default() });
    }

    #[test]
    fn a_report_while_checking_redraws() {
        let mut ui = Installer::new();
        assert!(ui.report(Verdict::CHECKING, 0).redraw);
        assert_eq!(info(ui.screen(0)).headline.as_str(), "Checking");
    }

    #[test]
    fn each_refusal_says_what_failed() {
        let cases = [
            (State::Isolated, 2, "Refused"),
            (State::NoImage, 0, "No image"),
            (State::UnlockFailed, 1, "Unlock 1"),
            (State::FlashTestFailed, 0x0137, "Flash 37"),
            (State::Refused, -16, "Refused -16"),
            (State::from_wire(99), -3, "Refused -3"),
        ];
        for (state, code, headline) in cases {
            let mut ui = Installer::new();
            assert!(ui.report(Verdict { state, code, crc: CRC }, 0).redraw);
            assert_eq!(ui.phase(), Phase::Blocked);
            let shown = info(ui.screen(0));
            assert_eq!(shown.headline.as_str(), headline);
            assert_eq!(shown.hints, BACK);
            assert_eq!(ui.input(Input::R1Press, 1), Reply { redraw: true, ..Reply::default() });
            assert_eq!(ui.input(Input::Tick, 1 + HOLD_MS), Reply::default());
        }
    }

    #[test]
    fn a_refusal_after_the_confirm_replaces_the_installing_screen() {
        let mut ui = Installer::new();
        ui.report(ready(), 0);
        ui.input(Input::R1Press, 0);
        ui.input(Input::Tick, HOLD_MS);
        ui.report(Verdict { state: State::Installing, code: 0, crc: CRC }, HOLD_MS + 10);
        assert_eq!(info(ui.screen(HOLD_MS + 10)).hints, NOTHING);
        ui.report(Verdict { state: State::Refused, code: -14, crc: CRC }, HOLD_MS + 20);
        let shown = info(ui.screen(HOLD_MS + 20));
        assert_eq!(shown.headline.as_str(), "Refused -14");
        assert!(ui.input(Input::R2Click, HOLD_MS + 30).exit);
    }

    #[test]
    fn the_wire_values_are_commands_hpp() {
        let states = [
            State::Checking,
            State::Isolated,
            State::NoImage,
            State::Refused,
            State::UnlockFailed,
            State::FlashTestFailed,
            State::Ready,
            State::Installing,
        ];
        for (value, state) in (0u8..).zip(states) {
            assert_eq!(State::from_wire(value), state);
        }
    }
}
