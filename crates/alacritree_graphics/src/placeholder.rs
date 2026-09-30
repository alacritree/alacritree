//! Unicode placeholders: text cells holding U+10EEEE that each show one
//! tile of an image through a virtual placement, the part of kitty's
//! protocol that lets an image pass through anything that carries text.
//!
//! A cell's colours and combining marks say which image and which tile it
//! shows, so the image moves with its text through scrolling, reflow and a
//! multiplexer's redraws. The renderer decodes a row into [`Run`]s when it
//! re-reads that row, and runs name their image by id, so an image or a
//! placement that arrives after its text shows up at the next frame without
//! the row being read again.

use std::mem;

/// The character a placeholder cell holds.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// One placeholder cell as the grid holds it.
#[derive(Clone, Copy, Debug)]
pub struct Cell<'a> {
    /// The id the foreground colour encodes: the colour's 24 bits, or its
    /// palette index.
    pub foreground: u32,
    /// The id the underline colour encodes the same way, 0 without one.
    pub underline: u32,
    /// The combining marks after the placeholder: row, column, then the
    /// image id's high byte.
    pub marks: &'a [char],
}

/// Consecutive placeholder cells of one row showing consecutive tiles of one
/// row of an image's cell box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// The viewport column of its first cell.
    pub column: u32,
    pub columns: u32,
    pub image_id: u32,
    /// The virtual placement it shows, 0 for the image's first one.
    pub placement_id: u32,
    /// The row of the placement's cell box it shows.
    pub box_row: u32,
    /// The column of the placement's cell box its first cell shows.
    pub box_column: u32,
}

/// The placeholder runs of every viewport row.
///
/// Only the rows a capture re-reads are decoded again, and the generation
/// moves only when a row's runs change, so a frame that rereads no
/// placeholder row rebuilds no tiles.
#[derive(Debug, Default)]
pub struct Placeholders {
    rows: Vec<Vec<Run>>,
    scratch: Vec<Run>,
    generation: u64,
}

impl Placeholders {
    /// Forget every row and hold `rows` empty ones, for a viewport whose
    /// rows now hold other text.
    pub fn reset(&mut self, rows: usize) {
        if self.rows.len() == rows && self.rows.iter().all(Vec::is_empty) {
            return;
        }
        self.rows.iter_mut().for_each(Vec::clear);
        self.rows.resize_with(rows, Vec::new);
        self.generation += 1;
    }

    /// Replace the runs of viewport row `row` with those decoded from its
    /// placeholder cells, given left to right with their columns.
    ///
    /// kitty's `screen_render_line_graphics`: a cell continues the run to
    /// its left when it has the same image and placement and each mark it
    /// has agrees with the run, and missing marks are taken from the run.
    /// Unlike kitty, any other cell between two placeholders ends the run.
    pub fn scan_row<'a>(&mut self, row: usize, cells: impl IntoIterator<Item = (usize, Cell<'a>)>) {
        let mut scratch = mem::take(&mut self.scratch);
        scratch.clear();
        let mut pending: Option<Pending> = None;
        for (column, cell) in cells {
            let cell = Decoded::of(&cell);
            match pending.as_mut() {
                Some(run) if run.continues(column, &cell) => run.columns += 1,
                _ => {
                    scratch.extend(pending.take().map(Pending::finish));
                    pending = Some(Pending::start(column, &cell));
                },
            }
        }
        scratch.extend(pending.map(Pending::finish));

        let runs = &mut self.rows[row];
        if *runs != scratch {
            mem::swap(runs, &mut scratch);
            self.generation += 1;
        }
        self.scratch = scratch;
    }

    /// Row `row` holds no placeholders.
    pub fn clear_row(&mut self, row: usize) {
        let runs = &mut self.rows[row];
        if !runs.is_empty() {
            runs.clear();
            self.generation += 1;
        }
    }

    /// Moves whenever a row's runs change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every run with its viewport row.
    pub fn runs(&self) -> impl Iterator<Item = (usize, &Run)> {
        self.rows.iter().enumerate().flat_map(|(row, runs)| runs.iter().map(move |run| (row, run)))
    }
}

/// What one cell says, each mark `None` when missing or not in the table.
struct Decoded {
    image_low: u32,
    placement_id: u32,
    row: Option<u32>,
    column: Option<u32>,
    image_high: Option<u32>,
}

impl Decoded {
    fn of(cell: &Cell<'_>) -> Self {
        let mark = |at: usize| cell.marks.get(at).copied().and_then(diacritic_value);
        Self {
            image_low: cell.foreground & 0xFF_FFFF,
            placement_id: cell.underline & 0xFF_FFFF,
            row: mark(0),
            column: mark(1),
            image_high: mark(2).filter(|&high| high <= 0xFF),
        }
    }
}

struct Pending {
    column: usize,
    columns: u32,
    image_low: u32,
    image_high: u32,
    placement_id: u32,
    box_row: u32,
    box_column: u32,
}

impl Pending {
    fn start(column: usize, cell: &Decoded) -> Self {
        Self {
            column,
            columns: 1,
            image_low: cell.image_low,
            image_high: cell.image_high.unwrap_or(0),
            placement_id: cell.placement_id,
            box_row: cell.row.unwrap_or(0),
            box_column: cell.column.unwrap_or(0),
        }
    }

    fn continues(&self, column: usize, cell: &Decoded) -> bool {
        column == self.column + self.columns as usize
            && cell.image_low == self.image_low
            && cell.placement_id == self.placement_id
            && cell.row.is_none_or(|row| row == self.box_row)
            && cell.column.is_none_or(|column| column == self.box_column + self.columns)
            && cell.image_high.is_none_or(|high| high == self.image_high)
    }

    fn finish(self) -> Run {
        Run {
            column: self.column as u32,
            columns: self.columns,
            image_id: self.image_low | self.image_high << 24,
            placement_id: self.placement_id,
            box_row: self.box_row,
            box_column: self.box_column,
        }
    }
}

/// The number a row or column diacritic stands for: its index in kitty's
/// `gen/rowcolumn-diacritics.txt`.
pub fn diacritic_value(mark: char) -> Option<u32> {
    DIACRITICS.binary_search(&mark).ok().map(|index| index as u32)
}

/// kitty's `gen/rowcolumn-diacritics.txt`, in order: the diacritic for
/// each number.
pub const DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;

    // Ghostty graphics_unicode.zig "unicode diacritic sorted"
    #[test]
    fn the_diacritic_table_is_sorted_for_its_binary_search() {
        assert!(DIACRITICS.is_sorted());
    }

    // Ghostty graphics_unicode.zig "unicode diacritic", spot checks from kitty
    #[test]
    fn a_diacritic_stands_for_its_index_in_kitty_table() {
        assert_eq!(diacritic_value('\u{0305}'), Some(0));
        assert_eq!(diacritic_value('\u{030D}'), Some(1));
        assert_eq!(diacritic_value('\u{0483}'), Some(30));
        assert_eq!(diacritic_value('\u{1D242}'), Some(294));
        assert_eq!(diacritic_value('\u{1D244}'), Some(296));
        assert_eq!(
            diacritic_value('\u{0300}'),
            None,
            "fuses with a base letter, so kitty skips it"
        );
    }
}
