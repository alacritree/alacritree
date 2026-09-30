//! Sixel images, from the data of `DCS P1;P2;P3 q ... ST` to RGBA.
//!
//! [`measure`] sizes an image under the terminal lock without allocating,
//! and [`decode`] draws it on the job pool. Both walk the same [`Tokens`].
//! The colour maths is Windows Terminal's `SixelParser.cpp`. Colour
//! registers are private to each image, as in xterm's default mode, so one
//! decode never waits on the one before it.

use crate::MAX_DIMENSION;
use crate::frame::Pixels;

/// Colour registers an image has.
pub const REGISTERS: usize = 256;

/// A longer run of digits saturates here, Windows Terminal's limit.
const MAX_PARAM: u32 = u16::MAX as u32;

/// Pixel rows one sixel covers at most, so a band stays within an image.
const MAX_ASPECT: u32 = MAX_DIMENSION / 6;

/// The register drawn with before `#` selects one: the VT340's foreground.
const FOREGROUND: usize = 15;

/// The VT340's colours, then xterm's 6x6x6 cube and grey ramp.
const PALETTE: [[u8; 3]; REGISTERS] = palette();

const VT340: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0x33, 0x33, 0xCC],
    [0xCC, 0x24, 0x24],
    [0x33, 0xCC, 0x33],
    [0xCC, 0x33, 0xCC],
    [0x33, 0xCC, 0xCC],
    [0xCC, 0xCC, 0x33],
    [0x78, 0x78, 0x78],
    [0x45, 0x45, 0x45],
    [0x57, 0x57, 0x99],
    [0x99, 0x45, 0x45],
    [0x57, 0x99, 0x57],
    [0x99, 0x57, 0x99],
    [0x57, 0x99, 0x99],
    [0x99, 0x99, 0x57],
    [0xCC, 0xCC, 0xCC],
];

const fn palette() -> [[u8; 3]; REGISTERS] {
    const CUBE: [u8; 6] = [0x00, 0x5F, 0x87, 0xAF, 0xD7, 0xFF];
    let mut palette = [[0; 3]; REGISTERS];
    let mut i = 0;
    while i < 16 {
        palette[i] = VT340[i];
        i += 1;
    }
    let mut i = 0;
    while i < 216 {
        palette[16 + i] = [CUBE[i / 36], CUBE[i / 6 % 6], CUBE[i % 6]];
        i += 1;
    }
    let mut i = 0;
    while i < 24 {
        let grey = 8 + 10 * i as u8;
        palette[232 + i] = [grey, grey, grey];
        i += 1;
    }
    palette
}

/// An image's size and how to draw it, known before its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub width: u32,
    pub height: u32,
    /// Pixel rows each sixel bit covers.
    aspect: u32,
    /// P2 = 1: pixels no sixel paints stay transparent.
    transparent: bool,
}

impl Geometry {
    fn new(width: u32, height: u32, aspect: u32, transparent: bool) -> Option<Self> {
        let (width, height) = (width.min(MAX_DIMENSION), height.min(MAX_DIMENSION));
        (width != 0 && height != 0).then_some(Self { width, height, aspect, transparent })
    }
}

/// Size the image `data` draws, or `None` when it draws nothing.
///
/// Raster attributes before the first sixel fix the size and aspect ratio.
/// Without them the size is the painted extent. P1's aspect ratio is
/// ignored, since producers that send no raster attributes expect square
/// pixels.
pub(crate) fn measure(params: [u16; 3], data: &[u8]) -> Option<Geometry> {
    let transparent = params[1] == 1;
    let mut aspect = 1;
    let (mut drawn, mut x, mut band, mut width, mut bands) = (false, 0u32, 0u32, 0u32, 0u32);
    for token in Tokens::new(data) {
        match token {
            Token::Raster([pan, pad, raster_width, raster_height]) if !drawn => {
                if pad != 0 {
                    aspect = pan.div_ceil(pad).clamp(1, MAX_ASPECT);
                }
                if raster_width != 0 && raster_height != 0 {
                    return Geometry::new(raster_width, raster_height, aspect, transparent);
                }
            },
            Token::Sixel { bits, count } => {
                drawn = true;
                x = x.saturating_add(count);
                width = width.max(x);
                if bits != 0 {
                    bands = band + 1;
                }
            },
            Token::CarriageReturn => x = 0,
            Token::NextLine => {
                x = 0;
                band = band.saturating_add(1);
            },
            Token::Raster(_) | Token::Colour { .. } => (),
        }
    }
    Geometry::new(width, bands.saturating_mul(6 * aspect), aspect, transparent)
}

/// Draw `data` at the size [`measure`] gave it, clipping what falls outside.
pub(crate) fn decode(data: &[u8], geometry: Geometry) -> Pixels {
    let Geometry { width, height, aspect, transparent } = geometry;
    let (width_px, height_px, aspect) = (width as usize, height as usize, aspect as usize);
    let mut palette = PALETTE;
    let mut rgba = vec![0; width_px * height_px * 4];
    if !transparent {
        let [r, g, b] = palette[0];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[r, g, b, 0xFF]);
        }
    }

    let mut colour = palette[FOREGROUND];
    let (mut x, mut top) = (0usize, 0usize);
    for token in Tokens::new(data) {
        match token {
            Token::Sixel { bits, count } => {
                let end = x.saturating_add(count as usize).min(width_px);
                if x < end && bits != 0 {
                    let [r, g, b] = colour;
                    for bit in (0..6).filter(|bit| bits & (1 << bit) != 0) {
                        let first = top.saturating_add(bit * aspect);
                        for y in first..first.saturating_add(aspect).min(height_px) {
                            let row = &mut rgba[(y * width_px + x) * 4..(y * width_px + end) * 4];
                            for pixel in row.chunks_exact_mut(4) {
                                pixel.copy_from_slice(&[r, g, b, 0xFF]);
                            }
                        }
                    }
                }
                x = x.saturating_add(count as usize);
            },
            Token::Colour { params, len } => {
                let register = params[0] as usize % REGISTERS;
                if len > 1 {
                    let [_, model, a, b, c] = params;
                    match model {
                        1 => palette[register] = hls(a, b, c),
                        2 => palette[register] = [percent(a), percent(b), percent(c)],
                        _ => (),
                    }
                }
                colour = palette[register];
            },
            Token::CarriageReturn => x = 0,
            Token::NextLine => {
                x = 0;
                top = top.saturating_add(6 * aspect);
            },
            Token::Raster(_) => (),
        }
    }
    Pixels::new(width, height, rgba.into_boxed_slice())
}

/// A percentage of full intensity, rounded.
fn percent(value: u32) -> u8 {
    ((value.min(100) * 255 + 50) / 100) as u8
}

/// DEC's HLS, where blue is at 0°, red at 120° and green at 240°.
fn hls(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let hue = hue % 360;
    let (lum, sat) = (lightness.min(100) as f32, saturation.min(100) as f32);
    let chroma = (50.0 - (lum - 50.0).abs()) * sat / 50.0;
    let x = chroma * (60.0 - ((hue % 120) as f32 - 60.0).abs()) / 60.0;
    let base = lum - chroma / 2.0;
    let scale = |value: f32| (value * 255.0 / 100.0 + 0.5) as u8;
    let (max, mid, min) = (scale(chroma + base), scale(x + base), scale(base));
    match hue {
        0..60 => [mid, min, max],
        60..120 => [max, min, mid],
        120..180 => [max, mid, min],
        180..240 => [mid, max, min],
        240..300 => [min, max, mid],
        _ => [min, mid, max],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    /// Six pixels stacked, bit 0 on top, drawn `count` times across.
    Sixel { bits: u8, count: u32 },
    /// `#Pc` selects a register, and defines it first when more params
    /// follow. `len` counts the params given.
    Colour { params: [u32; 5], len: usize },
    /// `"Pan;Pad;Ph;Pv`.
    Raster([u32; 4]),
    /// `$`.
    CarriageReturn,
    /// `-`: back to the left edge of the next band.
    NextLine,
}

struct Tokens<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Tokens<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.at).copied()
    }

    /// Read `;`-separated numbers into `params`, absent ones 0, and return
    /// how many were given. Extra ones are skipped.
    fn params(&mut self, params: &mut [u32]) -> usize {
        let (mut index, mut given) = (0, false);
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => {
                    if let Some(param) = params.get_mut(index) {
                        *param = (*param * 10 + u32::from(byte - b'0')).min(MAX_PARAM);
                    }
                },
                b';' => index += 1,
                _ => break,
            }
            given = true;
            self.at += 1;
        }
        if given { (index + 1).min(params.len()) } else { 0 }
    }
}

fn sixel_bits(byte: u8) -> Option<u8> {
    (b'?'..=b'~').contains(&byte).then(|| byte - b'?')
}

impl Iterator for Tokens<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            let byte = self.peek()?;
            self.at += 1;
            let token = match byte {
                b'?'..=b'~' => Token::Sixel { bits: byte - b'?', count: 1 },
                b'!' => {
                    let mut count = [0];
                    self.params(&mut count);
                    // A repeat applies to the sixel right after it and to
                    // nothing else.
                    let Some(bits) = self.peek().and_then(sixel_bits) else {
                        continue;
                    };
                    self.at += 1;
                    Token::Sixel { bits, count: count[0].max(1) }
                },
                b'#' => {
                    let mut params = [0; 5];
                    match self.params(&mut params) {
                        0 => continue,
                        len => Token::Colour { params, len },
                    }
                },
                b'"' => {
                    let mut params = [0; 4];
                    self.params(&mut params);
                    Token::Raster(params)
                },
                b'$' => Token::CarriageReturn,
                b'-' => Token::NextLine,
                _ => continue,
            };
            return Some(token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [0xFF, 0, 0, 0xFF];
    const GREEN: [u8; 4] = [0, 0xFF, 0, 0xFF];
    const BLUE: [u8; 4] = [0, 0, 0xFF, 0xFF];
    const BLACK: [u8; 4] = [0, 0, 0, 0xFF];
    const CLEAR: [u8; 4] = [0; 4];

    /// Decode `data` with transparent background, as `(width, height, rows)`.
    fn image(data: &str) -> (u32, u32, Vec<Vec<[u8; 4]>>) {
        image_with([0, 1, 0], data)
    }

    fn image_with(params: [u16; 3], data: &str) -> (u32, u32, Vec<Vec<[u8; 4]>>) {
        let geometry = measure(params, data.as_bytes()).expect("an image");
        let pixels = decode(data.as_bytes(), geometry);
        let (width, height) = (pixels.width(), pixels.height());
        let rows = pixels
            .rgba()
            .chunks_exact(width as usize * 4)
            .map(|row| row.chunks_exact(4).map(|pixel| pixel.try_into().unwrap()).collect())
            .collect();
        (width, height, rows)
    }

    fn column(rows: &[Vec<[u8; 4]>], x: usize) -> Vec<[u8; 4]> {
        rows.iter().map(|row| row[x]).collect()
    }

    #[test]
    fn rgb_registers_are_percentages() {
        let (_, _, rows) = image("#1;2;100;0;0#1~#2;2;0;100;0#2~#3;2;0;0;100#3~#4;2;50;50;50#4~");
        assert_eq!(rows[0][..3], [RED, GREEN, BLUE]);
        assert_eq!(rows[0][3], [0x80, 0x80, 0x80, 0xFF]);
    }

    #[test]
    fn hls_registers_put_blue_at_zero_degrees() {
        let (_, _, rows) =
            image("#1;1;0;50;100#1~#1;1;120;50;100#1~#1;1;240;50;100#1~#1;1;0;100;0#1~");
        assert_eq!(rows[0], [BLUE, RED, GREEN, [0xFF; 4]]);
    }

    #[test]
    fn selecting_a_register_uses_the_vt340_palette() {
        let (_, _, rows) = image("#2~#0~~#255~");
        assert_eq!(rows[0], [[0xCC, 0x24, 0x24, 0xFF], BLACK, BLACK, [0xEE, 0xEE, 0xEE, 0xFF]]);
        // Before any `#`, the VT340's foreground.
        assert_eq!(image("~").2[0], [[0xCC, 0xCC, 0xCC, 0xFF]]);
    }

    #[test]
    fn a_register_redefined_later_keeps_the_pixels_drawn_before() {
        let (_, _, rows) = image("#1;2;100;0;0~#1;2;0;0;100~#257~");
        assert_eq!(rows[0], [RED, BLUE, BLUE]);
    }

    #[test]
    fn repeat_draws_the_next_sixel_that_many_times() {
        let (width, _, rows) = image("#1;2;100;0;0!3~!0~!~");
        assert_eq!(width, 5);
        assert_eq!(rows[5], [RED; 5]);
        // A repeat followed by anything but a sixel is dropped.
        assert_eq!(image("!3#1;2;100;0;0~").0, 1);
    }

    #[test]
    fn each_bit_is_one_row_from_the_top() {
        // `@` sets bit 0, `A` bit 1 and `_` bit 5.
        let (_, height, rows) = image("#1;2;100;0;0@A$_");
        assert_eq!(height, 6);
        assert_eq!(column(&rows, 0), [RED, CLEAR, CLEAR, CLEAR, CLEAR, RED]);
        assert_eq!(column(&rows, 1), [CLEAR, RED, CLEAR, CLEAR, CLEAR, CLEAR]);
    }

    #[test]
    fn raster_attributes_size_and_clip_the_image() {
        let (width, height, rows) = image("\"1;1;2;3#1;2;100;0;0~~~-~");
        assert_eq!((width, height), (2, 3));
        assert_eq!(rows, vec![vec![RED; 2]; 3]);
    }

    #[test]
    fn raster_aspect_ratio_stretches_each_sixel() {
        let (width, height, rows) = image("\"2;1#1;2;100;0;0@");
        assert_eq!((width, height), (1, 12));
        assert_eq!(column(&rows, 0)[..3], [RED, RED, CLEAR]);
        // P1's aspect ratio, 5:1 for P1 = 2, does not.
        assert_eq!(image_with([2, 1, 0], "@").1, 6);
    }

    #[test]
    fn raster_attributes_after_a_sixel_are_ignored() {
        assert_eq!(image("~\"1;1;9;9").0, 1);
    }

    #[test]
    fn background_select_fills_with_register_zero_or_leaves_it_clear() {
        let data = "\"1;1;2;6#0;2;100;0;0#0@";
        let (_, _, filled) = image_with([0, 0, 0], data);
        assert_eq!(column(&filled, 0), [RED, BLACK, BLACK, BLACK, BLACK, BLACK]);
        assert_eq!(column(&filled, 1), [BLACK; 6]);
        assert_eq!(filled, image_with([0, 2, 0], data).2);

        let (_, _, clear) = image_with([0, 1, 0], data);
        assert_eq!(column(&clear, 0), [RED, CLEAR, CLEAR, CLEAR, CLEAR, CLEAR]);
        assert_eq!(column(&clear, 1), [CLEAR; 6]);
    }

    #[test]
    fn without_raster_attributes_the_painted_extent_sizes_the_image() {
        let (width, height, _) = image("~~-~");
        assert_eq!((width, height), (2, 12));
        // A trailing band with nothing drawn adds no height.
        assert_eq!(image("~-??-").1, 6);
        assert_eq!(measure([0, 1, 0], b"??-"), None);
        assert_eq!(measure([0, 1, 0], b""), None);
    }

    #[test]
    fn a_size_past_the_limit_is_capped() {
        let geometry = measure([0, 1, 0], b"!65535~!65535~").unwrap();
        assert_eq!(geometry.width, MAX_DIMENSION);
    }
}
