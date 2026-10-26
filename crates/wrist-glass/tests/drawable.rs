//! Every byte a built-in locale can put on a line has a glyph, so each byte is one glyph wide.

use wrist_glass::font::glyph;
use wrist_l10n::{DatePiece, Piece, TimePiece, BUILT_IN};

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

#[test]
fn every_built_in_string_is_in_the_font() {
    for l in BUILT_IN {
        let mut all: Vec<&'static str> = Vec::new();
        for m in l.messages {
            texts(m, &mut all);
        }
        all.extend(l.weekdays);
        all.extend(l.months);
        all.extend(l.date.iter().filter_map(|p| if let DatePiece::Text(t) = p { Some(*t) } else { None }));
        all.extend(l.time.iter().filter_map(|p| if let TimePiece::Text(t) = p { Some(*t) } else { None }));
        all.push("0123456789");
        for s in all {
            assert!(s.bytes().all(|b| b == b' ' || glyph(b) != [0; 5]), "{}: {s:?} has a byte the font cannot draw", l.tag);
        }
    }
}
