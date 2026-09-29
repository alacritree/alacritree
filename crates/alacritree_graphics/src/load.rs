//! A transmission from its first chunk to data ready for decoding: kitty's
//! `LoadData`, `load_image_data` and the checks `process_image_data` makes.
//!
//! Only what the reply depends on is checked here, under the terminal lock:
//! sizes, the PNG header, the zlib header and whether a file can be read.
//! Inflating and decoding the rest happens in [`crate::decode`].

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::mem;
use std::path::{Path, PathBuf};

use crate::command::{Command, Medium};
use crate::decode::{Decode, Source};
use crate::reply::CommandError;

/// kitty's `MAX_DATA_SZ`: the most data one transmission may carry.
pub(crate) const MAX_DATA_SIZE: usize = 400_000_000;

/// PNG data is capped at [`MAX_DATA_SIZE`], so without `S` the buffer
/// starts at kitty's guess and grows.
const PNG_SIZE_GUESS: usize = 100 * 1024;

/// The first bytes of a file read under the lock, enough for any header.
const FILE_HEAD: usize = 64 * 1024;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// kitty's error when a PNG ends before the header is complete.
const PNG_TRUNCATED: &str = "PNG data is truncated: not enough bytes to satisfy read request";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Format {
    Rgb,
    #[default]
    Rgba,
    Png,
}

impl Format {
    pub(crate) fn from_key(format: u32) -> Result<Self, CommandError> {
        match format {
            0 | 32 => Ok(Self::Rgba),
            24 => Ok(Self::Rgb),
            100 => Ok(Self::Png),
            other => Err(CommandError::UnknownFormat(other)),
        }
    }

    pub(crate) fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba | Self::Png => 4,
        }
    }
}

/// What a finished transmission becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The data of the image with this internal id.
    Image(u64),
    /// Nothing: an `a=q` answered under this id.
    Query(u32),
}

/// The one transmission in progress.
#[derive(Default)]
pub(crate) struct Load {
    target: Option<Target>,
    /// The first chunk's command, which continuation chunks inherit.
    pub start: Command,
    format: Format,
    buf: Vec<u8>,
    /// Decoded bytes a raw image needs; for PNG, `S` or a guess.
    data_size: usize,
    /// The size past which more data is `EFBIG`.
    capacity: usize,
}

impl Load {
    pub(crate) fn target(&self) -> Option<Target> {
        self.target
    }

    /// Start a transmission for `target`, from its first chunk.
    pub(crate) fn begin(&mut self, command: Command, target: Target) -> Result<(), CommandError> {
        self.abort();
        self.start = command;
        self.format = Format::from_key(command.format)?;
        self.data_size = match self.format {
            Format::Png => {
                let size = command.data_size as usize;
                if size > MAX_DATA_SIZE {
                    return Err(CommandError::PngDataTooLarge);
                }
                if size == 0 { PNG_SIZE_GUESS } else { size }
            },
            format => {
                let size = command.data_width as usize
                    * command.data_height as usize
                    * format.bytes_per_pixel();
                if size == 0 {
                    return Err(CommandError::ZeroSize);
                }
                size
            },
        };
        self.capacity = self.data_size + if command.compressed { 1024 } else { 10 };
        self.target = Some(target);
        Ok(())
    }

    /// Add a direct chunk. Returns whether it was the last one.
    pub(crate) fn append(&mut self, payload: &[u8], more: bool) -> Result<bool, CommandError> {
        let used = self.buf.len() + payload.len();
        let limit = match self.format {
            Format::Png => self.capacity.max(MAX_DATA_SIZE),
            _ => self.capacity,
        };
        if used > limit {
            self.abort();
            return Err(CommandError::TooMuchData);
        }
        self.buf.extend_from_slice(payload);
        Ok(!more)
    }

    /// Validate the finished direct data and hand it over for decoding.
    pub(crate) fn finish(&mut self) -> Result<Decode, CommandError> {
        let data = mem::take(&mut self.buf);
        self.target = None;
        let (width, height) = self.check(&data, data.len(), false)?;
        Ok(self.decode(Source::Bytes(data), width, height))
    }

    /// Validate an image file and hand it over for decoding. `path` is the
    /// decoded payload.
    pub(crate) fn open_file(&mut self, path: &[u8]) -> Result<Decode, CommandError> {
        self.target = None;
        if path.len() > 2048 {
            return Err(CommandError::FilenameTooLong);
        }
        let medium = self.start.medium.unwrap_or_default();
        if medium == Medium::SharedMemory {
            log::debug!("graphics: shared memory transmission is not supported");
            return Err(CommandError::ImageFile);
        }
        let path = image_path(path).ok_or(CommandError::ImageFile)?;
        let temporary = medium == Medium::TempFile
            && path.to_string_lossy().contains("tty-graphics-protocol")
            && in_temp_dir(&path);
        let (file, offset, len, head) = self.read_file(&path, temporary)?;
        let (width, height) = self.check(&head, len, true)?;
        let source = Source::File { file, offset, len };
        Ok(self.decode(source, width, height))
    }

    /// Drop the transmission in progress and its data.
    pub(crate) fn abort(&mut self) {
        self.target = None;
        self.buf.clear();
        self.buf.shrink_to(FILE_HEAD);
    }

    /// Open `path`, check it is a regular file holding enough data, and read
    /// its head. A `temporary` file is deleted once it is open, as kitty
    /// does, whether or not reading it succeeds. The open handle keeps the
    /// data readable, so nothing that drops the decode later has to remember
    /// the file.
    fn read_file(
        &self,
        path: &Path,
        temporary: bool,
    ) -> Result<(File, u64, usize, Vec<u8>), CommandError> {
        let fail = |why: &dyn std::fmt::Display| {
            log::debug!("graphics: cannot read image file {}: {why}", path.display());
            CommandError::ImageFile
        };
        // Checked before opening, so opening a FIFO or a device cannot block.
        let metadata = std::fs::metadata(path).map_err(|e| fail(&e))?;
        if !metadata.is_file() {
            return Err(fail(&"not a regular file"));
        }
        let mut file = File::open(path).map_err(|e| fail(&e))?;
        if temporary {
            let _ = std::fs::remove_file(path);
        }
        let metadata = file.metadata().map_err(|e| fail(&e))?;
        if !metadata.is_file() {
            return Err(fail(&"not a regular file"));
        }

        let offset = u64::from(self.start.data_offset);
        let available =
            usize::try_from(metadata.len().saturating_sub(offset)).unwrap_or(usize::MAX);
        let wanted = match self.start.data_size {
            0 => available,
            size => size as usize,
        };
        let max = if self.start.compressed || self.format == Format::Png {
            MAX_DATA_SIZE
        } else {
            self.data_size
        };
        let len = wanted.min(max).min(available);

        let mut head = vec![0; len.min(FILE_HEAD)];
        file.seek(SeekFrom::Start(offset)).map_err(|e| fail(&e))?;
        file.read_exact(&mut head).map_err(|e| fail(&e))?;
        Ok((file, offset, len, head))
    }

    /// kitty's size checks, and the PNG and zlib headers. `head` starts the
    /// data, which is `len` bytes long. Returns the image's size.
    fn check(&self, head: &[u8], len: usize, from_file: bool) -> Result<(u32, u32), CommandError> {
        let insufficient = |got| {
            if from_file {
                log::debug!("graphics: image file holds {got} < {} bytes", self.data_size);
                CommandError::ImageFile
            } else {
                CommandError::InsufficientData { got, expected: self.data_size }
            }
        };
        let raw_size = (self.start.data_width, self.start.data_height);
        match (self.format, self.start.compressed) {
            (Format::Png, false) => png_size(head),
            (Format::Png, true) => png_size(&inflate_head(head, 24)?),
            (_, false) if len < self.data_size => Err(insufficient(len)),
            (_, false) => Ok(raw_size),
            (_, true) => inflate_head(head, 1).map(|_| raw_size),
        }
    }

    fn decode(&self, source: Source, width: u32, height: u32) -> Decode {
        Decode {
            source,
            format: self.format,
            compressed: self.start.compressed,
            width,
            height,
            raw_size: self.data_size,
        }
    }
}

/// Whether `path` lies in a temporary directory, the only place kitty lets a
/// client have a file deleted from.
fn in_temp_dir(path: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let mut dirs = vec![std::env::temp_dir()];
    if cfg!(unix) {
        dirs.extend(["/tmp", "/dev/shm"].map(PathBuf::from));
    }
    dirs.iter().filter_map(|dir| std::fs::canonicalize(dir).ok()).any(|dir| path.starts_with(dir))
}

/// The path a client named, if it may be read at all: absolute, and once
/// resolved outside `/proc`, `/sys` and `/dev` as kitty requires. On
/// Windows it must be a drive path, since opening a UNC path authenticates
/// to the server it names.
fn image_path(bytes: &[u8]) -> Option<PathBuf> {
    let path = PathBuf::from(std::str::from_utf8(bytes).ok()?);
    if !path.is_absolute() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        match path.components().next() {
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)) => {},
            _ => return None,
        }
    }
    #[cfg(not(windows))]
    let path = std::fs::canonicalize(&path).ok()?;
    #[cfg(not(windows))]
    {
        let mut parts = path.components().skip(1).map(|part| part.as_os_str());
        match parts.next().and_then(|part| part.to_str()) {
            Some("proc" | "sys") => return None,
            Some("dev") if parts.next() != Some("shm".as_ref()) || parts.next().is_none() => {
                return None;
            },
            _ => {},
        }
    }
    Some(path)
}

/// The size in a PNG's `IHDR`, with kitty's errors for a header libpng
/// rejects.
pub(crate) fn png_size(data: &[u8]) -> Result<(u32, u32), CommandError> {
    let truncated = CommandError::BadPng(PNG_TRUNCATED);
    let signature = data.get(..8).ok_or(truncated)?;
    if signature != PNG_SIGNATURE {
        return Err(CommandError::BadPng("Not a PNG file"));
    }
    let header = data.get(8..24).ok_or(CommandError::BadPng(PNG_TRUNCATED))?;
    let length = u32::from_be_bytes(header[..4].try_into().expect("four bytes"));
    if length != 13 || &header[4..8] != b"IHDR" {
        return Err(CommandError::BadPng("Missing IHDR before IDAT"));
    }
    let width = u32::from_be_bytes(header[8..12].try_into().expect("four bytes"));
    let height = u32::from_be_bytes(header[12..16].try_into().expect("four bytes"));
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(CommandError::BadPng("Invalid IHDR data"));
    }
    if width > crate::MAX_DIMENSION || height > crate::MAX_DIMENSION {
        return Err(CommandError::PngTooLarge);
    }
    Ok((width, height))
}

/// Inflate enough of zlib `data` for `want` bytes, to find a broken stream
/// while the reply can still say so.
fn inflate_head(data: &[u8], want: usize) -> Result<Vec<u8>, CommandError> {
    let mut inflate = flate2::Decompress::new(true);
    let mut out = vec![0; want];
    loop {
        let before = (inflate.total_in(), inflate.total_out());
        let input = &data[inflate.total_in() as usize..];
        let output = &mut out[inflate.total_out() as usize..];
        let status = inflate
            .decompress(input, output, flate2::FlushDecompress::None)
            .map_err(|_| CommandError::Inflate("Z_DATA_ERROR"))?;
        let done = inflate.total_out() as usize == want || status == flate2::Status::StreamEnd;
        if done || before == (inflate.total_in(), inflate.total_out()) {
            break;
        }
    }
    out.truncate(inflate.total_out() as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut data = PNG_SIGNATURE.to_vec();
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&[8, 6, 0, 0, 0]);
        data
    }

    #[test]
    fn a_png_header_gives_the_size_or_kittys_error() {
        assert_eq!(png_size(&png_header(3, 5)), Ok((3, 5)));
        assert_eq!(png_size(&[b'a'; 20]), Err(CommandError::BadPng("Not a PNG file")));
        assert_eq!(png_size(&png_header(3, 5)[..20]), Err(CommandError::BadPng(PNG_TRUNCATED)));
        assert_eq!(png_size(&png_header(0, 5)), Err(CommandError::BadPng("Invalid IHDR data")));
        assert_eq!(png_size(&png_header(10_001, 5)), Err(CommandError::PngTooLarge));
    }

    #[test]
    fn a_broken_zlib_stream_is_found_from_its_head() {
        assert_eq!(inflate_head(b"not zlib", 1), Err(CommandError::Inflate("Z_DATA_ERROR")));
        let mut deflate = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut deflate, &[7; 100]).unwrap();
        assert_eq!(inflate_head(&deflate.finish().unwrap(), 4), Ok(vec![7; 4]));
    }

    #[cfg(not(windows))]
    #[test]
    fn kittys_protected_paths_are_refused_before_opening() {
        for path in ["/proc/self/cmdline", "/dev/null", "/dev", "relative"] {
            assert_eq!(image_path(path.as_bytes()), None, "{path}");
        }
        let file = std::env::temp_dir().join("alacritree-graphics-path-policy");
        std::fs::write(&file, b"x").unwrap();
        assert!(image_path(file.to_str().unwrap().as_bytes()).is_some());
        std::fs::remove_file(file).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn only_drive_paths_are_read_on_windows() {
        for path in [r"\\server\share\x.png", r"\\?\UNC\server\share\x", r"\\.\pipe\x", "/tmp/x"] {
            assert_eq!(image_path(path.as_bytes()), None, "{path}");
        }
        assert!(image_path(br"C:\Users\x.png").is_some());
    }
}
