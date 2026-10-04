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
fn a_sixel_image_taller_than_the_screen_scrolls_whole_into_view() {
    let mut pane = Pane::new(10, 5);
    pane.feed(sixel(10, 240));

    // Twelve rows from line 0: the screen scrolls until the cursor is below
    // the last one.
    assert_rect(pane.quads()[0].dest, [0.0, -8.0, 1.0, 4.0]);
    assert_eq!(pane.cursor(), (0, 4));
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
fn text_printed_over_a_sixel_image_erases_only_the_cells_it_lands_in() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[1;1H{}", sixel(30, 40)));

    // Three columns by two rows; the top middle cell gets text.
    pane.feed("\x1b[1;2HX");

    let quads: Vec<_> = pane.quads().iter().map(|quad| (quad.dest, quad.src)).collect();
    assert_eq!(quads, [
        ([0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 10.0, 20.0]),
        ([2.0, 0.0, 3.0, 1.0], [20.0, 0.0, 30.0, 20.0]),
        ([0.0, 1.0, 3.0, 2.0], [0.0, 20.0, 30.0, 40.0]),
    ]);
}

#[test]
fn spaces_printed_over_every_cell_of_a_sixel_image_remove_it() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[2;3H{}", sixel(20, 30)));

    pane.feed("\x1b[2;3H  \x1b[3;3H ");
    assert_eq!(pane.image_count(), 1);
    pane.feed(" ");
    assert_eq!(pane.image_count(), 0);
}

#[test]
fn a_sixel_image_clipped_by_a_scroll_region_goes_once_its_visible_cells_are_printed_over() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[2;1H{}", sixel(10, 60)));
    pane.feed("\x1b[2;1HX");

    // Scrolling the region up by two clips the image to its last row.
    pane.feed("\x1b[2;5r\x1b[2S\x1b[2;1HX");

    assert!(pane.quads().is_empty());
    assert_eq!(pane.image_count(), 0);
}

#[test]
fn a_sixel_image_goes_once_its_cells_at_a_new_cell_size_are_printed_over() {
    let mut pane = Pane::new(10, 5);
    pane.feed(format!("\x1b[1;1H{}", sixel(40, 36)));
    pane.feed("\x1b[1;1HX");

    // Four columns by two rows become two columns by one row.
    pane.term.graphics_mut().set_cell_pixels(20, 40);
    pane.feed("\x1b[1;1HXX");

    assert!(pane.quads().is_empty());
    assert_eq!(pane.image_count(), 0);
}

#[test]
fn after_the_cells_shrink_text_erases_one_new_cell_of_a_sixel_image() {
    let mut pane = Pane::with(10, 5, 5, (20, 40));
    pane.feed(format!("\x1b[1;1H{}", sixel(40, 36)));
    pane.feed("\x1b[1;1HX");

    // Two columns by one row become four columns by two rows, the left
    // half still erased.
    pane.term.graphics_mut().set_cell_pixels(10, 20);
    pane.feed("\x1b[1;3HX");

    let quads = pane.quads();
    assert_eq!(quads.len(), 2);
    assert_rect(quads[0].dest, [3.0, 0.0, 4.0, 1.0]);
    assert_rect(quads[0].src, [30.0, 0.0, 40.0, 20.0]);
    assert_rect(quads[1].dest, [2.0, 1.0, 4.0, 1.8]);
    assert_rect(quads[1].src, [20.0, 20.0, 40.0, 36.0]);
}

#[test]
fn decaln_erases_sixel_images_on_screen_and_keeps_kitty_placements() {
    let mut pane = Pane::new(10, 5);
    pane.message("a=T,f=24,i=1,s=10,v=20,C=1", vec![0; 10 * 20 * 3]);
    pane.feed(format!("\x1b[3;5H{}", sixel(30, 40)));

    pane.feed("\x1b#8");

    assert_eq!(pane.image_count(), 1);
    let quads = pane.quads();
    assert_eq!(quads.len(), 1);
    assert_rect(quads[0].dest, [0.0, 0.0, 1.0, 1.0]);
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
