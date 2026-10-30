//! Every string a built-in locale can put in a slot is in that slot's face, in both typefaces.

use wrist_glass::pages::{Faces, ATKINSON, POPPINS};
use wrist_glass::text::Font;
use wrist_l10n::{DatePiece, Locale, Msg, Piece, TimePiece, BUILT_IN};

const DIGITS: &str = "0123456789";

fn texts(pieces: &'static [Piece], out: &mut Vec<&'static str>) {
    for p in pieces {
        match p {
            Piece::Text(t) => out.push(t),
            Piece::Arg(_) => {}
            Piece::Select { cases, other, .. } => {
                for (_, case) in *cases {
                    texts(case, out);
                }
                texts(other, out);
            }
        }
    }
}

fn message(l: &Locale, msg: Msg, out: &mut Vec<&'static str>) {
    texts(l.messages[msg.parts().0], out);
}

fn assert_covers(slot: &str, font: &Font, strings: &[&str]) {
    for s in strings {
        let missing: String = font.uncovered(s).collect();
        assert!(missing.is_empty(), "{slot}: {s:?} lacks {missing:?}");
    }
}

/// A version's characters, and a build ID's once upper-cased.
const VERSION: &str = "0123456789.-+ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ.-_+";

#[test]
fn every_built_in_string_is_in_its_slot_s_face() {
    for faces in [&POPPINS, &ATKINSON] {
        let Faces { name, words, id, charge, time } = faces;
        for l in BUILT_IN {
            let mut all = vec![DIGITS];
            for m in l.messages {
                texts(m, &mut all);
            }
            all.extend(l.weekdays);
            all.extend(l.months);
            all.extend(l.date.iter().filter_map(|p| if let DatePiece::Text(t) = p { Some(*t) } else { None }));
            assert_covers(&format!("{} {name} words", l.tag), words, &all);

            let mut clock = vec![DIGITS];
            clock.extend(l.time.iter().filter_map(|p| if let TimePiece::Text(t) = p { Some(*t) } else { None }));
            message(l, Msg::FaceTimeUnset, &mut clock);
            assert_covers(&format!("{} {name} time", l.tag), time, &clock);

            let mut pct = vec![DIGITS];
            message(l, Msg::ChargePercent { pct: 0 }, &mut pct);
            message(l, Msg::ChargeUnset, &mut pct);
            assert_covers(&format!("{} {name} charge", l.tag), charge, &pct);

            let mut steps = vec![DIGITS];
            for n in [0, 1, 2, 5, 21] {
                message(l, Msg::StepsToday { steps: n }, &mut steps);
            }
            assert_covers(&format!("{} {name} steps", l.tag), id, &steps);
        }
        assert_covers(&format!("{name} version"), words, &[VERSION]);
        assert_covers(&format!("{name} id"), id, &[ID]);
    }
}

#[test]
fn a_figure_space_is_uncovered_unless_its_face_has_it() {
    for faces in [&POPPINS, &ATKINSON] {
        for font in [faces.words, faces.id, faces.charge, faces.time] {
            let missing = font.face.glyph('\u{2007}').is_none();
            assert_eq!(font.uncovered("\u{2007}").count() == 1, missing, "{} {}", faces.name, font.face.px);
        }
    }
}
