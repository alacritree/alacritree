//! Ghostty's graphics tests for chunking, deletes, scroll-margin clipping,
//! insert and delete line, and cursor movement, ported from
//! `src/terminal/kitty/graphics_exec.zig` and `graphics_storage.zig`.  Each
//! test names the one it ports.  Where Ghostty adds images and placements
//! to its storage directly, these send the commands that make them.

mod common;

use alacritty_terminal::grid::Dimensions;
use common::{Pane, assert_rect};

/// An RGB image of `width`x`height` pixels, transmitted as `id`.
fn transmit(pane: &mut Pane, id: u32, width: u32, height: u32) {
    let control = format!("a=t,f=24,i={id},s={width},v={height}");
    let reply = pane.message(&control, vec![0; (width * height * 3) as usize]);
    assert_eq!(reply.as_deref(), Some("OK"));
}

/// Place image `id` as placement `p` at screen `(x, y)`, leaving the cursor
/// where it was.
fn place(pane: &mut Pane, id: u32, p: u32, (x, y): (usize, usize), keys: &str) {
    let cursor = pane.cursor();
    pane.feed(format!("\x1b[{};{}H", y + 1, x + 1));
    let reply = pane.message(&format!("a=p,i={id},p={p},C=1{keys}"), b"");
    assert_eq!(reply.as_deref(), Some("OK"));
    pane.feed(format!("\x1b[{};{}H", cursor.1 + 1, cursor.0 + 1));
}

/// A 10x6 terminal of 10x10 pixel cells, Ghostty's margin test screen.
fn margins_pane() -> Pane {
    Pane::with(10, 6, 100, (10, 10))
}

/// The one quad's top row and source rectangle.
fn only_placement(pane: &mut Pane) -> (f32, [f32; 4]) {
    let quads = pane.quads();
    assert_eq!(quads.len(), 1, "{quads:?}");
    (quads[0].dest[1], quads[0].src)
}

// "kittygfx chunked success response uses initial identifiers"
#[test]
fn chunked_success_response_uses_initial_identifiers() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(pane.send("a=t,f=24,s=1,v=2,I=93,p=7,m=1", [0; 3]), None);
    assert_eq!(pane.send("m=0", [0; 3]).as_deref(), Some("\x1b_Gi=1,I=93,p=7;OK\x1b\\"));
}

// "kittygfx chunked error response uses initial identifiers", with kitty's
// ENODATA where Ghostty says EINVAL.
#[test]
fn chunked_error_response_uses_initial_identifiers() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(pane.send("a=t,f=24,s=1,v=1,i=41,p=7,m=1", [0]), None);
    assert_eq!(
        pane.send("m=0", [0]).as_deref(),
        Some("\x1b_Gi=41,p=7;ENODATA:Insufficient image data: 2 < 3\x1b\\")
    );
}

// "kittygfx more chunks with q=1", "with q=0" and "with chunk increasing q"
#[test]
fn quiet_is_inherited_by_later_chunks_unless_they_raise_it() {
    let mut pane = Pane::new(5, 5);
    let pixels = [0xff; 3];
    assert_eq!(pane.send("a=T,f=24,t=d,i=1,s=1,v=2,c=10,r=1,m=1,q=1", pixels), None);
    assert_eq!(pane.send("m=0", pixels), None);

    assert_eq!(pane.send("a=t,f=24,t=d,s=1,v=2,c=10,r=1,m=1,i=1,q=0", pixels), None);
    assert_eq!(pane.message("m=0", pixels).as_deref(), Some("OK"));

    assert_eq!(pane.send("a=t,f=24,t=d,s=1,v=2,c=10,r=1,m=1,i=1,q=0", pixels), None);
    assert_eq!(pane.send("m=0,q=1", pixels), None);
}

// "kittygfx delete aborts chunked image load"
#[test]
fn delete_aborts_a_chunked_load() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(pane.send("a=t,f=24,s=1,v=2,i=1,m=1", [0; 3]), None);
    assert_eq!(pane.send("a=d", b""), None);
    assert_eq!(pane.send("a=t,f=24,s=1,v=2,i=1,m=1", [0; 3]), None);
    assert_eq!(pane.message("m=0", [0; 3]).as_deref(), Some("OK"));
    assert_eq!(pane.pixels(1).len(), 2 * 4);
}

// "kittygfx query validates image data", with kitty's ENODATA
#[test]
fn query_validates_image_data() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(
        pane.send("a=q,f=24,s=1,v=1,i=31", [0, 0]).as_deref(),
        Some("\x1b_Gi=31;ENODATA:Insufficient image data: 2 < 3\x1b\\")
    );
    assert_eq!(pane.image_count(), 0);
}

// "kittygfx valid query does not replace or store image"
#[test]
fn valid_query_does_not_replace_or_store_an_image() {
    let mut pane = Pane::new(5, 5);
    assert_eq!(pane.message("a=t,f=24,s=1,v=1,i=31", [255, 0, 0]).as_deref(), Some("OK"));
    assert_eq!(pane.message("a=q,f=24,s=1,v=1,i=31", [0, 0, 0]).as_deref(), Some("OK"));
    assert_eq!(pane.image_count(), 1);
    assert_eq!(pane.pixels(31), [255, 0, 0, 255]);
}

// "kittygfx number-based transmission assigns smallest free id"
#[test]
fn a_number_gets_the_smallest_free_id() {
    let mut pane = Pane::new(5, 5);
    let ids = |pane: &mut Pane, control: &str| {
        common::code_and_ids(&pane.send(control, [255, 0, 0]).unwrap())
    };
    assert_eq!(ids(&mut pane, "a=t,f=24,s=1,v=1,I=42"), ("OK".into(), "i=1,I=42".into()));
    ids(&mut pane, "a=t,f=24,s=1,v=1,i=2");
    assert_eq!(ids(&mut pane, "a=t,f=24,s=1,v=1,I=43").1, "i=3,I=43");
    pane.send("a=d,d=I,i=1", b"");
    assert_eq!(ids(&mut pane, "a=t,f=24,s=1,v=1,I=44").1, "i=1,I=44");
    assert_eq!(pane.image_count(), 3);
}

// "kittygfx display clamps cell offsets"
#[test]
fn cell_offsets_are_clamped_to_the_cell() {
    let mut pane = Pane::with(5, 5, 100, (10, 20));
    let reply = pane.message("a=T,t=d,f=24,i=1,s=1,v=1,c=2,r=1,X=99,Y=99,C=1", [0; 3]);
    assert_eq!(reply.as_deref(), Some("OK"));
    let [left, top, right, bottom] = pane.quads()[0].dest;
    assert_rect([left, top, right, bottom], [0.9, 0.95, 2.0, 1.0]);
    assert_eq!((((right - left) * 10.0).round(), ((bottom - top) * 20.0).round()), (11.0, 1.0));
}

// "kittygfx placement bounds cursor movement for untrusted dimensions"
#[test]
fn cursor_movement_is_bounded_for_untrusted_dimensions() {
    let mut pane = Pane::with(5, 5, 100, (10, 20));
    let control = format!("a=T,t=d,f=24,i=1,s=1,v=1,c={0},r={0}", u32::MAX);
    assert_eq!(pane.message(&control, [0xff; 3]).as_deref(), Some("OK"));
    assert_eq!(pane.graphics().placement_count(), 1);
    // Four rows reach the bottom, then scrolling stops one screen later.
    assert_eq!(pane.term.grid().history_size(), 5);
}

// "kittygfx placement moves cursor past a tall image"
#[test]
fn cursor_moves_past_a_tall_image() {
    let mut pane = Pane::with(5, 5, 100, (10, 20));
    assert_eq!(pane.message("a=t,t=d,f=24,i=1,s=1,v=1", [0xff; 3]).as_deref(), Some("OK"));
    assert_eq!(pane.message("a=p,i=1,p=1,c=5,r=8", b"").as_deref(), Some("OK"));
    assert_eq!(pane.message("a=p,i=1,p=2,C=1", b"").as_deref(), Some("OK"));
    let tops: Vec<f32> = pane.quads().iter().map(|quad| quad.dest[1]).collect();
    assert_eq!(tops, [-4.0, 4.0]);
}

// "storage: scroll margins placement inside region scrolls and clips at top"
#[test]
fn margin_scroll_moves_and_clips_a_placement_inside_the_region() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 20);
    place(&mut pane, 1, 1, (0, 2), "");
    pane.feed("\x1b[2;5r\x1b[5;1H");

    pane.feed("\x1bD");
    assert_eq!(only_placement(&mut pane), (1.0, [0.0, 0.0, 10.0, 20.0]));
    pane.feed("\x1bD");
    assert_eq!(only_placement(&mut pane), (1.0, [0.0, 10.0, 10.0, 20.0]));
    pane.feed("\x1bD");
    assert_eq!(pane.graphics().placement_count(), 0);
    assert_eq!(pane.image_count(), 1);
}

// "storage: scroll margins placement straddling region does not move"
#[test]
fn margin_scroll_leaves_a_placement_straddling_the_region() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 30);
    place(&mut pane, 1, 1, (0, 0), "");
    pane.feed("\x1b[1;2r\x1b[2;1H\x1bD");
    assert_eq!(only_placement(&mut pane), (0.0, [0.0, 0.0, 10.0, 30.0]));
}

// "storage: scroll margins placement below region does not move"
#[test]
fn margin_scroll_leaves_a_placement_below_the_region() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 10);
    place(&mut pane, 1, 1, (0, 5), "");
    pane.feed("\x1b[1;4r\x1b[4;1H\x1bD\x1bD");
    assert_eq!(only_placement(&mut pane).0, 5.0);
}

// "storage: scroll margins reverse index clips at bottom"
#[test]
fn reverse_index_clips_at_the_bottom_margin() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 20);
    place(&mut pane, 1, 1, (0, 3), "");
    pane.feed("\x1b[2;5r\x1b[2;1H\x1bM");
    assert_eq!(only_placement(&mut pane), (4.0, [0.0, 0.0, 10.0, 10.0]));
    pane.feed("\x1bM");
    assert_eq!(pane.graphics().placement_count(), 0);
}

// "storage: scroll margins scaled placement clips proportionally"
#[test]
fn a_scaled_placement_clips_proportionally() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 40);
    place(&mut pane, 1, 1, (0, 1), ",c=1,r=2");
    pane.feed("\x1b[2;3r\x1b[3;1H\x1bD");
    let quads = pane.quads();
    assert_rect(quads[0].dest, [0.0, 1.0, 1.0, 2.0]);
    assert_rect(quads[0].src, [0.0, 20.0, 10.0, 40.0]);
}

// "storage: scroll margins insert/delete lines do not move placements"
#[test]
fn insert_and_delete_line_do_not_move_images() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 10);
    place(&mut pane, 1, 1, (0, 2), "");
    pane.feed("\x1b[2;5r\x1b[2;1H");
    pane.feed("\x1b[M");
    assert_eq!(only_placement(&mut pane).0, 2.0);
    pane.feed("\x1b[L");
    assert_eq!(only_placement(&mut pane).0, 2.0);
}

// "storage: scroll without margins moves placement into scrollback"
#[test]
fn a_scroll_without_margins_moves_the_placement_into_scrollback() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 20);
    place(&mut pane, 1, 1, (0, 0), "");
    pane.feed("\x1b[6;1H\x1bD");
    assert_eq!(only_placement(&mut pane).0, -1.0);
    let frame = pane.frame_at(1);
    assert_eq!(frame.quads()[0].dest[1], 0.0);
}

// "storage: scroll margins large scroll deletes inside placement"
#[test]
fn a_large_margin_scroll_deletes_a_placement_inside() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 10);
    place(&mut pane, 1, 1, (0, 2), "");
    pane.feed("\x1b[2;5r\x1b[10S");
    assert_eq!(pane.graphics().placement_count(), 0);
}

// "storage: scroll margins straddling placement pin inside region restored"
#[test]
fn a_placement_hanging_below_the_region_does_not_move() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 20);
    place(&mut pane, 1, 1, (0, 4), "");
    pane.feed("\x1b[2;5r\x1b[5;1H\x1bD");
    assert_eq!(only_placement(&mut pane), (4.0, [0.0, 0.0, 10.0, 20.0]));
}

// "storage: scroll margins multi-line scroll up with scrollback"
#[test]
fn a_region_at_the_top_clips_instead_of_scrolling_into_history() {
    let mut pane = margins_pane();
    transmit(&mut pane, 1, 10, 10);
    place(&mut pane, 1, 1, (0, 2), "");
    pane.feed("\x1b[1;4r\x1b[2S");
    assert_eq!(only_placement(&mut pane).0, 0.0);
    pane.feed("\x1b[2S");
    assert_eq!(pane.graphics().placement_count(), 0);
}

/// A 2x2 terminal of 1x1 pixel cells with scrollback, Ghostty's delete test
/// screen.
fn small_pane() -> Pane {
    Pane::with(2, 2, 100, (1, 1))
}

// "storage: delete all visible placements preserves scrollback"
#[test]
fn delete_all_keeps_placements_in_scrollback() {
    let mut pane = small_pane();
    transmit(&mut pane, 1, 1, 1);
    place(&mut pane, 1, 1, (0, 0), "");
    pane.feed("\x1b[S");
    place(&mut pane, 1, 2, (0, 0), "");
    pane.send("a=d,d=A", b"");
    assert_eq!(pane.graphics().placement_count(), 1);
    assert_eq!(pane.image_count(), 1);
    assert_eq!(pane.frame_at(1).quads()[0].dest[1], 0.0);
}

// "storage: delete all includes placements spanning into active area"
#[test]
fn delete_all_includes_placements_reaching_the_screen() {
    let mut pane = small_pane();
    transmit(&mut pane, 1, 1, 2);
    place(&mut pane, 1, 1, (0, 0), "");
    pane.feed("\x1b[S");
    pane.send("a=d,d=A", b"");
    assert_eq!(pane.graphics().placement_count(), 0);
    assert_eq!(pane.image_count(), 0);
}

// "storage: erase display preserves scrollback and reclaims unplaced images"
#[test]
fn erase_display_keeps_scrollback_and_frees_unplaced_images() {
    let mut pane = small_pane();
    transmit(&mut pane, 1, 1, 1);
    place(&mut pane, 1, 1, (0, 0), "");
    pane.feed("\x1b[S");
    transmit(&mut pane, 2, 1, 1);
    place(&mut pane, 2, 1, (0, 0), "");
    transmit(&mut pane, 3, 1, 1);
    pane.feed("\x1b[2J");
    assert_eq!(pane.graphics().placement_count(), 1);
    assert_eq!(pane.image_count(), 1);
    pane.settle();
    assert!(pane.graphics().pixels(1).is_some());
}

// "storage: uppercase id delete preserves image when placement does not
// match", "frees image after placement matches" and "without placement
// frees unplaced image"
#[test]
fn uppercase_id_delete_frees_only_what_it_matched() {
    let mut pane = small_pane();
    transmit(&mut pane, 1, 1, 1);
    pane.send("a=d,d=I,i=1,p=7", b"");
    assert_eq!(pane.image_count(), 1);

    place(&mut pane, 1, 9, (1, 1), "");
    pane.send("a=d,d=I,i=1,p=9", b"");
    assert_eq!(pane.graphics().placement_count(), 0);
    assert_eq!(pane.image_count(), 0);

    transmit(&mut pane, 1, 1, 1);
    pane.send("a=d,d=I,i=1", b"");
    assert_eq!(pane.image_count(), 0);
}

/// A 100x100 terminal of 1x1 pixel cells, Ghostty's point delete screen,
/// with image 1 of 50x50 and image 2 of 25x25 pixels.
fn point_pane() -> Pane {
    let mut pane = Pane::with(100, 100, 100, (1, 1));
    transmit(&mut pane, 1, 50, 50);
    transmit(&mut pane, 2, 25, 25);
    pane
}

// "storage: delete intersecting cursor"
#[test]
fn delete_at_the_cursor() {
    let mut pane = point_pane();
    place(&mut pane, 1, 1, (0, 0), "");
    place(&mut pane, 1, 2, (25, 25), "");
    pane.feed("\x1b[13;13H");
    pane.send("a=d,d=c", b"");
    assert_eq!(pane.graphics().placement_count(), 1);
    assert_eq!(pane.image_count(), 2);
    assert_eq!(pane.quads()[0].dest[..2], [25.0, 25.0]);
}

// "storage: delete intersecting cursor checks interior row column" and
// "delete intersecting cell checks interior row column"
#[test]
fn delete_at_a_cell_checks_both_row_and_column() {
    for delete in ["\x1b[6;22H\x1b_Ga=d,d=c\x1b\\", "\x1b_Ga=d,d=p,x=22,y=6\x1b\\"] {
        let mut pane = Pane::with(100, 100, 100, (1, 1));
        transmit(&mut pane, 1, 10, 10);
        place(&mut pane, 1, 1, (0, 0), "");
        place(&mut pane, 1, 2, (20, 0), "");
        pane.feed(delete);
        assert_eq!(pane.graphics().placement_count(), 1);
        assert_eq!(pane.quads()[0].dest[0], 0.0, "{delete:?}");
    }
}

// "storage: delete by column" and "delete by row", with the second placement
// moved clear of the first so each delete hits one of them.
#[test]
fn delete_by_column_and_row() {
    for delete in ["a=d,d=x,x=61", "a=d,d=y,y=61"] {
        let mut pane = point_pane();
        place(&mut pane, 1, 1, (0, 0), "");
        place(&mut pane, 1, 2, (60, 60), "");
        pane.send(delete, b"");
        assert_eq!(pane.graphics().placement_count(), 1, "{delete}");
        assert_eq!(pane.image_count(), 2);
        assert_eq!(pane.quads()[0].dest[..2], [0.0, 0.0]);
    }
}
