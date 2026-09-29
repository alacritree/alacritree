//! What zellij and herdr send the terminal they run in, built byte for byte
//! from their format strings: zellij's `zellij-server/src/output/mod.rs`
//! (v0.45.1) and herdr's `src/kitty_graphics.rs` (v0.9.1).

mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Pane, TempFile, assert_rect};

/// zellij's `emit_kitty_transmit`: the base64 cut into 4096-character parts.
fn zellij_transmit(id: u32, width: usize, height: usize, rgba: &[u8]) -> String {
    let b64 = STANDARD.encode(rgba);
    let parts: Vec<&str> =
        b64.as_bytes().chunks(4096).map(|part| std::str::from_utf8(part).unwrap()).collect();
    let last = parts.len() - 1;
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            let more = if last == 0 { 0 } else { 1 };
            out += &format!(
                "\u{1b}_Ga=t,q=2,f=32,t=d,i={id},s={width},v={height},m={more};{part}\u{1b}\\"
            );
        } else {
            let more = if index == last { 0 } else { 1 };
            out += &format!("\u{1b}_Gq=2,m={more};{part}\u{1b}\\");
        }
    }
    out
}

/// zellij's `vte_goto_instruction` and placement.
#[allow(clippy::too_many_arguments)]
fn zellij_place(
    id: u32,
    placement: u32,
    (cell_x, cell_y): (usize, usize),
    (x, y, w, h): (u32, u32, u32, u32),
    (offset_x, offset_y): (u32, u32),
    z: i32,
) -> String {
    format!(
        "\u{1b}[{};{}H\u{1b}[m\u{1b}_Ga=p,q=2,i={id},p={placement},x={x},y={y},w={w},h={h},\
         X={offset_x},Y={offset_y},z={z},C=1\u{1b}\\",
        cell_y + 1,
        cell_x + 1,
    )
}

#[test]
fn zellij_places_crops_moves_and_deletes() {
    let mut pane = Pane::with(40, 12, 100, (8, 16));
    let rgba: Vec<u8> = (0..64 * 48).flat_map(|i| [i as u8, 0, 0, 255]).collect();
    let transmit = zellij_transmit(1, 64, 48, &rgba);
    assert_eq!(transmit.matches("\u{1b}_G").count(), 4, "zellij splits it in four");
    pane.feed(&transmit);
    pane.feed("\x1b[1;1H");

    // A crop of 32x24 pixels at (16, 8), shown at its pixel size.
    pane.feed(zellij_place(1, 1, (3, 2), (16, 8, 32, 24), (0, 0), 0));
    pane.feed(zellij_place(1, 2, (20, 6), (0, 0, 64, 48), (4, 2), -1));
    assert!(pane.replies().is_empty(), "q=2 silences everything");
    assert_eq!(pane.cursor(), (20, 6), "C=1 leaves the cursor at the goto");

    let quads = pane.quads();
    assert_eq!(quads.len(), 2);
    assert_rect(quads[0].dest, [20.5, 6.125, 28.5, 9.125]);
    assert_rect(quads[0].src, [0.0, 0.0, 64.0, 48.0]);
    assert_rect(quads[1].dest, [3.0, 2.0, 7.0, 3.5]);
    assert_rect(quads[1].src, [16.0, 8.0, 48.0, 32.0]);

    // Re-emitting a placement id moves that placement.
    pane.feed(zellij_place(1, 1, (5, 4), (16, 8, 32, 24), (0, 0), 0));
    assert_eq!(pane.graphics().placement_count(), 2);
    assert_rect(pane.quads()[1].dest, [5.0, 4.0, 9.0, 5.5]);

    pane.feed("\u{1b}_Ga=d,q=2,d=i,i=1,p=2\u{1b}\\");
    assert_eq!(pane.graphics().placement_count(), 1);
    assert_eq!(pane.image_count(), 1);
    pane.feed("\u{1b}_Ga=d,q=2,d=I,i=1\u{1b}\\");
    assert_eq!(pane.image_count(), 0);
    assert!(pane.replies().is_empty());
}

/// herdr's `encode_kitty_data`: each 3072-byte chunk encoded on its own.
fn herdr_data(control: &str, data: &[u8]) -> String {
    let mut chunks = data.chunks(3072).peekable();
    let first = chunks.next().unwrap();
    let more = u8::from(chunks.peek().is_some());
    let mut out = format!("\x1b_G{control},m={more};{}\x1b\\", STANDARD.encode(first));
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        out += &format!("\x1b_Gm={more};{}\x1b\\", STANDARD.encode(chunk));
    }
    out
}

#[test]
fn herdr_transmits_places_crops_and_deletes() {
    let mut pane = Pane::with(40, 12, 100, (8, 16));
    let rgba: Vec<u8> = (0..40 * 30).flat_map(|i| [0, i as u8, 0, 255]).collect();

    // encode_transmit_and_display, cropped to the bottom 20 rows and
    // stretched over 4x2 cells.
    let mut control = "a=T,t=d,f=32,s=40,v=30,i=900001,p=77,c=4,r=2,z=0,C=1,q=2".to_owned();
    control += ",y=10,h=20";
    pane.feed(format!("\x1b[{};{}H", 3 + 1, 5 + 1));
    let stream = herdr_data(&control, &rgba);
    assert_eq!(stream.matches("\x1b_G").count(), 2);
    pane.feed(&stream);
    assert!(pane.replies().is_empty());
    assert_eq!(pane.cursor(), (5, 3));
    let quads = pane.quads();
    assert_eq!(quads.len(), 1);
    assert_rect(quads[0].dest, [5.0, 3.0, 9.0, 5.0]);
    assert_rect(quads[0].src, [0.0, 10.0, 40.0, 30.0]);

    // encode_display_placement: a second placement with an empty payload.
    pane.feed("\x1b[8;2H\x1b_Ga=p,i=900001,p=78,c=2,r=1,z=-1,C=1,q=2,x=10,w=20;\x1b\\");
    let quads = pane.quads();
    assert_eq!(quads.len(), 2);
    assert_rect(quads[0].dest, [1.0, 7.0, 3.0, 8.0]);
    assert_rect(quads[0].src, [10.0, 0.0, 30.0, 30.0]);

    pane.feed("\x1b_Ga=d,d=i,i=900001,p=77,q=2;\x1b\\");
    assert_eq!(pane.graphics().placement_count(), 1);
    pane.feed("\x1b_Ga=d,d=I,i=900001,q=2;\x1b\\");
    assert_eq!(pane.image_count(), 0);

    // encode_upload_image, then a display of the uploaded image.
    pane.feed(herdr_data("a=t,t=d,f=32,s=40,v=30,i=900002,q=2", &rgba));
    pane.feed("\x1b[1;1H\x1b_Ga=p,i=900002,p=5,c=5,r=2,z=0,C=1,q=2;\x1b\\");
    assert_rect(pane.quads()[0].dest, [0.0, 0.0, 5.0, 2.0]);
    assert!(pane.replies().is_empty());
}

#[test]
fn herdr_regular_file_transmission_restores_the_cursor() {
    let mut pane = Pane::with(40, 12, 100, (8, 16));
    let rgba: Vec<u8> = (0..3 * 2).flat_map(|_| [1, 2, 3, 255]).collect();
    let file = TempFile::new("herdr-frame", &rgba);

    // encode_kitty_regular_file
    let control = "a=T,f=32,s=3,v=2,i=42,p=7,c=3,r=2,z=0,C=1,q=0";
    let payload = STANDARD.encode(file.path());
    pane.feed(format!("\x1b7\x1b[2;3H\x1b_G{control},t=f;{payload}\x1b\\\x1b8"));

    assert_eq!(pane.replies(), ["\x1b_Gi=42,p=7;OK\x1b\\"]);
    assert_eq!(pane.cursor(), (0, 0));
    assert_rect(pane.quads()[0].dest, [2.0, 1.0, 5.0, 3.0]);
    assert!(file.0.exists(), "t=f leaves the file alone");
}
