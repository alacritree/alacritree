//! Turning finished image data into [`Pixels`], on the job pool.
//!
//! A transmission is answered under the terminal lock before any of this
//! runs, so an error found here reaches only the log: the image stays
//! without pixels and draws nothing. kitty decodes in its parser and can
//! still reply, which this crate cannot without holding the lock through
//! the decode.

use std::fmt::Display;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use alacritree_common::jobs::{self, Job, Priority};

use crate::Waker;
use crate::frame::Pixels;
use crate::load::{Format, MAX_DATA_SIZE};

/// Where an image's pixels land once decoded, shared between the store and
/// the job filling it, so the job never takes the terminal lock.
#[derive(Default)]
pub(crate) struct Slot {
    pixels: OnceLock<Arc<Pixels>>,
    failed: AtomicBool,
}

impl Slot {
    pub(crate) fn pixels(&self) -> Option<&Arc<Pixels>> {
        self.pixels.get()
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

pub(crate) enum Source {
    Bytes(Vec<u8>),
    File { file: File, offset: u64, len: usize },
}

/// Everything a decode needs, fixed at parse time.
pub(crate) struct Decode {
    pub source: Source,
    pub format: Format,
    pub compressed: bool,
    pub width: u32,
    pub height: u32,
    /// The bytes raw pixel data must hold.
    pub raw_size: usize,
}

#[derive(Debug, thiserror::Error)]
enum DecodeError {
    #[error("failed to read the image file: {0}")]
    Read(#[source] io::Error),
    #[error("failed to inflate image data: {0}")]
    Inflate(#[source] io::Error),
    #[error("image data size post inflation does not match expected size: {got} != {expected}")]
    InflatedSize { got: usize, expected: usize },
    #[error("insufficient image data: {got} < {expected}")]
    Insufficient { got: usize, expected: usize },
    #[error("failed to decode PNG: {0}")]
    Png(#[from] png::DecodingError),
    #[error("PNG size changed from its header: {0}x{1}")]
    PngSize(u32, u32),
    #[error("PNG palette was not expanded")]
    Palette,
}

impl Decode {
    /// Decode on the job pool, as [`spawn`] does.
    pub(crate) fn spawn(
        self,
        slot: Arc<Slot>,
        ready: Arc<AtomicU64>,
        waker: Option<Waker>,
    ) -> Job<()> {
        spawn(move || self.run(), slot, ready, waker)
    }

    fn run(self) -> Result<Pixels, DecodeError> {
        let data = match self.source {
            Source::Bytes(data) => data,
            Source::File { file, offset, len } => {
                read_file(file, offset, len).map_err(DecodeError::Read)?
            },
        };
        let data = if self.compressed {
            let limit = match self.format {
                Format::Png => MAX_DATA_SIZE,
                _ => self.raw_size,
            };
            let data = inflate(&data, limit).map_err(DecodeError::Inflate)?;
            if self.format != Format::Png && data.len() != self.raw_size {
                return Err(DecodeError::InflatedSize { got: data.len(), expected: self.raw_size });
            }
            data
        } else {
            data
        };

        let rgba = match self.format {
            Format::Png => decode_png(&data, self.width, self.height)?,
            raw => {
                if data.len() < self.raw_size {
                    let (got, expected) = (data.len(), self.raw_size);
                    return Err(DecodeError::Insufficient { got, expected });
                }
                let data = &data[..self.raw_size];
                match raw {
                    Format::Rgb => data
                        .chunks_exact(3)
                        .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 0xFF])
                        .collect(),
                    _ => data.to_vec(),
                }
            },
        };
        Ok(Pixels::new(self.width, self.height, rgba.into_boxed_slice()))
    }
}

/// Run `decode` on the job pool into `slot`, then bump `ready` and wake the
/// pane. Dropping the returned job before it starts cancels it.
pub(crate) fn spawn<E: Display>(
    decode: impl FnOnce() -> Result<Pixels, E> + Send + 'static,
    slot: Arc<Slot>,
    ready: Arc<AtomicU64>,
    waker: Option<Waker>,
) -> Job<()> {
    jobs::pool().spawn(Priority::Interactive, move |_| match decode() {
        Ok(pixels) => {
            let _ = slot.pixels.set(Arc::new(pixels));
            ready.fetch_add(1, Ordering::Release);
            if let Some(waker) = waker {
                waker();
            }
        },
        Err(error) => {
            log::warn!("graphics: {error}");
            slot.failed.store(true, Ordering::Release);
        },
    })
}

fn read_file(mut file: File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut data)?;
    Ok(data)
}

/// Inflate a zlib stream, refusing to grow past `limit` bytes.
fn inflate(data: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).take(limit as u64 + 1).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(io::Error::other("inflates past the size the image can have"));
    }
    Ok(out)
}

/// Decode a PNG to straight-alpha RGBA8, whatever its colour type and
/// depth, as kitty's libpng setup does.
fn decode_png(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, DecodeError> {
    let limits = png::Limits { bytes: MAX_DATA_SIZE };
    let mut decoder = png::Decoder::new_with_limits(data, limits);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    if (info.width, info.height) != (width, height) {
        return Err(DecodeError::PngSize(info.width, info.height));
    }
    buf.truncate(info.buffer_size());

    Ok(match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 0xFF]).collect(),
        png::ColorType::GrayscaleAlpha => {
            buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        },
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 0xFF]).collect(),
        png::ColorType::Indexed => return Err(DecodeError::Palette),
    })
}
