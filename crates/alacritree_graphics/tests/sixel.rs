//! Sixel images through the patched vte into a real `Term`: where they land,
//! where they leave the cursor, and how a program detects them.

mod common;

use alacritree_graphics::frame::Band;
use alacritty_terminal::index::{Column, Line};
use common::{Pane, assert_rect};

/// A `width`x`height` red sixel image on a clear background, `height` a
/// multiple of 6.
fn sixel(width: u32, height: u32) -> String {
    let band = format!("!{width}~");
    let bands = vec![band; height as usize / 6].join("-");
    format!("\x1bP0;1q\"1;1;{width};{height}#1;2;100;0;0{bands}\x1b\\")
}

#[test]
fn a_sixel_image_lands_at_the_cursor_and_the_cursor_moves_below_it() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[2;3H{}", sixel(20, 30)));

    // 20x30 pixels over 10x20 cells: two columns, two rows.
    assert_rect(pane.quads()[0].dest, [2.0, 1.0, 4.0, 2.5]);
    assert_eq!(pane.cursor(), (2, 3));

    let frame = pane.frame();
    let run = &frame.runs()[0];
    assert_eq!(run.band, Band::UnderText);
    assert_eq!((run.pixels.width(), run.pixels.height()), (20, 30));
    assert!(run.pixels.rgba().chunks_exact(4).all(|pixel| pixel == [0xFF, 0, 0, 0xFF]));
}

#[test]
fn a_sixel_image_at_the_bottom_scrolls_the_screen_to_put_the_cursor_below_it() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[5;1Hab{}", sixel(10, 36)));

    // Two rows placed on the last line scroll the screen by two.
    assert_rect(pane.quads()[0].dest, [2.0, 2.0, 3.0, 3.8]);
    assert_eq!(pane.cursor(), (2, 4));
    assert_eq!(pane.term.grid()[Line(2)][Column(0)].c, 'a');
}

#[test]
fn in_display_mode_a_sixel_image_goes_top_left_and_leaves_the_cursor() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[?80h\x1b[3;4H{}", sixel(10, 180)));

    // Nine rows of image, clipped to the five the screen has.
    assert_rect(pane.quads()[0].dest, [0.0, 0.0, 1.0, 5.0]);
    assert_eq!(pane.cursor(), (3, 2));

    pane.feed("\x1b[?80$p\x1b[?80l\x1b[?80$p");
    assert_eq!(pane.replies(), ["\x1b[?80;1$y", "\x1b[?80;2$y"]);
}

#[test]
fn a_sixel_image_deletes_the_older_ones_it_covers() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[1;1H{}", sixel(10, 18)));
    pane.feed(format!("\x1b[1;2H{}", sixel(10, 18)));
    pane.feed(format!("\x1b[1;1H{}", sixel(30, 36)));
    assert_eq!(pane.image_count(), 1);

    // One that only overlaps stays.
    pane.feed(format!("\x1b[1;3H{}", sixel(30, 36)));
    assert_eq!(pane.image_count(), 2);
}

#[test]
fn an_empty_sixel_image_leaves_the_cursor() {
    let mut pane = Pane::new(10, 5);
    pane.feed("\x1b[2;2H\x1bPq??-\x1b\\");
    assert_eq!(pane.image_count(), 0);
    assert_eq!(pane.cursor(), (1, 1));
}

#[test]
fn primary_device_attributes_report_sixel() {
    let mut pane = Pane::new(10, 5);
    pane.feed("\x1b[c");
    assert_eq!(pane.replies(), ["\x1b[?62;4c"]);
}

#[test]
fn xtsmgraphics_reports_colour_registers_and_geometry() {
    let mut pane = Pane::new(10, 5);
    for (query, reply) in [
        ("\x1b[?1;1S", "\x1b[?1;0;256S"),
        ("\x1b[?1;3;1024S", "\x1b[?1;0;256S"),
        ("\x1b[?1;4S", "\x1b[?1;0;256S"),
        // 10x5 cells of 10x20 pixels.
        ("\x1b[?2;1S", "\x1b[?2;0;100;100S"),
        ("\x1b[?2;2S", "\x1b[?2;0;100;100S"),
        ("\x1b[?2;4S", "\x1b[?2;0;10000;10000S"),
        ("\x1b[?3;1S", "\x1b[?3;1S"),
        ("\x1b[?1;9S", "\x1b[?1;2S"),
    ] {
        pane.feed(query);
        assert_eq!(pane.replies(), [reply], "{query:?}");
    }
}
