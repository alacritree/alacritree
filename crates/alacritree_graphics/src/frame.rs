//! The visible images of one screen, laid out for the renderer.
//!
//! The graphics layer fills an [`ImageFrame`] from its placements and the
//! renderer draws it as it stands, so this module is the whole contract
//! between them.  Everything here is plain data: no GL and no terminal lock.
//!
//! A frame is rebuilt only when something it depends on changed, and a
//! rebuild reuses the vectors it already holds, so a steady screen costs
//! neither allocation nor work here.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where a placement draws relative to the grid's own passes, from its z.
///
/// The order of the variants is the draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Band {
    /// `z < -2^30`: over cells with the default background, under coloured
    /// ones.
    UnderBackgrounds,
    /// `-2^30 <= z < 0`: over every background, under the text.
    UnderText,
    /// `z >= 0`: over the text and its decorations.
    OverText,
}

impl Band {
    pub const ALL: [Band; 3] = [Band::UnderBackgrounds, Band::UnderText, Band::OverText];

    pub fn of(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Band::UnderBackgrounds
        } else if z < 0 {
            Band::UnderText
        } else {
            Band::OverText
        }
    }
}

/// Identifies one set of decoded pixels for the life of the process.
///
/// A key is never reused, so a texture uploaded under it stays valid for as
/// long as the key is alive, and new content always arrives under a new key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PixelsKey(u64);

/// Decoded pixels of one image: RGBA8 with straight alpha, rows top first.
#[derive(Debug)]
pub struct Pixels {
    key: PixelsKey,
    width: u32,
    height: u32,
    rgba: Box<[u8]>,
}

impl Pixels {
    /// # Panics
    ///
    /// When `rgba` is not exactly `width * height * 4` bytes.
    pub fn new(width: u32, height: u32, rgba: Box<[u8]>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        assert_eq!(rgba.len(), width as usize * height as usize * 4, "{width}x{height} RGBA");
        Self { key: PixelsKey(NEXT.fetch_add(1, Ordering::Relaxed)), width, height, rgba }
    }

    pub fn key(&self) -> PixelsKey {
        self.key
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// One visible placement: where it lands and which texels it shows.
///
/// This is the instance record the renderer uploads as it stands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ImageQuad {
    /// `[left, top, right, bottom]`, in cells from the top-left corner of
    /// the viewport's first cell.  Fractions carry pixel offsets and native
    /// image sizes, so the renderer only scales by its cell size.  A
    /// placement partly above the viewport has a negative top.
    pub dest: [f32; 4],
    /// `[left, top, right, bottom]`, in texels of the image.
    pub src: [f32; 4],
}

/// Consecutive quads that sample one image, all in one band.
#[derive(Clone, Debug)]
pub struct ImageRun {
    pub band: Band,
    pub pixels: Arc<Pixels>,
    pub quads: Range<u32>,
}

/// Everything the image passes draw for one frame.
#[derive(Debug, Default)]
pub struct ImageFrame {
    /// Instance records in draw order: band, then z, then the order the
    /// images were created in, then placement.
    quads: Vec<ImageQuad>,
    /// Runs in the same order, each covering a contiguous span of `quads`.
    runs: Vec<ImageRun>,
    /// Changes whenever the frame is rebuilt, so the renderer uploads the
    /// quads only when they may differ from what it holds.
    generation: u64,
}

impl ImageFrame {
    /// Start a rebuild, keeping both vectors' capacity.
    pub fn clear(&mut self) {
        self.quads.clear();
        self.runs.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Append one quad.  Quads must arrive in draw order; one that continues
    /// the last run's band and pixels extends it.
    pub fn push(&mut self, band: Band, pixels: &Arc<Pixels>, quad: ImageQuad) {
        debug_assert!(self.runs.last().is_none_or(|last| last.band <= band), "bands out of order");
        let at = self.quads.len() as u32;
        self.quads.push(quad);
        match self.runs.last_mut() {
            Some(last) if last.band == band && last.pixels.key() == pixels.key() => {
                last.quads.end = at + 1;
            },
            _ => self.runs.push(ImageRun { band, pixels: Arc::clone(pixels), quads: at..at + 1 }),
        }
    }

    pub fn quads(&self) -> &[ImageQuad] {
        &self.quads
    }

    pub fn runs(&self) -> &[ImageRun] {
        &self.runs
    }

    /// The runs of one band, in draw order.
    pub fn band(&self, band: Band) -> &[ImageRun] {
        let start = self.runs.partition_point(|run| run.band < band);
        let end = self.runs.partition_point(|run| run.band <= band);
        &self.runs[start..end]
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels() -> Arc<Pixels> {
        Arc::new(Pixels::new(1, 1, vec![0; 4].into()))
    }

    fn quad(left: f32) -> ImageQuad {
        ImageQuad { dest: [left, 0.0, left + 1.0, 1.0], src: [0.0, 0.0, 1.0, 1.0] }
    }

    #[test]
    fn z_splits_into_bands_at_minus_two_to_the_thirty_and_zero() {
        assert_eq!(Band::of(i32::MIN), Band::UnderBackgrounds);
        assert_eq!(Band::of(-(1 << 30) - 1), Band::UnderBackgrounds);
        assert_eq!(Band::of(-(1 << 30)), Band::UnderText);
        assert_eq!(Band::of(-1), Band::UnderText);
        assert_eq!(Band::of(0), Band::OverText);
    }

    #[test]
    fn placements_of_one_image_in_one_band_share_a_run() {
        let (a, b) = (pixels(), pixels());
        let mut frame = ImageFrame::default();

        frame.push(Band::UnderText, &a, quad(0.0));
        frame.push(Band::UnderText, &a, quad(1.0));
        frame.push(Band::UnderText, &b, quad(2.0));
        frame.push(Band::OverText, &b, quad(3.0));

        let spans: Vec<_> = frame.runs().iter().map(|r| (r.band, r.quads.clone())).collect();
        assert_eq!(spans, [
            (Band::UnderText, 0..2),
            (Band::UnderText, 2..3),
            (Band::OverText, 3..4)
        ]);
        assert!(frame.band(Band::UnderBackgrounds).is_empty());
        assert_eq!(frame.band(Band::UnderText).len(), 2);
        assert_eq!(frame.band(Band::OverText)[0].quads, 3..4);
    }

    #[test]
    fn a_rebuild_keeps_capacity_and_moves_the_generation() {
        let a = pixels();
        let mut frame = ImageFrame::default();
        frame.push(Band::OverText, &a, quad(0.0));
        let (generation, capacity) = (frame.generation(), frame.quads.capacity());

        frame.clear();

        assert!(frame.is_empty());
        assert_ne!(frame.generation(), generation);
        assert_eq!(frame.quads.capacity(), capacity);
    }

    #[test]
    fn every_set_of_pixels_gets_its_own_key() {
        assert_ne!(pixels().key(), pixels().key());
    }
}
