//! Every screen fits the glass, keeps its text off the hints and the ring, and says all it has
//! to say in glyphs its faces have.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use incised_text::Face;
use inscribed_disc::audit::off_glass;
use inscribed_disc::{Abgr2222, Surface};
use wrist_installer_ui::draw::{
    self, render, render_arcs, render_text, width_at, wrap, BLACK, DETAIL_LINES, HEIGHT, LARGE, SMALL, WIDTH,
};
use wrist_installer_ui::flow::{Input, HOLD_MS};
use wrist_installer_ui::installer::{
    Info, Installer, Screen, State, Verdict, HOLD_NAME, HOLD_PROMPT, TITLE,
};

const W: i32 = WIDTH as i32;
const H: i32 = HEIGHT as i32;

fn report(state: State, code: i32, crc: u32) -> Installer {
    let mut ui = Installer::new();
    ui.report(Verdict { state, code, crc }, 0);
    ui
}

/// Every screen the installer can show, with the widest values each can carry.
fn screens() -> Vec<(String, Screen)> {
    let mut out = vec![("checking".to_string(), Installer::new().screen(0))];
    for crc in [0, 0x1A2B_3C4D, 0x8888_8888, u32::MAX] {
        let ready = report(State::Ready, 0, crc);
        out.push((format!("ready {crc:08X}"), ready.screen(0)));
        let mut hold = ready;
        hold.input(Input::R1Press, 0);
        for t in [0, 1, HOLD_MS / 2, HOLD_MS - 1, HOLD_MS * 2] {
            out.push((format!("hold {crc:08X} at {t}"), hold.screen(t)));
        }
        let mut sent = hold;
        sent.input(Input::Tick, HOLD_MS);
        out.push((format!("sent {crc:08X}"), sent.screen(HOLD_MS)));
        out.push((format!("installing {crc:08X}"), report(State::Installing, 0, crc).screen(0)));
    }
    for state in [State::Isolated, State::NoImage, State::Refused, State::UnlockFailed, State::FlashTestFailed] {
        for code in [0, -1, -16, 0x3F, 0xFF, i32::MIN, i32::MAX] {
            out.push((format!("{state:?} {code}"), report(state, code, 0x1A2B_3C4D).screen(0)));
        }
    }
    out
}

fn frame(draw: impl FnOnce(&mut Surface<'_, Abgr2222>)) -> Vec<u8> {
    let mut fb = vec![BLACK.0; (W * H) as usize];
    // Unclipped, so whatever would run under the rim is still there to find.
    let mut surface = Surface::<Abgr2222>::rect(&mut fb, WIDTH, HEIGHT).unwrap();
    draw(&mut surface);
    fb
}

fn inked(fb: &[u8]) -> impl Iterator<Item = (i32, i32)> + '_ {
    fb.iter().enumerate().filter(|(_, &b)| b != BLACK.0).map(|(i, _)| (i as i32 % W, i as i32 / W))
}

#[test]
fn every_screen_is_on_the_glass() {
    for (name, screen) in screens() {
        let fb = frame(|s| render(s, &screen));
        let off: Vec<_> = off_glass(&fb, W, H, 1, BLACK.0).collect();
        assert!(off.is_empty(), "{name}: {} pixels under the rim, first {:?}", off.len(), off[0]);
    }
}

#[test]
fn text_keeps_clear_of_the_hints_and_the_ring() {
    for (name, screen) in screens() {
        let arcs = frame(|s| render_arcs(s, &screen));
        let text = frame(|s| render_text(s, &screen));
        let near = |x: i32, y: i32| {
            (-2..=2).any(|dy| {
                (-2..=2).any(|dx| {
                    let (ax, ay) = (x + dx, y + dy);
                    (0..W).contains(&ax) && (0..H).contains(&ay) && arcs[(ay * W + ax) as usize] != BLACK.0
                })
            })
        };
        let touching: Vec<_> = inked(&text).filter(|&(x, y)| near(x, y)).collect();
        assert!(touching.is_empty(), "{name}: text within 2 px of an arc at {touching:?}");
    }
}

/// Each string a screen draws, with the face it is drawn in.
fn every_text(screen: &Screen) -> Vec<(String, &'static Face)> {
    match screen {
        Screen::Info(Info { headline, caption, detail, hint, .. }) => vec![
            (TITLE.into(), SMALL),
            (headline.as_str().into(), draw::headline_face(headline.as_str())),
            ((*caption).into(), SMALL),
            (detail.replace('\n', " "), SMALL),
            ((*hint).into(), SMALL),
        ],
        Screen::Hold(hold) => vec![
            (HOLD_PROMPT.into(), SMALL),
            (HOLD_NAME.into(), LARGE),
            (hold.number.to_string(), draw::COUNTDOWN),
            (hold.crc.as_str().into(), draw::HOLD_CRC),
        ],
    }
}

#[test]
fn every_character_has_a_glyph() {
    for (name, screen) in screens() {
        for (text, face) in every_text(&screen) {
            assert_eq!(face.measure(&text).missing, 0, "{name}: {text:?}");
        }
    }
}

#[test]
fn details_wrap_whole_words_within_their_lines() {
    for (name, screen) in screens() {
        let Screen::Info(info) = screen else { continue };
        let mut lines = [""; DETAIL_LINES];
        let count = wrap(SMALL, info.detail, draw::at::DETAIL, draw::at::DETAIL_PITCH, &mut lines);
        assert!(count <= DETAIL_LINES, "{name}: {count} lines for {:?}", info.detail);
        let words: Vec<&str> = info.detail.split_whitespace().collect();
        let rewrapped: Vec<&str> = lines[..count].iter().flat_map(|l| l.split(' ')).collect();
        assert_eq!(rewrapped, words, "{name}: a word was split or lost");
        for (i, line) in (0..).zip(&lines[..count]) {
            let baseline = draw::at::DETAIL + i * draw::at::DETAIL_PITCH;
            let room = width_at(SMALL, baseline).min(draw::TEXT_WIDTH);
            assert!(SMALL.measure(line).advance <= room, "{name}: {line:?} is wider than {room}");
        }
    }
}

#[test]
fn the_detail_ends_above_the_hint() {
    let bottom = draw::at::DETAIL + (DETAIL_LINES as i32 - 1) * draw::at::DETAIL_PITCH + i32::from(SMALL.descent);
    assert!(bottom <= draw::at::HINT - i32::from(SMALL.ascent) + 2, "{bottom}");
}

#[test]
fn the_ring_fills_on_every_tick() {
    let ring = |held: u32| {
        let fb = frame(|s| {
            if let Some(arc) = draw::progress(draw::RING, held) {
                draw::arc(s, arc, Abgr2222::WHITE);
            }
        });
        inked(&fb).count()
    };
    assert_eq!(ring(0), 0);
    let mut last = 0;
    for held in (100..=HOLD_MS).step_by(100) {
        let now = ring(held);
        assert!(now > last, "{held} ms: {now} pixels, {last} a tick before");
        last = now;
    }
    assert_eq!(ring(HOLD_MS), ring(2 * HOLD_MS));
}
