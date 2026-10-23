//! The record a module's EEPROM starts with (`HARDWARE.md`, "Module records, read from one watch").

pub const RECORD_BYTES: usize = 100;

const MAGIC: [u8; 2] = *b"MI";
const VERSION: u8 = 1;

const NAME: core::ops::Range<usize> = 4..36;
const REVISION: core::ops::Range<usize> = 36..68;
const SERIAL: core::ops::Range<usize> = 68..99;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    name: [u8; 32],
    revision: [u8; 32],
    serial: [u8; 31],
}

impl ModuleInfo {
    #[must_use]
    pub fn parse(record: &[u8; RECORD_BYTES]) -> Option<Self> {
        if record[0..2] != MAGIC || record[2] != VERSION {
            return None;
        }
        let mut info = Self { name: [0; 32], revision: [0; 32], serial: [0; 31] };
        info.name.copy_from_slice(&record[NAME]);
        info.revision.copy_from_slice(&record[REVISION]);
        info.serial.copy_from_slice(&record[SERIAL]);
        Some(info)
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        text(&self.name)
    }

    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        text(&self.revision)
    }

    #[must_use]
    pub fn serial(&self) -> Option<&str> {
        text(&self.serial)
    }
}

fn text(field: &[u8]) -> Option<&str> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).ok()
}

fn printable(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_graphic())
}

const LABEL_MAX: usize = RECORD_BYTES - 1;

/// A NUL-terminated `<slot>_<name>-<revision>` label in place of an `MI` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    text: [u8; LABEL_MAX],
    len: u8,
    slot_end: u8,
    /// `len` when there is no revision.
    name_end: u8,
}

impl Label {
    #[must_use]
    pub fn parse(record: &[u8; RECORD_BYTES]) -> Option<Self> {
        let len = record.iter().position(|&b| b == 0)?;
        let body = core::str::from_utf8(&record[..len]).ok()?;
        let slot_end = body.find('_')?;
        let name_end = body.rfind('-').filter(|&i| i > slot_end).unwrap_or(len);
        let slot = &body[..slot_end];
        let name = &body[slot_end + 1..name_end];
        let revision = body.get(name_end + 1..).unwrap_or("");
        let slot_ok = !slot.is_empty() && slot.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        let name_ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric());
        let revision_ok = printable(revision) && (name_end == len || !revision.is_empty());
        if !(slot_ok && name_ok && revision_ok) {
            return None;
        }
        let mut text = [0u8; LABEL_MAX];
        text[..len].copy_from_slice(&record[..len]);
        #[allow(clippy::cast_possible_truncation)]
        Some(Self { text, len: len as u8, slot_end: slot_end as u8, name_end: name_end as u8 })
    }

    fn str(&self, range: core::ops::Range<usize>) -> &str {
        core::str::from_utf8(&self.text[range]).unwrap_or("")
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.str(0..usize::from(self.len))
    }

    #[must_use]
    pub fn slot(&self) -> &str {
        self.str(0..usize::from(self.slot_end))
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.str(usize::from(self.slot_end) + 1..usize::from(self.name_end))
    }

    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        let start = usize::from(self.name_end) + 1;
        (start <= usize::from(self.len)).then(|| self.str(start..usize::from(self.len)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Module(ModuleInfo),
    Label(Label),
    Blank,
    Unrecognised,
}

impl Record {
    #[must_use]
    pub fn parse(record: &[u8; RECORD_BYTES]) -> Self {
        if record.iter().all(|&b| b == 0xFF) {
            return Self::Blank;
        }
        if let Some(info) = ModuleInfo::parse(record) {
            let clean = info.name().is_some_and(|n| !n.is_empty() && printable(n))
                && info.revision().is_some_and(printable);
            return if clean { Self::Module(info) } else { Self::Unrecognised };
        }
        Label::parse(record).map_or(Self::Unrecognised, Self::Label)
    }

    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Module(info) => info.name(),
            Self::Label(label) => Some(label.name()),
            Self::Blank | Self::Unrecognised => None,
        }
    }

    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        match self {
            Self::Module(info) => info.revision(),
            Self::Label(label) => label.revision(),
            Self::Blank | Self::Unrecognised => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Expected,
    Other,
    Blank,
    Unrecognised,
    Unsteady,
    /// The caller's, when the EEPROM did not answer.
    Unread,
}

impl Verdict {
    #[must_use]
    pub fn refuses(self) -> bool {
        self == Self::Other
    }

    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Expected => "expected",
            Self::Other => "ANOTHER MODULE",
            Self::Blank => "blank",
            Self::Unrecognised => "unrecognised",
            Self::Unsteady => "unsteady (two reads differ)",
            Self::Unread => "not read",
        }
    }
}

#[must_use]
pub fn judge(first: &[u8; RECORD_BYTES], second: &[u8; RECORD_BYTES], expected: &str) -> Verdict {
    if first != second {
        return Verdict::Unsteady;
    }
    let record = Record::parse(first);
    match (&record, record.name()) {
        (Record::Blank, _) => Verdict::Blank,
        (_, None) => Verdict::Unrecognised,
        (_, Some(name)) if name.eq_ignore_ascii_case(expected) => Verdict::Expected,
        (_, Some(_)) => Verdict::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, revision: &str, serial: &str) -> [u8; RECORD_BYTES] {
        let mut r = [0u8; RECORD_BYTES];
        r[0..2].copy_from_slice(b"MI");
        r[2] = 1;
        r[NAME][..name.len()].copy_from_slice(name.as_bytes());
        r[REVISION][..revision.len()].copy_from_slice(revision.as_bytes());
        r[SERIAL][..serial.len()].copy_from_slice(serial.as_bytes());
        r
    }

    #[test]
    fn fields_stop_at_their_nul() {
        let info = ModuleInfo::parse(&record("LS012", "1.3", "")).unwrap();
        assert_eq!(info.name(), Some("LS012"));
        assert_eq!(info.revision(), Some("1.3"));
        assert_eq!(info.serial(), Some(""));
    }

    #[test]
    fn a_full_width_serial_is_read_whole() {
        let serial = "1234567890123456789012345678901";
        let info = ModuleInfo::parse(&record("U5A5-04G", "3.1", serial)).unwrap();
        assert_eq!(info.serial(), Some(serial));
    }

    #[test]
    fn a_blank_eeprom_has_no_record() {
        assert_eq!(ModuleInfo::parse(&[0xFF; RECORD_BYTES]), None);
    }

    fn view_label() -> [u8; RECORD_BYTES] {
        let mut r = [0xFFu8; RECORD_BYTES];
        r[..14].copy_from_slice(b"view_LS012-1_3");
        r[14] = 0;
        r
    }

    #[test]
    fn the_display_label_is_recognised_not_corrupt() {
        let r = view_label();
        assert_eq!(ModuleInfo::parse(&r), None);
        let Record::Label(label) = Record::parse(&r) else { panic!("not a label") };
        assert_eq!(label.text(), "view_LS012-1_3");
        assert_eq!(label.slot(), "view");
        assert_eq!(label.name(), "LS012");
        assert_eq!(label.revision(), Some("1_3"));
        assert_eq!(Record::parse(&r).name(), Some("LS012"));
    }

    #[test]
    fn a_label_without_a_revision_has_none() {
        let mut r = [0u8; RECORD_BYTES];
        r[..10].copy_from_slice(b"view_LS012");
        let Record::Label(label) = Record::parse(&r) else { panic!("not a label") };
        assert_eq!(label.name(), "LS012");
        assert_eq!(label.revision(), None);
    }

    #[test]
    fn loose_text_is_not_a_label() {
        for text in [&b"LS012-1_3"[..], b"_LS012-1", b"view_-1_3", b"View_LS012", b"view_LS 012", b"view_LS012-"] {
            let mut r = [0u8; RECORD_BYTES];
            r[..text.len()].copy_from_slice(text);
            assert_eq!(Record::parse(&r), Record::Unrecognised, "{}", core::str::from_utf8(text).unwrap());
        }
    }

    #[test]
    fn text_without_a_nul_is_not_a_label() {
        let mut r = [b'a'; RECORD_BYTES];
        r[4] = b'_';
        assert_eq!(Record::parse(&r), Record::Unrecognised);
    }

    #[test]
    fn records_of_the_three_mi_modules_parse() {
        for (name, rev) in [("BLUENRG2", "1.3"), ("AG3335M", "1.2"), ("PAH8316", "1.0")] {
            let r = Record::parse(&record(name, rev, ""));
            assert!(matches!(r, Record::Module(_)));
            assert_eq!(r.name(), Some(name));
            assert_eq!(r.revision(), Some(rev));
        }
    }

    #[test]
    fn a_garbled_mi_name_is_unrecognised() {
        assert_eq!(Record::parse(&record("", "1.3", "")), Record::Unrecognised);
        assert_eq!(Record::parse(&record("BLUE\x01NRG", "1.3", "")), Record::Unrecognised);
        let mut r = record("AG3335M", "1.2", "");
        r[NAME.start + 2] = 0xC3;
        assert_eq!(Record::parse(&r), Record::Unrecognised);
    }

    #[test]
    fn blank_and_zeroed_arrays() {
        assert_eq!(Record::parse(&[0xFF; RECORD_BYTES]), Record::Blank);
        assert_eq!(Record::parse(&[0; RECORD_BYTES]), Record::Unrecognised);
    }

    #[test]
    fn the_four_slots_as_read_are_expected() {
        let cases = [
            (view_label(), "LS012"),
            (record("BLUENRG2", "1.3", ""), "BLUENRG2"),
            (record("AG3335M", "1.2", ""), "AG3335M"),
            (record("PAH8316", "1.0", ""), "PAH8316"),
        ];
        for (r, expected) in cases {
            assert_eq!(judge(&r, &r, expected), Verdict::Expected);
            assert!(!judge(&r, &r, expected).refuses());
        }
    }

    #[test]
    fn another_module_in_a_slot_refuses() {
        let gnss = record("AG3335M", "1.2", "");
        assert_eq!(judge(&gnss, &gnss, "BLUENRG2"), Verdict::Other);
        assert!(judge(&gnss, &gnss, "BLUENRG2").refuses());
        let mut other = [0xFFu8; RECORD_BYTES];
        other[..15].copy_from_slice(b"view_LS013B-2_0");
        other[15] = 0;
        assert_eq!(judge(&other, &other, "LS012"), Verdict::Other);
    }

    #[test]
    fn a_name_differing_only_in_case_or_revision_is_expected() {
        let r = record("BlueNRG2", "9.9", "");
        assert_eq!(judge(&r, &r, "BLUENRG2"), Verdict::Expected);
    }

    #[test]
    fn not_knowing_never_refuses() {
        let blank = [0xFF; RECORD_BYTES];
        assert_eq!(judge(&blank, &blank, "LS012"), Verdict::Blank);
        let zero = [0; RECORD_BYTES];
        assert_eq!(judge(&zero, &zero, "LS012"), Verdict::Unrecognised);
        let a = record("AG3335M", "1.2", "");
        let b = record("BLUENRG2", "1.3", "");
        assert_eq!(judge(&a, &b, "PAH8316"), Verdict::Unsteady);
        for v in [Verdict::Blank, Verdict::Unrecognised, Verdict::Unsteady, Verdict::Unread] {
            assert!(!v.refuses());
        }
    }

    #[test]
    fn another_version_is_not_parsed() {
        let mut r = record("LS012", "1.3", "");
        r[2] = 2;
        assert_eq!(ModuleInfo::parse(&r), None);
    }
}
