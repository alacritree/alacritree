//! Reads kitty's Unicode placeholder cells out of a grid row for the
//! graphics layer, which decodes them into the runs a frame draws tiles for.
//!
//! The capture calls this only for a row it re-reads that held a
//! placeholder, so rows without one cost nothing beyond the character check
//! the capture's own walk makes.

use alacritree_graphics::placeholder::{self, PLACEHOLDER, Placeholders};
use alacritty_terminal::grid::Row;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// Decode the placeholder cells of `cells` into viewport row `row`'s runs.
pub(super) fn scan_row(placeholders: &mut Placeholders, row: usize, cells: &Row<Cell>) {
    let found = cells.into_iter().enumerate().filter(|(_, cell)| cell.c == PLACEHOLDER);
    placeholders.scan_row(
        row,
        found.map(|(column, cell)| {
            (column, placeholder::Cell {
                foreground: color_id(cell.fg),
                underline: cell.underline_color().map_or(0, color_id),
                marks: cell.zerowidth().unwrap_or_default(),
            })
        }),
    );
}

/// The id a colour encodes, as kitty's `color_to_id` reads it: the 24 bits
/// of a true colour, or the index of a palette one. The default colours
/// encode 0.
fn color_id(color: Color) -> u32 {
    match color {
        Color::Spec(rgb) => u32::from_be_bytes([0, rgb.r, rgb.g, rgb.b]),
        Color::Indexed(index) => index.into(),
        // SGR 30-37 and 90-97 are palette entries 0-15 in kitty.
        Color::Named(named) if (named as usize) <= NamedColor::BrightWhite as usize => named as u32,
        Color::Named(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use alacritree_graphics::frame::ImageQuad;
    use alacritree_graphics::placeholder::{DIACRITICS, Run, diacritic_value};
    use alacritty_terminal::event::Event;
    use alacritty_terminal::grid::{Dimensions, Scroll};
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

    use super::*;
    use crate::config::Config;
    use crate::grid_gl::GpuGrid;
    use crate::repaint::Recorder;
    use crate::session::{EventProxy, TermSize};
    use crate::terminal_view::{GridSnapshot, capture_images};

    const P: &str = "\u{10EEEE}";
    /// The diacritics for 0 to 3, and for 42 as an image id's high byte.
    const D0: &str = "\u{0305}";
    const D1: &str = "\u{030D}";
    const D2: &str = "\u{030E}";
    const D3: &str = "\u{0310}";
    const D42: &str = "\u{059C}";

    /// A placeholder tile as kitty's tests read one: the source rectangle in
    /// fractions of the image, and the destination in viewport cells.
    #[derive(Debug)]
    struct Ref {
        src: [f32; 4],
        dest: [f32; 4],
    }

    /// A terminal whose frames go through the capture the grid paints from.
    struct Pane {
        term: Term<EventProxy<Recorder>>,
        parser: Processor<StdSyncHandler>,
        snapshot: GridSnapshot,
        gpu: GpuGrid,
        _events: mpsc::Receiver<Event>,
    }

    impl Pane {
        /// kitty's test screen: 10x20 pixel cells.
        fn new(columns: usize, lines: usize) -> Self {
            Self::with_cell(columns, lines, (10, 20))
        }

        fn with_cell(columns: usize, lines: usize, (width, height): (u32, u32)) -> Self {
            let (proxy, events) = EventProxy::new(Recorder::default());
            let config = TermConfig { scrolling_history: 100, ..TermConfig::default() };
            let mut term = Term::new(config, &TermSize::new(columns, lines), proxy);
            term.graphics_mut().set_cell_pixels(width, height);
            Self {
                term,
                parser: Processor::new(),
                snapshot: GridSnapshot::new(&Config::default().palette),
                gpu: GpuGrid::new(),
                _events: events,
            }
        }

        fn feed(&mut self, bytes: impl AsRef<[u8]>) {
            self.parser.advance(&mut self.term, bytes.as_ref());
        }

        /// kitty's tests' `put_image`: an RGB image of `x` bytes, transmitted
        /// with a virtual placement of `columns` by `rows` cells.
        fn put_image(&mut self, id: u32, (width, height): (u32, u32), cells: (u32, u32), p: u32) {
            let data = vec![b'x'; (width * height * 3) as usize];
            let (columns, rows) = cells;
            let control =
                format!("a=T,f=24,i={id},s={width},v={height},c={columns},r={rows},p={p},U=1,q=2");
            self.feed(format!("\x1b_G{control};{}\x1b\\", STANDARD.encode(data)));
        }

        /// kitty's tests' `put_ref`: another virtual placement of an image.
        fn put_ref(&mut self, id: u32, (columns, rows): (u32, u32), p: u32) {
            self.feed(format!("\x1b_Ga=p,i={id},c={columns},r={rows},p={p},U=1,q=2\x1b\\"));
        }

        /// One frame's capture, as `show` runs it under the terminal lock,
        /// once every decode in flight has finished.
        fn capture(&mut self) {
            let started = Instant::now();
            while self.term.graphics().is_decoding() {
                assert!(started.elapsed() < Duration::from_secs(20), "a decode never finished");
                std::thread::yield_now();
            }
            let switched = self.snapshot.context.session != Some(0);
            self.snapshot.capture(&mut self.term, &Config::default(), 0, None, None);
            capture_images(&mut self.term, &self.gpu, &self.snapshot, switched);
        }

        /// The placeholder runs the capture decoded, with their viewport rows.
        fn runs(&mut self) -> Vec<(usize, Run)> {
            self.capture();
            self.snapshot.placeholders.runs().map(|(row, run)| (row, *run)).collect()
        }

        /// The frame's tiles by the cell row they start on, then left to
        /// right, the order kitty's tests list its refs in.
        fn refs(&mut self) -> Vec<Ref> {
            self.capture();
            let state = self.gpu.state.lock().unwrap();
            let frame = &state.images;
            let mut refs: Vec<Ref> = frame
                .runs()
                .iter()
                .flat_map(|run| {
                    let size = [run.pixels.width() as f32, run.pixels.height() as f32];
                    let quads = &frame.quads()[run.quads.start as usize..run.quads.end as usize];
                    quads.iter().map(move |quad| Ref {
                        src: [0, 1, 2, 3].map(|side| quad.src[side] / size[side % 2]),
                        dest: quad.dest,
                    })
                })
                .collect();
            let key = |r: &Ref| (r.dest[1].floor(), r.dest[0]);
            refs.sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap());
            refs
        }

        fn quads(&self) -> Vec<ImageQuad> {
            self.gpu.state.lock().unwrap().images.quads().to_vec()
        }

        fn frame_generation(&self) -> u64 {
            self.gpu.state.lock().unwrap().images.generation()
        }

        /// The texels viewport cell `(column, row)` shows, from whichever
        /// quad covers its centre.
        fn tile_at(&self, column: usize, row: usize) -> Option<[f32; 4]> {
            let (x, y) = (column as f32, row as f32);
            self.quads().into_iter().find_map(|quad| {
                let [left, top, right, bottom] = quad.dest;
                let (cx, cy) = (x + 0.5, y + 0.5);
                if !(left <= cx && cx < right && top <= cy && cy < bottom) {
                    return None;
                }
                let scale_x = (quad.src[2] - quad.src[0]) / (right - left);
                let scale_y = (quad.src[3] - quad.src[1]) / (bottom - top);
                Some([
                    quad.src[0] + (x.max(left) - left) * scale_x,
                    quad.src[1] + (y.max(top) - top) * scale_y,
                    quad.src[0] + ((x + 1.0).min(right) - left) * scale_x,
                    quad.src[1] + ((y + 1.0).min(bottom) - top) * scale_y,
                ])
            })
        }

        /// Every placeholder cell in view, with the box row and column its
        /// marks name.
        fn placeholder_cells(&self) -> Vec<((usize, usize), (u32, u32))> {
            let grid = self.term.grid();
            let offset = grid.display_offset() as i32;
            let mut cells = Vec::new();
            for row in 0..grid.screen_lines() {
                let line = &grid[Line(row as i32 - offset)];
                for column in 0..grid.columns() {
                    let cell = &line[Column(column)];
                    if cell.c != PLACEHOLDER {
                        continue;
                    }
                    let marks = cell.zerowidth().expect("icat marks every cell");
                    let value = |at: usize| diacritic_value(marks[at]).unwrap();
                    cells.push(((column, row), (value(0), value(1))));
                }
            }
            cells
        }
    }

    fn assert_rect(actual: [f32; 4], expected: [f32; 4]) {
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-4, "{actual:?} != {expected:?}");
        }
    }

    fn assert_srcs(refs: &[Ref], expected: &[[f32; 4]]) {
        assert_eq!(refs.len(), expected.len(), "{refs:?}");
        for (r, e) in refs.iter().zip(expected) {
            assert_rect(r.src, *e);
        }
    }

    fn run(column: u32, columns: u32, box_row: u32, box_column: u32) -> Run {
        Run { column, columns, image_id: 0, placement_id: 0, box_row, box_column }
    }

    // kitty_tests/graphics.py test_unicode_placeholders
    #[test]
    fn placeholders_show_tiles_of_the_image_their_colour_names() {
        let mut pane = Pane::new(10, 5);
        pane.put_image(42, (20, 20), (4, 2), 0);
        pane.put_image((42 << 16) + (43 << 8) + 44, (10, 20), (4, 2), 0);
        assert!(pane.refs().is_empty(), "a virtual placement drew by itself");

        // One run of two cells, then two runs that are not contiguous.
        pane.feed("\x1b[38;5;42m");
        pane.feed(format!("{P}{D0}{D0}{P}{D0}{D1}"));
        pane.feed(format!("{P}{D0}{D0}{P}{D0}{D2}"));
        let expected = [[0.0, 0.0, 0.5, 0.5], [0.0, 0.0, 0.25, 0.5], [0.5, 0.0, 0.75, 0.5]];
        assert_srcs(&pane.refs(), &expected);

        pane.feed("\x1b[2K\r");
        assert!(pane.refs().is_empty(), "an erased line kept its tiles");

        // Ids in 24-bit colour. The second image is fitted to the box's
        // height and centred, so two cells span its whole width.
        pane.feed(format!("\x1b[38;2;0;0;42m{P}{D0}{D0}"));
        pane.feed(format!("\x1b[38;2;42;43;44m{P}{D0}{D1}{P}{D0}{D2}"));
        assert_srcs(&pane.refs(), &[[0.0, 0.0, 0.25, 0.5], [0.0, 0.0, 1.0, 0.5]]);

        // Implicit rows and columns mixed with explicit ones, in two runs.
        pane.feed("\x1b[2K\r\x1b[38;5;42m");
        pane.feed(format!("{P}{D0}{D0}{P}{D0}{P}{P}{D0}"));
        pane.feed(format!("{P}{D1}{P}{P}{P}{D1}{D3}"));
        assert_srcs(&pane.refs(), &[[0.0, 0.0, 1.0, 0.5], [0.0, 0.5, 1.0, 1.0]]);

        pane.feed("\x1bc");
        assert!(pane.refs().is_empty(), "a reset kept its tiles");
    }

    // kitty_tests/graphics.py test_unicode_placeholders_3rd_combining_char
    #[test]
    fn a_third_diacritic_names_the_image_id_high_byte() {
        let mut pane = Pane::new(10, 5);
        pane.put_image(42, (20, 20), (4, 2), 0);
        pane.put_image((42 << 24) + 43, (20, 10), (4, 1), 0);

        // Id 43, which no image has.
        pane.feed(format!("\x1b[38;2;0;0;43m{P}{D0}{P}{P}{P}"));
        assert!(pane.refs().is_empty());
        pane.feed("\x1b[2K\r");

        // An explicit zero high byte, then the second image through its
        // high byte, continued by implicit rows and columns.
        pane.feed(format!("\x1b[38;2;0;0;42m{P}{D0}{D0}{D0}{P}{D0}{D1}{D0}"));
        pane.feed(format!("\x1b[38;2;0;0;43m{P}{D0}{D0}{D42}{P}{D0}{D1}{D42}"));
        pane.feed(format!("{P}{D0}{P}"));
        assert_srcs(&pane.refs(), &[[0.0, 0.0, 0.5, 0.5], [0.0, 0.0, 1.0, 1.0]]);
        pane.feed("\x1b[2K\r");

        // The same through 256-colour indices: 16 bits of id.
        pane.feed(format!("\x1b[38;5;42m{P}{D0}{D0}{D0}{P}"));
        pane.feed(format!("\x1b[38;5;43m{P}{D0}{D0}{D42}{P}{P}{D0}{P}"));
        assert_srcs(&pane.refs(), &[[0.0, 0.0, 0.5, 0.5], [0.0, 0.0, 1.0, 1.0]]);
    }

    // kitty_tests/graphics.py test_unicode_placeholders_multiple_placements
    #[test]
    fn the_underline_colour_names_the_virtual_placement() {
        let mut pane = Pane::new(10, 5);
        pane.put_image(42, (20, 20), (1, 1), 1);
        pane.put_ref(42, (2, 1), 22);
        pane.put_ref(42, (4, 2), 44);
        assert!(pane.refs().is_empty());

        pane.feed("\x1b[38;5;42m\x1b[58;5;1m");
        pane.feed(format!("{P}{D0}"));
        pane.feed(format!("\x1b[58;5;22m{P}{D0}{P}{D0}"));
        pane.feed(format!("\x1b[58;5;44m{P}{D0}{P}{D0}{P}{D0}{P}{D0}"));

        let refs = pane.refs();
        // kitty's first ref runs past the image to 1.5, into the texture's
        // transparent border. Here the tile stops at the image's edge and
        // its destination is centred in the cell instead.
        let expected = [[0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0], [0.0, 0.0, 1.0, 0.5]];
        assert_srcs(&refs, &expected);
        assert_rect(refs[0].dest, [0.0, 0.25, 1.0, 0.75]);
    }

    // kitty_tests/graphics.py test_unicode_placeholders_scroll
    #[test]
    fn tiles_follow_their_rows_through_a_scroll_inside_margins() {
        let mut pane = Pane::with_cell(10, 8, (5, 10));
        pane.put_image(42, (5, 80), (1, 8), 0);
        pane.feed("\x1b[38;5;42m");
        for (line, mark) in DIACRITICS[..8].iter().enumerate() {
            pane.feed(format!("\x1b[{};1H{P}{mark}", line + 1));
        }
        let tiles = |pane: &mut Pane| -> Vec<(f32, f32)> {
            pane.refs().iter().map(|r| ((r.src[1] * 8.0).round(), r.dest[1])).collect()
        };
        let every_row: Vec<_> = (0..8).map(|row| (row as f32, row as f32)).collect();
        assert_eq!(tiles(&mut pane), every_row);

        // Lines 3 to 6 scroll up by two, so lines 3 and 4 go.
        pane.feed("\x1b[3;6r\x1b[6;1H\x1bD\x1bD");
        assert_eq!(tiles(&mut pane), [
            (0.0, 0.0),
            (1.0, 1.0),
            (4.0, 2.0),
            (5.0, 3.0),
            (6.0, 6.0),
            (7.0, 7.0)
        ]);

        // Then down by three, so line 6 goes.
        pane.feed("\x1b[3;1H\x1bM\x1bM\x1bM");
        assert_eq!(tiles(&mut pane), [(0.0, 0.0), (1.0, 1.0), (4.0, 5.0), (6.0, 6.0), (7.0, 7.0)]);
    }

    // Ghostty graphics_unicode.zig "unicode placement: none"
    #[test]
    fn text_without_placeholders_has_no_runs() {
        let mut pane = Pane::new(5, 5);
        pane.feed("hello\r\nworld\r\n1\r\n2");
        assert!(pane.runs().is_empty());
    }

    // Ghostty graphics_unicode.zig "unicode placement: single row/col"
    #[test]
    fn one_placeholder_is_one_run() {
        let mut pane = Pane::new(5, 5);
        pane.feed(format!("{P}{D0}{D0}"));
        assert_eq!(pane.runs(), [(0, run(0, 1, 0, 0))]);
    }

    // Ghostty graphics_unicode.zig "unicode placement: continuation break"
    #[test]
    fn a_skipped_box_column_breaks_the_run() {
        let mut pane = Pane::new(10, 5);
        pane.feed(format!("{P}{D0}{D0}{P}{D0}{D2}"));
        assert_eq!(pane.runs(), [(0, run(0, 1, 0, 0)), (0, run(1, 1, 0, 2))]);
    }

    // Ghostty graphics_unicode.zig "unicode placement: continuation with
    // diacritics set", "... with no col" and "... with no diacritics"
    #[test]
    fn cells_continue_a_run_with_explicit_or_missing_marks() {
        for cells in [
            format!("{P}{D0}{D0}{P}{D0}{D1}{P}{D0}{D2}"),
            format!("{P}{D0}{D0}{P}{D0}{P}{D0}"),
            format!("{P}{P}{P}"),
        ] {
            let mut pane = Pane::new(10, 5);
            pane.feed(&cells);
            assert_eq!(pane.runs(), [(0, run(0, 3, 0, 0))], "{cells:?}");
        }
    }

    // Ghostty graphics_unicode.zig "unicode placement: run ending" and
    // "... run starting in the middle"
    #[test]
    fn other_text_bounds_a_run() {
        let mut pane = Pane::new(10, 5);
        pane.feed(format!("{P}{D0}{D0}{P}{D0}{D1}ABC"));
        assert_eq!(pane.runs(), [(0, run(0, 2, 0, 0))]);

        let mut pane = Pane::new(10, 5);
        pane.feed(format!("ABC{P}{D0}{D0}{P}{D0}{D1}"));
        assert_eq!(pane.runs(), [(0, run(3, 2, 0, 0))]);
    }

    // Ghostty graphics_unicode.zig "unicode placement: specifying image id as
    // palette", "... with high bits" and "... placement id as palette"
    #[test]
    fn palette_colours_and_the_third_mark_encode_the_ids() {
        let mut pane = Pane::new(5, 5);
        pane.feed(format!("\x1b[38;5;42m{P}{D0}{D0}"));
        assert_eq!(pane.runs()[0].1.image_id, 42);

        let mut pane = Pane::new(5, 5);
        pane.feed(format!("\x1b[38;5;42m{P}{D0}{D0}{D2}"));
        assert_eq!(pane.runs()[0].1.image_id, 33_554_474);

        let mut pane = Pane::new(5, 5);
        pane.feed(format!("\x1b[38;5;42m\x1b[58;5;21m{P}{D0}{D0}"));
        let (_, found) = pane.runs()[0];
        assert_eq!((found.image_id, found.placement_id), (42, 21));
    }

    /// icat's ids keep every byte but the lowest non-zero, so the high byte
    /// goes through a third diacritic.
    const ICAT_ID: u32 = 0x2A12_3456;

    /// The width of the icat pane, and the spaces icat's default
    /// `--align center` prints before each row of its 4-column box.
    const ICAT_COLUMNS: usize = 20;
    const ICAT_INDENT: usize = (ICAT_COLUMNS - 4) / 2;

    /// What `kitten icat --unicode-placeholder` writes for a 20x20 RGB image
    /// into 5x10 pixel cells: `transmit_stream` sends it uncompressed in one
    /// chunk, being under 2048 bytes, and `write_unicode_placeholder` prints
    /// a 4x2 box centred in the pane.
    fn icat(id: u32) -> String {
        let rgb: Vec<u8> =
            (0..20 * 20).flat_map(|i: u32| [(i % 20) as u8, (i / 20) as u8, 0]).collect();
        let mut out = format!(
            "\r\x1b_Ga=T,q=2,f=24,U=1,s=20,v=20,c=4,r=2,i={id};{}\x1b\\",
            STANDARD_NO_PAD.encode(rgb)
        );
        out += &format!("\x1b[38:2:{}:{}:{}m", (id >> 16) & 255, (id >> 8) & 255, id & 255);
        let mark = |number: u32| DIACRITICS[number as usize];
        for row in 0..2 {
            out += &" ".repeat(ICAT_INDENT);
            for column in 0..4 {
                out.extend([PLACEHOLDER, mark(row), mark(column), mark(id >> 24)]);
            }
            if row == 0 {
                out += "\n\r";
            }
        }
        out + "\x1b[39m\n"
    }

    /// The texels of box cell `(row, column)` of the icat image.
    fn icat_tile(row: u32, column: u32) -> [f32; 4] {
        let (x, y) = (column as f32 * 5.0, row as f32 * 10.0);
        [x, y, x + 5.0, y + 10.0]
    }

    fn icat_pane() -> Pane {
        Pane::with_cell(ICAT_COLUMNS, 10, (5, 10))
    }

    /// Every placeholder in view shows the tile its marks name.
    fn assert_every_tile_on_its_cell(pane: &Pane, expected_cells: usize) {
        let cells = pane.placeholder_cells();
        assert_eq!(cells.len(), expected_cells, "placeholders in view: {cells:?}");
        for ((column, row), (box_row, box_column)) in cells {
            let tile = pane.tile_at(column, row).unwrap_or_else(|| {
                panic!("cell {column},{row} for box {box_row},{box_column} shows nothing")
            });
            assert_rect(tile, icat_tile(box_row, box_column));
        }
    }

    #[test]
    fn kitten_icat_output_gives_each_viewport_cell_its_tile() {
        let mut pane = icat_pane();
        pane.feed("$ kitten icat\r\n");
        pane.feed(icat(ICAT_ID));
        pane.capture();

        for row in 0..2 {
            for column in 0..4 {
                let (x, y) = (ICAT_INDENT + column as usize, row as usize + 1);
                assert_eq!(pane.tile_at(x, y), Some(icat_tile(row, column)), "cell {x},{y}");
            }
        }
        let (left, right) = (ICAT_INDENT - 1, ICAT_INDENT + 4);
        for (column, row) in [(left, 1), (right, 1), (right, 2), (ICAT_INDENT, 0), (ICAT_INDENT, 3)]
        {
            assert_eq!(pane.tile_at(column, row), None, "cell {column},{row} showed a tile");
        }
    }

    #[test]
    fn tiles_stay_on_their_cells_through_scrollback_and_resizes() {
        let mut pane = icat_pane();
        pane.feed(icat(ICAT_ID));
        let placements = pane.term.graphics().placement_count();
        pane.capture();
        assert_every_tile_on_its_cell(&pane, 8);

        for line in 0..20 {
            pane.feed(format!("line {line}\r\n"));
        }
        pane.capture();
        assert_every_tile_on_its_cell(&pane, 0);
        assert!(pane.quads().is_empty(), "an image in scrollback still drew");

        pane.term.scroll_display(Scroll::Top);
        pane.capture();
        assert_every_tile_on_its_cell(&pane, 8);

        // Ten columns split each placeholder row's run across two lines, and
        // six lines push more of the screen into history.
        pane.term.scroll_display(Scroll::Bottom);
        pane.term.resize(TermSize::new(10, 6));
        pane.term.scroll_display(Scroll::Top);
        pane.capture();
        assert_every_tile_on_its_cell(&pane, 8);

        pane.term.resize(TermSize::new(20, 10));
        pane.term.scroll_display(Scroll::Top);
        pane.capture();
        assert_every_tile_on_its_cell(&pane, 8);

        assert_eq!(pane.term.graphics().placement_count(), placements);
    }

    #[test]
    fn a_frame_damaging_no_placeholder_row_rebuilds_no_tiles() {
        let mut pane = icat_pane();
        pane.feed(icat(ICAT_ID));
        pane.capture();
        assert!(!pane.quads().is_empty());
        let (frame, runs) = (pane.frame_generation(), pane.snapshot.placeholders.generation());

        pane.feed("\x1b[8;1Hsome text\x1b[9;1Hmore");
        pane.capture();

        let damaged = pane.snapshot.damaged_rows();
        assert!(!damaged.is_empty() && damaged.iter().all(|&row| row > 1), "{damaged:?}");
        assert_eq!(pane.snapshot.placeholders.generation(), runs);
        assert_eq!(pane.frame_generation(), frame, "the tiles were rebuilt");
    }

    #[test]
    fn placeholders_printed_before_their_image_draw_once_it_decodes() {
        let mut pane = icat_pane();
        let bytes = icat(ICAT_ID);
        let (transmit, text) = bytes.split_at(bytes.find("\x1b[38:").unwrap());
        pane.feed(text);
        pane.capture();
        assert!(pane.quads().is_empty());

        pane.feed(transmit);
        pane.capture();

        assert!(pane.snapshot.damaged_rows().iter().all(|&row| row > 1));
        assert_every_tile_on_its_cell(&pane, 8);
    }

    #[test]
    fn a_placeholder_in_the_default_colour_shows_no_unnamed_image() {
        let mut pane = Pane::new(10, 5);
        let pixel = STANDARD.encode([0, 0, 0]);
        pane.feed(format!("\x1b_Ga=T,f=24,s=1,v=1,c=1,r=1,U=1,q=2;{pixel}\x1b\\"));
        pane.feed(format!("{P}{D0}{D0}"));
        assert!(pane.refs().is_empty());
    }

    #[test]
    fn a_placeholder_draws_as_a_blank_cell() {
        let mut pane = Pane::new(10, 2);
        pane.feed(format!("a{P}{D0}{D0}b"));
        pane.capture();
        let text: String = pane.snapshot.runs().map(|(text, _)| text).collect();
        assert!(text.starts_with("a b"), "{text:?}");
    }
}
