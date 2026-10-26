//! The installer's screens as PNGs, for the docs, drawn by the code the watch runs:
//!
//!     cargo run -p wrist-installer-ui --features preview --example screens -- release/images
//!
//! Writes `installer-<scene>.png` into the directory given, by default the current one.

use std::path::PathBuf;

use inscribed_disc::preview::{to_rgba, Bezel, Options};
use inscribed_disc::{Abgr2222, Surface};
use wrist_installer_ui::draw::{render, HEIGHT, WIDTH};
use wrist_installer_ui::flow::Input;
use wrist_installer_ui::installer::{Installer, State, Verdict};

/// A made-up CRC; the real one is in each release's INSTALL-CRC.txt.
const CRC: u32 = 0x1A2B_3C4D;

fn verdict(state: State, code: i32) -> Verdict {
    Verdict { state, code, crc: CRC }
}

/// Each scene, and the time it is drawn at.
fn scenes() -> Vec<(&'static str, Installer, u32)> {
    let checking = Installer::new();
    let mut ready = Installer::new();
    ready.report(verdict(State::Ready, 0), 0);
    let mut hold = ready;
    hold.input(Input::R1Press, 1000);
    let mut installing = hold;
    installing.input(Input::Tick, 4000);
    installing.report(verdict(State::Installing, 0), 4010);
    let mut no_image = Installer::new();
    no_image.report(verdict(State::NoImage, 0), 0);
    let mut refused = Installer::new();
    refused.report(verdict(State::Refused, -3), 0);
    vec![
        ("checking", checking, 0),
        ("ready", ready, 0),
        // Partway into the 2, with the ring a little under half round.
        ("hold", hold, 2300),
        ("installing", installing, 4010),
        ("no-image", no_image, 0),
        ("refused", refused, 0),
    ]
}

fn main() {
    let dir = std::env::args().nth(1).map_or_else(|| PathBuf::from("."), PathBuf::from);
    // One image pixel a panel pixel, as `page dump` gives them.
    let options = Options { scale: 1, bezel: Bezel::Transparent };
    for (name, installer, now) in scenes() {
        let mut frame = vec![0u8; (WIDTH * HEIGHT) as usize];
        let mut surface = Surface::<Abgr2222>::round(&mut frame, WIDTH, HEIGHT).expect("a 240x240 frame");
        render(&mut surface, &installer.screen(now));
        let path = dir.join(format!("installer-{name}.png"));
        let rgba = to_rgba(&frame, WIDTH, HEIGHT, options);
        write_indexed(&path, &rgba, WIDTH * options.scale, HEIGHT * options.scale).expect("write the PNG");
        println!("{}", path.display());
    }
}

/// RGBA as a palette PNG: the panel shows at most 64 colours, and the bezel is one more.
fn write_indexed(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut indices = Vec::with_capacity(rgba.len() / 4);
    for &px in rgba.as_chunks::<4>().0 {
        let i = palette.iter().position(|&c| c == px).unwrap_or_else(|| {
            palette.push(px);
            palette.len() - 1
        });
        indices.push(u8::try_from(i).expect("at most 256 colours"));
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, w, h);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Best);
    encoder.set_palette(palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect::<Vec<_>>());
    encoder.set_trns(palette.iter().map(|c| c[3]).collect::<Vec<_>>());
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&indices)?;
    writer.finish()?;
    Ok(())
}
