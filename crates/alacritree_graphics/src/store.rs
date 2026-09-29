//! The images and placements of one screen buffer: kitty's
//! `GraphicsManager` without the parts that decode or draw.
//!
//! A placement's row is absolute, the store's scroll count plus the screen
//! line it was placed on, so a scroll of the whole screen moves every
//! placement by changing one number.  Only a scroll inside margins, which
//! clips placements one by one, walks them.

use std::sync::Arc;

use alacritree_common::jobs::Job;

use crate::command::Command;
use crate::decode::Slot;
use crate::frame::{ImageQuad, Pixels};
use crate::placement::{CellSize, Placement};
use crate::{CursorMove, ScrollRegion, Viewport};

/// kitty's storage limit per screen buffer, counted in decoded RGBA bytes.
pub(crate) const DEFAULT_QUOTA: usize = 320 * 1024 * 1024;

pub(crate) struct Image {
    /// Creation order, which breaks draw-order ties.
    internal_id: u64,
    client_id: u32,
    number: u32,
    width: u32,
    height: u32,
    /// Where the decoded pixels land.  `None` until the transmission
    /// completes, and forever when it fails.
    data: Option<Arc<Slot>>,
    /// Dropping the job cancels a decode that has not started.
    _decode: Option<Job<()>>,
    /// Decoded bytes counted against the quota.
    footprint: usize,
    transient: bool,
    /// When it was last transmitted or placed, for eviction.
    atime: u64,
    placements: Vec<Placement>,
    next_placement: u32,
}

impl Image {
    /// Whether its data arrived and did not fail to decode: kitty's
    /// `root_frame_data_loaded`.
    fn has_data(&self) -> bool {
        self.data.as_ref().is_some_and(|slot| !slot.failed())
    }
}

/// A placement the frame shows, before it is sorted into draw order.
pub(crate) struct Visible {
    pub key: (i32, u64, u32),
    pub image: usize,
    pub quad: ImageQuad,
}

pub(crate) struct Store {
    images: Vec<Image>,
    next_image: u64,
    clock: u64,
    /// Lines scrolled off the top of the whole screen, minus lines scrolled
    /// back down.
    scrolled: i64,
    used: usize,
    pub quota: usize,
    /// Scrollback capacity in lines; a placement above it is dropped.
    history: i64,
    /// The scroll count at which some placement may have left the history.
    expiry: i64,
    /// Moves whenever something a frame shows may have changed.
    pub generation: u64,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            images: Vec::new(),
            next_image: 0,
            clock: 0,
            scrolled: 0,
            used: 0,
            quota: DEFAULT_QUOTA,
            history: 0,
            expiry: i64::MAX,
            generation: 0,
        }
    }
}

impl Store {
    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub(crate) fn image_count(&self) -> usize {
        self.images.len()
    }

    pub(crate) fn placement_count(&self) -> usize {
        self.images.iter().map(|image| image.placements.len()).sum()
    }

    pub(crate) fn set_history(&mut self, lines: usize) {
        let lines = i64::try_from(lines).unwrap_or(i64::MAX);
        if lines != self.history {
            self.history = lines;
            self.recompute_expiry();
        }
    }

    pub(crate) fn index_by_id(&self, id: u32) -> Option<usize> {
        self.images.iter().position(|image| image.client_id == id)
    }

    /// The newest image with `number`.
    pub(crate) fn index_by_number(&self, number: u32) -> Option<usize> {
        self.images.iter().rposition(|image| image.number == number)
    }

    pub(crate) fn index_by_internal(&self, internal_id: u64) -> Option<usize> {
        self.images.iter().position(|image| image.internal_id == internal_id)
    }

    pub(crate) fn decoded(&self, index: usize) -> Option<&Arc<Pixels>> {
        self.images[index].data.as_ref().and_then(|slot| slot.pixels())
    }

    pub(crate) fn pixels(&self, index: usize) -> &Arc<Pixels> {
        self.decoded(index).expect("a visible image")
    }

    pub(crate) fn is_decoding(&self) -> bool {
        self.images.iter().any(|image| {
            image.data.as_ref().is_some_and(|slot| slot.pixels().is_none() && !slot.failed())
        })
    }

    pub(crate) fn has_data(&self, index: usize) -> bool {
        self.images[index].has_data()
    }

    pub(crate) fn client_id(&self, index: usize) -> u32 {
        self.images[index].client_id
    }

    /// Before a new transmission, drop images whose data never arrived and
    /// unnamed ones nothing shows: kitty's `add_trim_predicate`.
    pub(crate) fn trim_for_transmission(&mut self) {
        self.remove_where(|image| {
            !image.has_data() || (image.client_id == 0 && image.placements.is_empty())
        });
    }

    /// The image a transmission fills: the one with `id`, emptied, or a new
    /// one.  An image with only a number gets the smallest free id.
    /// Returns its internal id and its client id.
    pub(crate) fn begin_image(&mut self, id: u32, number: u32, transient: bool) -> (u64, u32) {
        let atime = self.tick();
        let existing = (id != 0).then(|| self.index_by_id(id)).flatten();
        let index = match existing {
            Some(index) => {
                let image = &mut self.images[index];
                self.used -= image.footprint;
                image.footprint = 0;
                image.data = None;
                image._decode = None;
                image.placements.clear();
                self.generation += 1;
                index
            },
            None => {
                let client_id = if id == 0 && number != 0 { self.free_client_id() } else { id };
                self.next_image += 1;
                self.images.push(Image {
                    internal_id: self.next_image,
                    client_id,
                    number,
                    width: 0,
                    height: 0,
                    data: None,
                    _decode: None,
                    footprint: 0,
                    transient: false,
                    atime,
                    placements: Vec::new(),
                    next_placement: 0,
                });
                self.images.len() - 1
            },
        };
        let image = &mut self.images[index];
        image.atime = atime;
        image.transient = transient;
        (image.internal_id, image.client_id)
    }

    /// kitty's `get_free_client_id`.
    fn free_client_id(&self) -> u32 {
        let mut ids: Vec<u32> =
            self.images.iter().map(|image| image.client_id).filter(|&id| id != 0).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut free = 1;
        for id in ids {
            if id != free {
                break;
            }
            free = id + 1;
        }
        free
    }

    /// Whether an image this size fits the quota at all.
    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        footprint(width, height) <= self.quota
    }

    /// Record that the data of the image at `index` arrived.
    pub(crate) fn commit(
        &mut self,
        index: usize,
        (width, height): (u32, u32),
        slot: Arc<Slot>,
        decode: Job<()>,
    ) {
        let image = &mut self.images[index];
        image.width = width;
        image.height = height;
        image.footprint = footprint(width, height);
        image.data = Some(slot);
        image._decode = Some(decode);
        self.used += image.footprint;
        self.generation += 1;
    }

    /// Evict until the quota holds, keeping the image just transmitted:
    /// images with no data or no placements first, then transient ones, then
    /// the least recently used.  kitty's `apply_storage_quota`.
    pub(crate) fn apply_quota(&mut self, keep: Option<u64>) {
        if self.used <= self.quota {
            return;
        }
        self.remove_where(|image| {
            Some(image.internal_id) != keep && (!image.has_data() || image.placements.is_empty())
        });
        while self.used > self.quota {
            let oldest = self
                .images
                .iter()
                .enumerate()
                .min_by_key(|(_, image)| (!image.transient, image.atime))
                .map(|(index, _)| index);
            match oldest {
                Some(index) => self.remove_at(index),
                None => break,
            }
        }
    }

    pub(crate) fn remove_at(&mut self, index: usize) {
        let image = self.images.remove(index);
        self.used -= image.footprint;
        self.generation += 1;
    }

    fn remove_where(&mut self, mut remove: impl FnMut(&Image) -> bool) {
        let mut index = 0;
        while index < self.images.len() {
            if remove(&self.images[index]) {
                self.remove_at(index);
            } else {
                index += 1;
            }
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// kitty's `handle_put_command` once the image is found: place it at
    /// screen `(line, column)`, or move the placement with the same `p`.
    /// Returns how far the cursor moves after it.
    pub(crate) fn put(
        &mut self,
        index: usize,
        command: &Command,
        (line, column): (usize, usize),
        cell: CellSize,
    ) -> Option<CursorMove> {
        let atime = self.tick();
        let row = self.scrolled + line as i64;
        let image = &mut self.images[index];
        image.atime = atime;

        let (width, height) = (image.width as f32, image.height as f32);
        let (src_x, src_y) = (command.x as f32, command.y as f32);
        let src_width = if command.width != 0 { command.width as f32 } else { width };
        let src_height = if command.height != 0 { command.height as f32 } else { height };
        let is_virtual = command.unicode != 0;
        let mut placement = Placement {
            internal_id: 0,
            client_id: if image.client_id != 0 { command.placement } else { 0 },
            row: if is_virtual { 0 } else { row },
            column: if is_virtual { 0 } else { column as i32 },
            cell_x: command.cell_x.min(cell.width - 1),
            cell_y: command.cell_y.min(cell.height - 1),
            src_x,
            src_y,
            src_width: src_width.min(width - src_x.min(width)),
            src_height: src_height.min(height - src_y.min(height)),
            columns: command.columns,
            rows: command.rows,
            effective_columns: 0,
            effective_rows: 0,
            z: command.z,
            is_virtual,
        };
        placement.fit(cell);
        let movement = (!is_virtual && command.cursor_movement != 1).then_some(CursorMove {
            columns: placement.effective_columns,
            rows: placement.effective_rows.saturating_sub(1),
        });

        let existing = (placement.client_id != 0)
            .then(|| image.placements.iter().position(|p| p.client_id == placement.client_id))
            .flatten();
        match existing {
            Some(at) => {
                placement.internal_id = image.placements[at].internal_id;
                image.placements[at] = placement;
                self.recompute_expiry();
            },
            None => {
                image.next_placement += 1;
                placement.internal_id = image.next_placement;
                self.expiry = self.expiry.min(expiry(&placement, self.history));
                image.placements.push(placement);
            },
        }
        self.generation += 1;
        movement
    }

    /// kitty's `handle_delete_command` once any upload is aborted.
    /// `(line, column)` is the cursor on screen.
    pub(crate) fn delete(&mut self, command: &Command, (line, column): (usize, usize)) {
        let letter = command.delete;
        if command.placement == 0 {
            let target = match letter {
                b'I' => self.index_by_id(command.id),
                b'N' => self.index_by_number(command.number),
                b'R' => {
                    self.remove_where(|image| {
                        in_range(image.client_id, command) && image.placements.is_empty()
                    });
                    None
                },
                _ => None,
            };
            if let Some(index) = target.filter(|&index| self.images[index].placements.is_empty()) {
                self.remove_at(index);
                return;
            }
        }

        let free = letter.is_ascii_uppercase();
        let (x, y) = (i64::from(command.x) - 1, i64::from(command.y) - 1);
        let point = |p: &Placement, row: i64, x: i64, y: i64| {
            !p.is_virtual && p.covers_column(x) && p.covers_row(y, row)
        };
        match letter.to_ascii_lowercase() {
            0 | b'a' => self.filter(free, |p, _, row| !p.is_virtual && p.bottom(row) > 0),
            b'i' => self.filter(free, |p, id, _| {
                command.id != 0
                    && id == command.id
                    && (command.placement == 0 || p.client_id == command.placement)
            }),
            b'r' => self.filter(free, |_, id, _| in_range(id, command)),
            b'p' => self.filter(free, |p, _, row| point(p, row, x, y)),
            b'q' => self.filter(free, |p, _, row| point(p, row, x, y) && p.z == command.z),
            b'x' => self.filter(free, |p, _, _| !p.is_virtual && p.covers_column(x)),
            b'y' => self.filter(free, |p, _, row| !p.is_virtual && p.covers_row(y, row)),
            b'z' => self.filter(free, |p, _, _| !p.is_virtual && p.z == command.z),
            b'c' => {
                let (x, y) = (column as i64, line as i64);
                self.filter(free, |p, _, row| point(p, row, x, y));
            },
            b'n' => self.delete_newest(command, letter == b'N'),
            // Animation frames, which images here never have beyond the first.
            _ => (),
        }
    }

    fn delete_newest(&mut self, command: &Command, free: bool) {
        let Some(index) = self.index_by_number(command.number) else {
            return;
        };
        let image = &mut self.images[index];
        let before = image.placements.len();
        image.placements.retain(|p| command.placement != 0 && p.client_id != command.placement);
        if image.placements.len() != before {
            self.generation += 1;
        }
        if image.placements.is_empty() && (free || image.client_id == 0) {
            self.remove_at(index);
        }
    }

    /// kitty's `filter_refs` with `free_only_matched`: remove the placements
    /// `matches` picks, given each one's image id and screen row, then the
    /// images left without placements that had one removed, if `free` or
    /// they have no id.
    fn filter(&mut self, free: bool, matches: impl Fn(&Placement, u32, i64) -> bool) {
        let scrolled = self.scrolled;
        let mut index = 0;
        while index < self.images.len() {
            let image = &mut self.images[index];
            let id = image.client_id;
            let before = image.placements.len();
            image.placements.retain(|p| !matches(p, id, p.row - scrolled));
            let matched = image.placements.len() != before;
            if matched {
                self.generation += 1;
            }
            let image = &self.images[index];
            if matched && image.placements.is_empty() && (free || image.client_id == 0) {
                self.remove_at(index);
            } else {
                index += 1;
            }
        }
    }

    /// kitty's `grman_clear`: remove every drawn placement, or only those
    /// with a row on screen, then every image left without placements.
    pub(crate) fn clear(&mut self, all: bool) {
        let scrolled = self.scrolled;
        for image in &mut self.images {
            image.placements.retain(|p| p.is_virtual || !(all || p.bottom(p.row - scrolled) > 0));
        }
        self.remove_where(|image| image.placements.is_empty());
        self.recompute_expiry();
        self.generation += 1;
    }

    /// Scroll the lines of `region` by `delta` rows, negative for up.
    pub(crate) fn scroll(&mut self, region: &ScrollRegion, delta: i32, cell: CellSize) {
        self.set_history(region.history);
        self.generation += 1;
        if region.top == 0 && region.bottom == region.screen_lines {
            self.shift(delta);
            return;
        }

        let (top, bottom) = (region.top as i64, region.bottom as i64 - 1);
        let scrolled = self.scrolled;
        for image in &mut self.images {
            image.placements.retain_mut(|p| {
                if p.is_virtual {
                    return true;
                }
                let mut row = p.row - scrolled;
                let remove = p.scroll_within(&mut row, i64::from(delta), top, bottom, cell);
                p.row = row + scrolled;
                !remove
            });
        }
        self.remove_unreachable();
        self.recompute_expiry();
    }

    /// Move every placement by `delta` rows, for a scroll of the whole
    /// screen or content a resize moved.
    pub(crate) fn shift(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.scrolled -= i64::from(delta);
        self.generation += 1;
        if self.scrolled >= self.expiry {
            self.prune();
        }
    }

    /// Drop placements that scrolled past the history, and the unnamed
    /// images they leave behind.
    fn prune(&mut self) {
        let limit = self.scrolled - self.history;
        for image in &mut self.images {
            image.placements.retain(|p| p.is_virtual || p.bottom(p.row) > limit);
        }
        self.remove_unreachable();
        self.recompute_expiry();
    }

    /// Images without placements that no command can name again.
    fn remove_unreachable(&mut self) {
        self.remove_where(|image| {
            image.placements.is_empty() && image.client_id == 0 && image.number == 0
        });
    }

    fn recompute_expiry(&mut self) {
        let history = self.history;
        self.expiry = self
            .images
            .iter()
            .flat_map(|image| &image.placements)
            .filter(|p| !p.is_virtual)
            .map(|p| expiry(p, history))
            .min()
            .unwrap_or(i64::MAX);
    }

    /// kitty's `grman_rescale`.
    pub(crate) fn rescale(&mut self, cell: CellSize) {
        for placement in self.images.iter_mut().flat_map(|image| &mut image.placements) {
            placement.rescale(cell);
        }
        self.recompute_expiry();
        self.generation += 1;
    }

    /// Push the drawn placements `viewport` shows into `out`, in no order.
    pub(crate) fn collect(&self, viewport: Viewport, cell: CellSize, out: &mut Vec<Visible>) {
        let (rows, columns) = (viewport.rows as f64, viewport.columns as f64);
        let offset = self.scrolled - viewport.display_offset as i64;
        for (index, image) in self.images.iter().enumerate() {
            if image.data.as_ref().and_then(|slot| slot.pixels()).is_none() {
                continue;
            }
            for p in image.placements.iter().filter(|p| !p.is_virtual) {
                let Some([left, top, right, bottom]) = p.dest((p.row - offset) as f64, cell) else {
                    continue;
                };
                if top >= rows || bottom <= 0.0 || left >= columns || right <= 0.0 {
                    continue;
                }
                let quad = ImageQuad {
                    dest: [left as f32, top as f32, right as f32, bottom as f32],
                    src: [p.src_x, p.src_y, p.src_x + p.src_width, p.src_y + p.src_height],
                };
                out.push(Visible {
                    key: (p.z, image.internal_id, p.internal_id),
                    image: index,
                    quad,
                });
            }
        }
    }
}

fn footprint(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// The scroll count at which `placement` leaves a history of `history`
/// lines: kitty drops a placement whose bottom is at or above `-history`.
fn expiry(placement: &Placement, history: i64) -> i64 {
    placement.bottom(placement.row).saturating_add(history)
}

fn in_range(id: u32, command: &Command) -> bool {
    id != 0 && command.x <= id && id <= command.y
}
