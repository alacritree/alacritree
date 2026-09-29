//! One placement of an image and its geometry, ported from kitty's
//! `ImageRef` and the functions in `kitty/graphics.c` that size, draw and
//! clip it.
//!
//! Sizes that depend on the cell size are recomputed whenever it changes, so
//! a placement sized in cells keeps its cells and one at native size keeps
//! its pixels.

/// The size of one cell in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellSize {
    pub width: u32,
    pub height: u32,
}

impl CellSize {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self { width: width.max(1), height: height.max(1) }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Placement {
    /// Creation order within its image, which breaks draw-order ties.
    pub internal_id: u32,
    /// The client's `p`, or 0.
    pub client_id: u32,
    /// Absolute line of the top row: the store's scroll count plus the
    /// screen line it was placed on.
    pub row: i64,
    pub column: i32,
    /// Pixel offsets inside the anchor cell.
    pub cell_x: u32,
    pub cell_y: u32,
    /// Source rectangle in texels.
    pub src_x: f32,
    pub src_y: f32,
    pub src_width: f32,
    pub src_height: f32,
    /// Requested size in cells, 0 for "from the image".
    pub columns: u32,
    pub rows: u32,
    /// The cells the placement covers.
    pub effective_columns: u32,
    pub effective_rows: u32,
    pub z: i32,
    /// `U=1`: a placement Unicode placeholders refer to, never drawn here.
    pub is_virtual: bool,
}

impl Placement {
    /// kitty's `update_dest_rect`: the cells covered, from the requested
    /// columns and rows, or from the source size where one is missing.
    pub(crate) fn fit(&mut self, cell: CellSize) {
        let (cw, ch) = (f64::from(cell.width), f64::from(cell.height));
        let (src_width, src_height) = (f64::from(self.src_width), f64::from(self.src_height));
        let mut columns = self.columns;
        let mut rows = self.rows;
        if self.columns == 0 {
            columns = if self.rows == 0 {
                cells_for(src_width + f64::from(self.cell_x), cw)
            } else if src_height > 0.0 {
                let height = ch * f64::from(self.rows) + f64::from(self.cell_y);
                (height * src_width / src_height / cw).ceil() as u32
            } else {
                0
            };
        }
        if self.rows == 0 {
            rows = if self.columns == 0 {
                cells_for(src_height + f64::from(self.cell_y), ch)
            } else if src_width > 0.0 {
                let width = cw * f64::from(self.columns) + f64::from(self.cell_x);
                (width * src_height / src_width / ch).ceil() as u32
            } else {
                0
            };
        }
        self.effective_columns = columns;
        self.effective_rows = rows;
    }

    /// kitty's `grman_rescale` for one placement.
    pub(crate) fn rescale(&mut self, cell: CellSize) {
        self.cell_x = self.cell_x.min(cell.width - 1);
        self.cell_y = self.cell_y.min(cell.height - 1);
        self.fit(cell);
    }

    /// `[left, top, right, bottom]` in cells, with the anchor row at `top`.
    ///
    /// This is the rectangle kitty's `grman_update_layers` draws: both `c`
    /// and `r` stretch the image, one of them keeps its aspect ratio, and
    /// with neither it keeps its pixel size.
    pub(crate) fn dest(&self, top: f64, cell: CellSize) -> Option<[f64; 4]> {
        let (cw, ch) = (f64::from(cell.width), f64::from(cell.height));
        let (src_width, src_height) = (f64::from(self.src_width), f64::from(self.src_height));
        if src_width <= 0.0 || src_height <= 0.0 {
            return None;
        }
        let column = f64::from(self.column);
        let top_edge = top + f64::from(self.cell_y) / ch;
        let left = column + f64::from(self.cell_x) / cw;
        let (right, bottom);
        if self.rows != 0 {
            bottom = top + f64::from(self.rows);
            right = if self.columns != 0 {
                column + f64::from(self.columns)
            } else {
                left + (bottom - top_edge) * ch * src_width / src_height / cw
            };
        } else {
            right = if self.columns != 0 {
                column + f64::from(self.columns)
            } else {
                left + src_width / cw
            };
            bottom = top_edge + (right - left) * cw * src_height / src_width / ch;
        }
        Some([left, top_edge, right, bottom])
    }

    /// Move a placement that lies wholly inside the scroll region `top..=bottom`
    /// by `amount` rows, clipping what leaves the region off its source.
    /// `row` is its screen row.  Returns whether it should be removed.
    ///
    /// kitty's `scroll_filter_margins_func`: a placement straddling a margin
    /// before the scroll stays where it is.
    pub(crate) fn scroll_within(
        &mut self,
        row: &mut i64,
        amount: i64,
        top: i64,
        bottom: i64,
        cell: CellSize,
    ) -> bool {
        let rows = i64::from(self.effective_rows);
        if !(*row >= top && *row + rows - 1 <= bottom) {
            return false;
        }
        *row += amount;
        if self.outside(*row, top, bottom) {
            return true;
        }

        let ch = cell.height as f32;
        let scale = self.src_per_dest_pixel(cell);
        if *row < top {
            let clipped = (top - *row) as u32;
            let clip = scale * (ch * clipped as f32 - self.cell_y as f32);
            if self.src_height <= clip {
                return true;
            }
            self.src_y += clip;
            self.src_height -= clip;
            self.effective_rows -= clipped;
            if self.rows != 0 {
                self.rows -= clipped;
            }
            self.cell_y = 0;
            *row += i64::from(clipped);
        } else if *row + i64::from(self.effective_rows) - 1 > bottom {
            let clipped = (*row + i64::from(self.effective_rows) - 1 - bottom) as u32;
            let visible = ch * (self.effective_rows - clipped) as f32 - self.cell_y as f32;
            let height = scale * visible;
            if height <= 0.0 {
                return true;
            }
            let clip = self.src_height - height;
            if clip > 0.0 {
                self.src_height -= clip;
            }
            self.effective_rows -= clipped;
            if self.rows != 0 {
                self.rows -= clipped;
            }
        }
        self.outside(*row, top, bottom)
    }

    fn outside(&self, row: i64, top: i64, bottom: i64) -> bool {
        row + i64::from(self.effective_rows) <= top || row > bottom
    }

    /// Source pixels per screen pixel down the placement, matching
    /// [`Placement::dest`], so a scaled image is clipped rather than
    /// squashed.
    fn src_per_dest_pixel(&self, cell: CellSize) -> f32 {
        let (cw, ch) = (cell.width as f32, cell.height as f32);
        let height = if self.rows != 0 {
            self.rows as f32 * ch - self.cell_y as f32
        } else if self.columns != 0 && self.src_width > 0.0 {
            let width = self.columns as f32 * cw - self.cell_x as f32;
            width * self.src_height / self.src_width
        } else {
            return 1.0;
        };
        if height > 0.0 { self.src_height / height } else { 1.0 }
    }

    /// The last screen row this placement covers, exclusive.
    pub(crate) fn bottom(&self, row: i64) -> i64 {
        row + i64::from(self.effective_rows)
    }

    pub(crate) fn covers_column(&self, column: i64) -> bool {
        let start = i64::from(self.column);
        start <= column && column < start + i64::from(self.effective_columns)
    }

    pub(crate) fn covers_row(&self, row: i64, screen_row: i64) -> bool {
        screen_row <= row && row < self.bottom(screen_row)
    }
}

/// Whole cells needed for `pixels`, rounding up.
fn cells_for(pixels: f64, cell: f64) -> u32 {
    let pixels = pixels as u32;
    let cell = cell as u32;
    let cells = pixels / cell;
    if pixels > cells * cell { cells + 1 } else { cells }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellSize = CellSize { width: 10, height: 20 };

    fn placement(src_width: f32, src_height: f32) -> Placement {
        Placement {
            internal_id: 1,
            client_id: 0,
            row: 0,
            column: 0,
            cell_x: 0,
            cell_y: 0,
            src_x: 0.0,
            src_y: 0.0,
            src_width,
            src_height,
            columns: 0,
            rows: 0,
            effective_columns: 0,
            effective_rows: 0,
            z: 0,
            is_virtual: false,
        }
    }

    // kitty_tests/graphics.py test_graphics_put_with_pixel_offsets
    #[test]
    fn pixel_offsets_add_to_the_cells_covered() {
        let mut p = placement(10.0, 20.0);
        (p.cell_x, p.cell_y) = (5, 5);
        p.fit(CELL);
        assert_eq!((p.effective_columns, p.effective_rows), (2, 2));
    }

    // Ghostty "storage: aspect ratio calculation when only columns or rows specified"
    #[test]
    fn one_requested_side_derives_the_other_from_the_aspect_ratio() {
        let mut p = placement(20.0, 80.0);
        p.columns = 1;
        p.fit(CELL);
        assert_eq!((p.effective_columns, p.effective_rows), (1, 2));

        let mut p = placement(20.0, 80.0);
        p.rows = 2;
        p.fit(CELL);
        assert_eq!((p.effective_columns, p.effective_rows), (1, 2));
    }

    #[test]
    fn both_sides_requested_stretch_the_image() {
        let mut p = placement(1.0, 1.0);
        (p.columns, p.rows) = (3, 2);
        p.fit(CELL);
        assert_eq!(p.dest(0.0, CELL), Some([0.0, 0.0, 3.0, 2.0]));
    }

    #[test]
    fn a_native_size_image_covers_its_pixel_count() {
        let mut p = placement(15.0, 30.0);
        p.cell_x = 3;
        p.fit(CELL);
        assert_eq!(p.dest(1.0, CELL), Some([0.3, 1.0, 1.8, 2.5]));
    }

    #[test]
    fn an_empty_source_draws_nothing() {
        assert_eq!(placement(0.0, 20.0).dest(0.0, CELL), None);
    }
}
