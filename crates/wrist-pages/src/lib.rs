#![no_std]

//! Which page is on the glass, and where each press moves it.
//!
//! Pure state: the image hands it presses (from the buttons or the console's `press`) and the
//! time, and draws [`Nav::page`]. From the face, Down and Up step through the image's pages; on
//! another page they keep stepping, Back returns to the face, and after [`TIMEOUT_MS`] without a
//! press the face comes back by itself. Select and long presses move nothing; [`action`] says what
//! a press does beyond moving.
//!
//! Lighting the face is the image's, for every short press on every page, as before pages.
//! Holds are not presses: any button held 3 s resets into the bootloader, and Select held
//! 16 s cycles every rail. Nothing here sees them.

/// A button, by what the case calls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Back,
    Select,
}

impl Button {
    /// In the order of the image's button table: Up/`SW1`, Down/`SW3`, Back/`SW4`,
    /// Select/`SW2`.
    pub const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Back, Self::Select];

    /// From the image's button index.
    #[must_use]
    pub const fn from_index(i: usize) -> Option<Self> {
        if i < Self::ALL.len() {
            Some(Self::ALL[i])
        } else {
            None
        }
    }

    /// The image's button index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Lower case, as the console takes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Back => "back",
            Self::Select => "select",
        }
    }

    /// The switch and the name, for the console's log.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Up => "SW1/Up",
            Self::Down => "SW3/Down",
            Self::Back => "SW4/Back",
            Self::Select => "SW2/Select",
        }
    }

    /// `up`, `down`, `back` or `select`, in any case.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name().eq_ignore_ascii_case(word))
    }
}

/// A debounced press: released before the hold that resets, and long when held past a short
/// press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub button: Button,
    pub long: bool,
}

/// The pages, in the order Down visits them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    /// The date, the time and the charge.
    #[default]
    Face,
    /// The charge, the cell's voltage and the input.
    Battery,
    /// The radio, its pairing prompt, and the switch.
    Bluetooth,
    /// The version and the build ID.
    About,
}

impl Page {
    pub const ALL: [Self; 4] = [Self::Face, Self::Battery, Self::Bluetooth, Self::About];
    /// An image without Bluetooth's.
    pub const WITHOUT_BLUETOOTH: [Self; 3] = [Self::Face, Self::Battery, Self::About];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Face => "face",
            Self::Battery => "battery",
            Self::Bluetooth => "bluetooth",
            Self::About => "about",
        }
    }
}

/// What a press on a page does beyond moving between pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Turn the radio on, or off.
    ToggleBluetooth,
    /// Answer the numeric comparison waiting: the phone shows the same number, or not.
    Compare(bool),
}

/// The six digits a pairing puts on the Bluetooth page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
    Passkey,
    Compare,
}

/// How long a pairing's digits wait for the wearer: past any pairing's own timeout.
pub const PROMPT_MS: u64 = 60_000;

/// A pairing's six digits, from the radio's prompt until the wearer answers or the pairing ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Prompt {
    #[default]
    None,
    /// A passkey asked for, its digits yet to come.
    Asked,
    /// Since the image's millisecond given.
    Waiting(Pairing, u32, u64),
    /// A comparison answered; the radio has yet to pass it on.
    Answered(bool),
}

impl Prompt {
    /// The passkey's digits, if the pairing that asked for them has not ended since.
    pub fn passkey(&mut self, value: u32, now_ms: u64) -> bool {
        let asked = *self == Self::Asked;
        if asked {
            *self = Self::Waiting(Pairing::Passkey, value, now_ms);
        }
        asked
    }

    /// The pairing still waiting for the wearer at `now_ms`, and its digits.
    #[must_use]
    pub const fn waiting(self, now_ms: u64) -> Option<(Pairing, u32)> {
        match (self, self.ends_ms(now_ms)) {
            (Self::Waiting(pairing, value, _), Some(_)) => Some((pairing, value)),
            _ => None,
        }
    }

    /// When the digits still waiting at `now_ms` stop.
    #[must_use]
    pub const fn ends_ms(self, now_ms: u64) -> Option<u64> {
        match self {
            Self::Waiting(_, _, since) if now_ms < since.saturating_add(PROMPT_MS) => Some(since.saturating_add(PROMPT_MS)),
            _ => None,
        }
    }

    /// Answer the comparison waiting at `now_ms`; false when none is.
    pub fn answer(&mut self, yes: bool, now_ms: u64) -> bool {
        let waiting = matches!(self.waiting(now_ms), Some((Pairing::Compare, _)));
        if waiting {
            *self = Self::Answered(yes);
        }
        waiting
    }

    /// When the radio owes the chip the comparison's answer, and the answer: the wearer's, or
    /// no once the digits stop waiting.
    #[must_use]
    pub const fn owed(self) -> Option<(u64, bool)> {
        match self {
            Self::Waiting(Pairing::Compare, _, since) => Some((since.saturating_add(PROMPT_MS), false)),
            Self::Answered(yes) => Some((0, yes)),
            _ => None,
        }
    }
}

/// The action `press` takes on `page` while `pairing`, if any.
#[must_use]
pub const fn action(page: Page, press: Press, pairing: Option<Pairing>) -> Option<Action> {
    match (page, press.button, press.long, pairing) {
        (Page::Bluetooth, Button::Select, false, Some(Pairing::Compare)) => Some(Action::Compare(true)),
        (Page::Bluetooth, Button::Back, false, Some(Pairing::Compare)) => Some(Action::Compare(false)),
        (Page::Bluetooth, Button::Select, false, None) => Some(Action::ToggleBluetooth),
        _ => None,
    }
}

/// A central refused for a pairing key the watch no longer has, as its link gave its address,
/// and how many times in a row. It can't connect until it forgets the watch and pairs again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OldKey(Option<([u8; 6], u32)>);

impl OldKey {
    pub const NONE: Self = Self(None);

    pub fn refused(&mut self, address: [u8; 6]) {
        let tries = match self.0 {
            Some((a, n)) if a == address => n.saturating_add(1),
            _ => 1,
        };
        self.0 = Some((address, tries));
    }

    /// A pairing done, or the radio off.
    pub fn clear(&mut self) {
        self.0 = None;
    }

    #[must_use]
    pub const fn central(self) -> Option<([u8; 6], u32)> {
        self.0
    }
}

/// The page a waiting `pairing` holds up: none while an alarm rings, so the ring's face is seen
/// and the first press stops it.
#[must_use]
pub const fn held(pairing: Option<Pairing>, ringing: bool) -> Option<Page> {
    match (pairing, ringing) {
        (Some(_), false) => Some(Page::Bluetooth),
        _ => None,
    }
}

/// How long a page other than the face stays without a press.
pub const TIMEOUT_MS: u64 = 10_000;

/// Where the wearer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nav {
    page: Page,
    /// When the last press arrived, in the image's milliseconds.
    pressed_at_ms: u64,
    /// The image's pages, in the order Down visits them, the face first.
    pages: &'static [Page],
}

impl Default for Nav {
    fn default() -> Self {
        Self::new()
    }
}

impl Nav {
    /// On the face, among the pages of an image without Bluetooth.
    #[must_use]
    pub const fn new() -> Self {
        Self::with(&Page::WITHOUT_BLUETOOTH)
    }

    /// On the face, among `pages`.
    #[must_use]
    pub const fn with(pages: &'static [Page]) -> Self {
        Self { page: Page::Face, pressed_at_ms: 0, pages }
    }

    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// The page `press` leads to from `page`.
    #[must_use]
    pub const fn next(&self, page: Page, press: Press) -> Page {
        if press.long {
            return page;
        }
        match press.button {
            Button::Down => self.step(page, 1),
            Button::Up => self.step(page, self.pages.len() - 1),
            Button::Back => Page::Face,
            Button::Select => page,
        }
    }

    /// `by` pages on from `page`, wrapping; the face for a page this image lacks.
    const fn step(&self, page: Page, by: usize) -> Page {
        let mut i = 0;
        while i < self.pages.len() {
            if self.pages[i] as u8 == page as u8 {
                return self.pages[(i + by) % self.pages.len()];
            }
            i += 1;
        }
        Page::Face
    }

    /// Apply `press` at `now_ms`; true when the page changed. Every press restarts the
    /// timeout.
    pub fn press(&mut self, press: Press, now_ms: u64) -> bool {
        self.pressed_at_ms = now_ms;
        let before = self.page;
        self.page = self.next(before, press);
        self.page != before
    }

    /// Bring up `page` as if pressed to at `now_ms`, if this image has it; true when the page
    /// changed.
    pub fn show(&mut self, page: Page, now_ms: u64) -> bool {
        if !self.pages.contains(&page) {
            return false;
        }
        self.pressed_at_ms = now_ms;
        let before = self.page;
        self.page = page;
        self.page != before
    }

    /// When [`Nav::tick`] next has something to do: `None` on the face.
    #[must_use]
    pub const fn deadline_ms(&self) -> Option<u64> {
        match self.page {
            Page::Face => None,
            _ => Some(self.pressed_at_ms.saturating_add(TIMEOUT_MS)),
        }
    }

    /// Back to the face once the timeout has passed; true when the page changed.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        match self.deadline_ms() {
            Some(due) if now_ms >= due => {
                self.page = Page::Face;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn short(button: Button) -> Press {
        Press { button, long: false }
    }

    #[test]
    fn an_old_key_is_counted_per_central_until_a_pairing_or_the_radio_off() {
        let (a, b) = ([1, 2, 3, 4, 5, 6], [6, 5, 4, 3, 2, 1]);
        let mut old = OldKey::NONE;
        assert_eq!(old.central(), None);
        old.refused(a);
        old.refused(a);
        assert_eq!(old.central(), Some((a, 2)));
        old.refused(b);
        assert_eq!(old.central(), Some((b, 1)));
        old.clear();
        assert_eq!(old.central(), None);
    }

    #[test]
    fn buttons_round_trip_by_index_and_name() {
        for (i, b) in Button::ALL.into_iter().enumerate() {
            assert_eq!(Button::from_index(i), Some(b));
            assert_eq!(b.index(), i);
            assert_eq!(Button::parse(b.name()), Some(b));
            assert!(b.label().ends_with(b.name().get(1..).unwrap()));
        }
        assert_eq!(Button::from_index(4), None);
        assert_eq!(Button::parse("Up"), Some(Button::Up));
        assert_eq!(Button::parse("SELECT"), Some(Button::Select));
        assert_eq!(Button::parse("u"), None);
        assert_eq!(Button::parse(""), None);
    }

    #[test]
    fn every_short_press_from_every_page() {
        use Button::{Back, Down, Select, Up};
        use Page::{About, Battery, Bluetooth, Face};
        for (pages, table) in [
            (
                &Page::WITHOUT_BLUETOOTH[..],
                &[
                    (Face, Up, About),
                    (Face, Down, Battery),
                    (Battery, Up, Face),
                    (Battery, Down, About),
                    (About, Up, Battery),
                    (About, Down, Face),
                ][..],
            ),
            (
                &Page::ALL[..],
                &[
                    (Face, Up, About),
                    (Face, Down, Battery),
                    (Battery, Up, Face),
                    (Battery, Down, Bluetooth),
                    (Bluetooth, Up, Battery),
                    (Bluetooth, Down, About),
                    (About, Up, Bluetooth),
                    (About, Down, Face),
                ][..],
            ),
        ] {
            let nav = Nav { pages, ..Nav::new() };
            assert_eq!(table.len(), pages.len() * 2);
            for &(from, button, to) in table {
                assert_eq!(nav.next(from, short(button)), to, "{from:?} + {button:?}");
            }
            for &from in pages {
                assert_eq!(nav.next(from, short(Back)), Face);
                assert_eq!(nav.next(from, short(Select)), from);
            }
        }
    }

    #[test]
    fn a_page_the_image_lacks_steps_to_the_face() {
        let nav = Nav::new();
        assert_eq!(nav.next(Page::Bluetooth, short(Button::Down)), Page::Face);
        assert_eq!(nav.next(Page::Bluetooth, short(Button::Up)), Page::Face);
    }

    #[test]
    fn long_presses_move_nothing() {
        let nav = Nav::with(&Page::ALL);
        for page in Page::ALL {
            for button in Button::ALL {
                assert_eq!(nav.next(page, Press { button, long: true }), page);
            }
        }
    }

    #[test]
    fn down_visits_every_page_and_comes_home() {
        for pages in [&Page::WITHOUT_BLUETOOTH[..], &Page::ALL[..]] {
            let mut nav = Nav::with(pages);
            let mut seen = [false; Page::ALL.len()];
            for t in 0..pages.len() as u64 {
                seen[nav.page() as usize] = true;
                nav.press(short(Button::Down), t);
            }
            assert_eq!(seen.iter().filter(|&&s| s).count(), pages.len());
            assert_eq!(nav.page(), Page::Face);
        }
    }

    #[test]
    fn select_on_the_bluetooth_page_switches_the_radio_or_answers_the_comparison() {
        let select = short(Button::Select);
        let held = Press { button: Button::Select, long: true };
        let compare = Some(Pairing::Compare);
        assert_eq!(action(Page::Bluetooth, select, None), Some(Action::ToggleBluetooth));
        assert_eq!(action(Page::Bluetooth, held, None), None);
        assert_eq!(action(Page::Bluetooth, select, compare), Some(Action::Compare(true)));
        assert_eq!(action(Page::Bluetooth, short(Button::Back), compare), Some(Action::Compare(false)));
        assert_eq!(action(Page::Bluetooth, short(Button::Back), None), None);
        for button in Button::ALL {
            assert_eq!(action(Page::Bluetooth, Press { button, long: true }, compare), None);
        }
        for page in [Page::Face, Page::Battery, Page::About] {
            for pairing in [None, compare] {
                for button in Button::ALL {
                    assert_eq!(action(page, short(button), pairing), None);
                }
                assert_eq!(action(page, held, pairing), None);
            }
        }
        for button in [Button::Up, Button::Down] {
            assert_eq!(action(Page::Bluetooth, short(button), compare), None);
        }
    }

    #[test]
    fn an_answered_comparison_waits_no_longer() {
        let mut prompt = Prompt::Waiting(Pairing::Compare, 123_456, 0);
        assert_eq!(prompt.waiting(0), Some((Pairing::Compare, 123_456)));
        assert_eq!(prompt.owed(), Some((PROMPT_MS, false)));
        assert!(prompt.answer(true, 0));
        assert_eq!((prompt.waiting(0), prompt.owed(), prompt.ends_ms(0)), (None, Some((0, true)), None));
        assert!(!prompt.answer(false, 0));
        assert_eq!(prompt.owed(), Some((0, true)));
        let mut passkey = Prompt::Waiting(Pairing::Passkey, 1, 0);
        assert!(!passkey.answer(true, 0));
        assert_eq!((passkey.waiting(0), passkey.owed()), (Some((Pairing::Passkey, 1)), None));
        assert!(!Prompt::None.answer(true, 0));
    }

    #[test]
    fn a_comparison_answered_after_its_digits_stop_is_not() {
        let mut prompt = Prompt::Waiting(Pairing::Compare, 7, 1_000);
        assert!(!prompt.answer(true, 1_000 + PROMPT_MS));
        assert_eq!(prompt.owed(), Some((1_000 + PROMPT_MS, false)));
        assert!(prompt.answer(true, 1_000 + PROMPT_MS - 1));
    }

    #[test]
    fn a_passkey_arriving_after_its_pairing_ended_is_not_shown() {
        let mut prompt = Prompt::Asked;
        assert_eq!((prompt.waiting(0), prompt.ends_ms(0)), (None, None));
        assert!(prompt.passkey(42, 5));
        assert_eq!(prompt.waiting(5), Some((Pairing::Passkey, 42)));
        let mut ended = Prompt::None;
        assert!(!ended.passkey(42, 5));
        assert_eq!(ended, Prompt::None);
        let mut compared = Prompt::Waiting(Pairing::Compare, 7, 0);
        assert!(!compared.passkey(42, 5));
        assert_eq!(compared.waiting(5), Some((Pairing::Compare, 7)));
    }

    #[test]
    fn a_prompt_whose_end_never_came_stops_waiting() {
        for pairing in [Pairing::Passkey, Pairing::Compare] {
            let prompt = Prompt::Waiting(pairing, 1, 1_000);
            assert_eq!(prompt.waiting(1_000 + PROMPT_MS - 1), Some((pairing, 1)));
            assert_eq!(prompt.waiting(1_000 + PROMPT_MS), None);
            assert_eq!(prompt.ends_ms(1_000 + PROMPT_MS - 1), Some(1_000 + PROMPT_MS));
        }
    }

    #[test]
    fn select_during_passkey_entry_leaves_the_radio_alone() {
        assert_eq!(action(Page::Bluetooth, short(Button::Select), Some(Pairing::Passkey)), None);
    }

    #[test]
    fn a_ring_is_seen_before_a_pairing() {
        for pairing in [Pairing::Passkey, Pairing::Compare] {
            assert_eq!(held(Some(pairing), false), Some(Page::Bluetooth));
            assert_eq!(held(Some(pairing), true), None);
        }
        assert_eq!(held(None, false), None);
        assert_eq!(held(None, true), None);
    }

    #[test]
    fn a_page_shown_restarts_the_wait_and_one_the_image_lacks_is_not_shown() {
        let mut nav = Nav::with(&Page::ALL);
        assert!(nav.show(Page::Bluetooth, 2_000));
        assert_eq!((nav.page(), nav.deadline_ms()), (Page::Bluetooth, Some(2_000 + TIMEOUT_MS)));
        assert!(!nav.show(Page::Bluetooth, 3_000));
        assert_eq!(nav.deadline_ms(), Some(3_000 + TIMEOUT_MS));
        let mut without = Nav::new();
        assert!(!without.show(Page::Bluetooth, 2_000));
        assert_eq!((without.page(), without.deadline_ms()), (Page::Face, None));
    }

    #[test]
    fn a_page_times_out_to_the_face_and_a_press_restarts_the_wait() {
        let mut nav = Nav::new();
        assert_eq!(nav.deadline_ms(), None);
        assert!(nav.press(short(Button::Down), 1_000));
        assert_eq!(nav.deadline_ms(), Some(1_000 + TIMEOUT_MS));
        assert!(!nav.tick(1_000 + TIMEOUT_MS - 1));
        assert!(!nav.press(short(Button::Select), 5_000));
        assert!(!nav.tick(1_000 + TIMEOUT_MS));
        assert_eq!(nav.page(), Page::Battery);
        assert!(nav.tick(5_000 + TIMEOUT_MS));
        assert_eq!(nav.page(), Page::Face);
        assert_eq!(nav.deadline_ms(), None);
        assert!(!nav.tick(u64::MAX));
    }

    #[test]
    fn the_face_never_times_out_and_the_deadline_saturates() {
        let mut nav = Nav::new();
        assert!(!nav.press(short(Button::Back), 0));
        assert!(!nav.tick(u64::MAX));
        nav.press(short(Button::Up), u64::MAX - 1);
        assert_eq!(nav.deadline_ms(), Some(u64::MAX));
        assert!(nav.tick(u64::MAX));
    }
}
