#![no_std]

//! Which page is on the glass, and where each press moves it.
//!
//! Pure state: the image hands it presses (from the buttons or the console's `press`) and the
//! time, and draws [`Nav::page`]. From the face, Down and Up step through [`Page::ALL`]; on
//! another page they keep stepping, Back returns to the face, and after [`TIMEOUT_MS`]
//! without a press the face comes back by itself. Select and long presses move nothing yet.
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

    /// `up`, `down`, `back` or `select`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == word)
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
    /// The version and the build ID.
    About,
}

impl Page {
    pub const ALL: [Self; 3] = [Self::Face, Self::Battery, Self::About];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Face => "face",
            Self::Battery => "battery",
            Self::About => "about",
        }
    }

    const fn position(self) -> usize {
        match self {
            Self::Face => 0,
            Self::Battery => 1,
            Self::About => 2,
        }
    }

    /// `by` pages on, wrapping; `ALL.len() - 1` steps back one.
    const fn step(self, by: usize) -> Self {
        Self::ALL[(self.position() + by) % Self::ALL.len()]
    }
}

/// How long a page other than the face stays without a press.
pub const TIMEOUT_MS: u64 = 10_000;

/// Where the wearer is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Nav {
    page: Page,
    /// When the last press arrived, in the image's milliseconds.
    pressed_at_ms: u64,
}

impl Nav {
    /// On the face.
    #[must_use]
    pub const fn new() -> Self {
        Self { page: Page::Face, pressed_at_ms: 0 }
    }

    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// The page `press` leads to from `page`.
    #[must_use]
    pub const fn next(page: Page, press: Press) -> Page {
        if press.long {
            return page;
        }
        match (page, press.button) {
            (_, Button::Down) => page.step(1),
            (_, Button::Up) => page.step(Page::ALL.len() - 1),
            (_, Button::Back) => Page::Face,
            (_, Button::Select) => page,
        }
    }

    /// Apply `press` at `now_ms`; true when the page changed. Every press restarts the
    /// timeout.
    pub fn press(&mut self, press: Press, now_ms: u64) -> bool {
        self.pressed_at_ms = now_ms;
        let before = self.page;
        self.page = Self::next(before, press);
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
            assert_eq!(Button::parse(b.name()), Some(b));
        }
        assert_eq!(Button::from_index(4), None);
        assert_eq!(Button::parse("Up"), None);
        assert_eq!(Button::parse(""), None);
    }

    #[test]
    fn every_short_press_from_every_page() {
        use Button::{Back, Down, Select, Up};
        use Page::{About, Battery, Face};
        let table = [
            (Face, Up, About),
            (Face, Down, Battery),
            (Face, Back, Face),
            (Face, Select, Face),
            (Battery, Up, Face),
            (Battery, Down, About),
            (Battery, Back, Face),
            (Battery, Select, Battery),
            (About, Up, Battery),
            (About, Down, Face),
            (About, Back, Face),
            (About, Select, About),
        ];
        assert_eq!(table.len(), Page::ALL.len() * Button::ALL.len());
        for (from, button, to) in table {
            assert_eq!(Nav::next(from, short(button)), to, "{from:?} + {button:?}");
        }
    }

    #[test]
    fn long_presses_move_nothing() {
        for page in Page::ALL {
            for button in Button::ALL {
                assert_eq!(Nav::next(page, Press { button, long: true }), page);
            }
        }
    }

    #[test]
    fn down_visits_every_page_and_comes_home() {
        let mut nav = Nav::new();
        let mut seen = [false; Page::ALL.len()];
        for t in 0..Page::ALL.len() as u64 {
            seen[nav.page().position()] = true;
            nav.press(short(Button::Down), t);
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(nav.page(), Page::Face);
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
