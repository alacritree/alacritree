//! A terminal to drive graphics commands through: bytes go through the
//! patched `vte::ansi::Processor` into a real `alacritty_terminal::Term`,
//! and the replies come back as the `PtyWrite` events a session would write
//! to its PTY.

#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use alacritree_graphics::frame::{ImageFrame, ImageQuad};
use alacritree_graphics::{Graphics, Viewport};
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const DECODE_TIMEOUT: Duration = Duration::from_secs(20);

struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone)]
pub struct Events(mpsc::Sender<Event>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

/// Counts the waker's calls, which pool threads make.
#[derive(Clone, Default)]
pub struct Wakes(Arc<(Mutex<usize>, Condvar)>);

impl Wakes {
    pub fn count(&self) -> usize {
        *self.0.0.lock().unwrap()
    }

    /// Block until the waker has run `count` times in all.
    pub fn wait_for(&self, count: usize) {
        let (lock, condvar) = &*self.0;
        let (woken, timeout) = condvar
            .wait_timeout_while(lock.lock().unwrap(), DECODE_TIMEOUT, |woken| *woken < count)
            .unwrap();
        assert!(!timeout.timed_out(), "the waker ran {} of {count} times", *woken);
    }

    fn wake(&self) {
        *self.0.0.lock().unwrap() += 1;
        self.0.1.notify_all();
    }
}

pub struct Pane {
    pub term: Term<Events>,
    parser: Processor<StdSyncHandler>,
    events: mpsc::Receiver<Event>,
    cell: (u32, u32),
    pub wakes: Wakes,
}

impl Pane {
    /// kitty's test screen: five lines of scrollback and 10x20 pixel cells.
    pub fn new(columns: usize, lines: usize) -> Self {
        Self::with(columns, lines, 5, (10, 20))
    }

    pub fn with(columns: usize, lines: usize, history: usize, cell: (u32, u32)) -> Self {
        let (sender, events) = mpsc::channel();
        let config = Config { scrolling_history: history, ..Config::default() };
        let mut term = Term::new(config, &Size { columns, lines }, Events(sender));
        let wakes = Wakes::default();
        let graphics = term.graphics_mut();
        graphics.set_cell_pixels(cell.0, cell.1);
        let waker = wakes.clone();
        graphics.set_waker(move || waker.wake());
        Self { term, parser: Processor::new(), events, cell, wakes }
    }

    pub fn graphics(&self) -> &Graphics {
        self.term.graphics()
    }

    pub fn feed(&mut self, bytes: impl AsRef<[u8]>) {
        self.parser.advance(&mut self.term, bytes.as_ref());
    }

    /// Send one graphics command, as kitty's tests' `send_command` does, and
    /// return its reply.
    pub fn send(&mut self, control: &str, payload: impl AsRef<[u8]>) -> Option<String> {
        self.feed(command(control, payload));
        let mut replies = self.replies();
        assert!(replies.len() <= 1, "more than one reply: {replies:?}");
        replies.pop()
    }

    /// The text of a reply between its `;` and the string terminator, as
    /// kitty's tests' `parse_response` reads it.
    pub fn message(&mut self, control: &str, payload: impl AsRef<[u8]>) -> Option<String> {
        self.send(control, payload).map(|reply| message(&reply).to_owned())
    }

    /// Everything the terminal wrote back since the last call.
    pub fn replies(&mut self) -> Vec<String> {
        let cell = self.cell;
        let (columns, lines) = (self.term.columns(), self.term.screen_lines());
        self.events
            .try_iter()
            .filter_map(|event| match event {
                Event::PtyWrite(text) => Some(text),
                Event::TextAreaSizeRequest(format) => Some(format(WindowSize {
                    num_lines: lines as u16,
                    num_cols: columns as u16,
                    cell_width: cell.0 as u16,
                    cell_height: cell.1 as u16,
                })),
                _ => None,
            })
            .collect()
    }

    /// The cursor as kitty's tests read it: `(x, y)`.
    pub fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.column.0, point.line.0 as usize)
    }

    /// Wait for every decode in flight on the active screen.
    pub fn settle(&self) {
        let started = Instant::now();
        while self.graphics().is_decoding() {
            assert!(started.elapsed() < DECODE_TIMEOUT, "a decode never finished");
            std::thread::yield_now();
        }
    }

    pub fn image_count(&self) -> usize {
        self.graphics().image_count()
    }

    /// The frame for the screen at the bottom of its history.
    pub fn frame(&mut self) -> ImageFrame {
        self.frame_at(0)
    }

    pub fn frame_at(&mut self, display_offset: usize) -> ImageFrame {
        self.settle();
        let viewport = Viewport {
            display_offset,
            rows: self.term.screen_lines(),
            columns: self.term.columns(),
        };
        let mut frame = ImageFrame::default();
        self.term.graphics_mut().build_frame(&mut frame, viewport);
        frame
    }

    /// The quads of the current frame, in draw order.
    pub fn quads(&mut self) -> Vec<ImageQuad> {
        self.frame().quads().to_vec()
    }

    /// The decoded pixels of image `id`.
    pub fn pixels(&self, id: u32) -> Vec<u8> {
        self.settle();
        self.graphics()
            .pixels(id)
            .unwrap_or_else(|| panic!("image {id} has no pixels"))
            .rgba()
            .to_vec()
    }
}

/// `ESC _ G control ; base64 ESC \`, with no `;` when there is no payload.
pub fn command(control: &str, payload: impl AsRef<[u8]>) -> String {
    let payload = payload.as_ref();
    if payload.is_empty() {
        format!("\x1b_G{control}\x1b\\")
    } else {
        format!("\x1b_G{control};{}\x1b\\", STANDARD.encode(payload))
    }
}

pub fn message(reply: &str) -> &str {
    let text = reply.split_once(';').expect("a reply has a ;").1;
    text.split_once('\x1b').expect("a reply ends in ST").0
}

/// A reply's code and identifiers, as kitty's tests'
/// `parse_response_with_ids` reads them: `("OK", "i=1,I=2")`.
pub fn code_and_ids(reply: &str) -> (String, String) {
    let (ids, text) = reply.split_once(';').expect("a reply has a ;");
    let code = text.split_once('\x1b').unwrap().0.split(':').next().unwrap();
    (code.to_owned(), ids.split_once('G').unwrap().1.to_owned())
}

/// kitty's tests' `byte_block`: `size` bytes counting up and wrapping.
pub fn byte_block(size: usize) -> Vec<u8> {
    (0..size).map(|index| index as u8).collect()
}

/// RGB pixels as RGBA, opaque.
pub fn opaque(rgb: &[u8]) -> Vec<u8> {
    rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 0xFF]).collect()
}

pub fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// A PNG of `data` in `color`, 8 bits per sample.
pub fn png(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
    png_with_palette(width, height, color, data, None)
}

pub fn png_with_palette(
    width: u32,
    height: u32,
    color: png::ColorType,
    data: &[u8],
    palette: Option<&[u8]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    if let Some(palette) = palette {
        encoder.set_palette(palette.to_vec());
    }
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(data).unwrap();
    writer.finish().unwrap();
    out
}

/// `(left, top, right, bottom)` of a quad's destination, in cells.
pub fn dest(quad: &ImageQuad) -> [f32; 4] {
    quad.dest
}

pub fn assert_rect(actual: [f32; 4], expected: [f32; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-4, "{actual:?} != {expected:?}");
    }
}

/// A file in the temporary directory, removed when dropped.
pub struct TempFile(pub std::path::PathBuf);

impl TempFile {
    pub fn new(name: &str, data: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::write(&path, data).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
