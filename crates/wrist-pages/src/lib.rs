#![no_std]

//! Which page is on the glass, and where each press moves it.
//!
//! Pure state: the image hands it presses (from the buttons or the console's `press`) and the
//! time, and draws [`Nav::page`]. From the face, Down and Up step through the image's pages (the
//! Bluetooth page only in an image with Bluetooth); on another page they keep stepping, Back
//! returns to the face, and after [`TIMEOUT_MS`] without a press the face comes back by itself.
//! Select and long presses move nothing; on the Bluetooth page they are [`action`]s.
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

/// The action `press` takes on `page` while `pairing`: during a numeric comparison Select says
/// the numbers match and Back that they don't, during passkey entry nothing does anything, and
/// otherwise Select switches the radio. No hold answers: a long hold resets the watch.
#[must_use]
pub const fn action(page: Page, press: Press, pairing: Option<Pairing>) -> Option<Action> {
    match (page, press.button, press.long, pairing) {
        (Page::Bluetooth, Button::Select, false, Some(Pairing::Compare)) => Some(Action::Compare(true)),
        (Page::Bluetooth, Button::Back, false, Some(Pairing::Compare)) => Some(Action::Compare(false)),
        (Page::Bluetooth, Button::Select, false, None) => Some(Action::ToggleBluetooth),
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
    fn select_during_passkey_entry_leaves_the_radio_alone() {
        assert_eq!(action(Page::Bluetooth, short(Button::Select), Some(Pairing::Passkey)), None);
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
