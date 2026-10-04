//! The kitty graphics protocol and sixel images, from an APC payload or a
//! sixel string to the list of images a frame draws.
//!
//! [`frame`] is the contract with the renderer. Nothing in this crate knows
//! about GL or about `alacritty_terminal`, which calls in with plain values.
//! kitty's implementation is the reference, except that pixels are decoded
//! on the job pool rather than under the terminal lock.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};

use crate::command::{Action, Command, Medium};
use crate::decode::{Decode, Slot};
use crate::frame::{Band, ImageFrame, Pixels};
use crate::load::{Load, Target};
use crate::placeholder::Placeholders;
use crate::placement::CellSize;
use crate::reply::{CommandError, ReplyTo, reply};
use crate::store::{Store, Visible};

mod command;
mod decode;
pub mod frame;
mod load;
pub mod placeholder;
mod placement;
mod reply;
mod sixel;
mod store;

/// kitty's `MAX_IMAGE_DIMENSION`, in pixels per side.
pub(crate) const MAX_DIMENSION: u32 = 10_000;

/// kitty's own client sends chunks without padding.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Called from a pool thread when a decode finishes.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// The terminal state a command reads.
#[derive(Clone, Copy, Debug)]
pub struct ApcContext {
    /// The cursor's screen line.
    pub line: usize,
    /// The cursor's column.
    pub column: usize,
    /// Scrollback capacity of the active screen in lines, 0 on the alternate
    /// screen.
    pub history: usize,
}

/// What the terminal does after a command.
#[derive(Debug, Default)]
pub struct ApcOutcome {
    /// Bytes to write back to the program, a whole APC string.
    pub reply: Option<String>,
    /// How far to move the cursor past a placement.
    pub cursor: Option<CursorMove>,
}

/// kitty's cursor movement after a placement: right by `columns`, then down
/// by `rows`. The terminal wraps a column past the edge onto the next line
/// and scrolls at the bottom margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorMove {
    pub columns: u32,
    pub rows: u32,
}

/// A grid scroll, in screen lines.
#[derive(Clone, Copy, Debug)]
pub struct ScrollRegion {
    /// First line of the scroll region.
    pub top: usize,
    /// Line after the last one of the scroll region.
    pub bottom: usize,
    pub screen_lines: usize,
    /// Scrollback capacity of the active screen in lines, 0 on the alternate
    /// screen.
    pub history: usize,
}

/// The part of the grid a frame shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// Lines the view is scrolled back into history.
    pub display_offset: usize,
    pub rows: usize,
    pub columns: usize,
}

/// A terminal's images: one store per screen buffer, the transmission in
/// progress, and the frame builder.
pub struct Graphics {
    main: Store,
    alt: Store,
    alt_active: bool,
    load: Load,
    /// The decoded payload of the command being handled.
    payload: Vec<u8>,
    cell: CellSize,
    waker: Option<Waker>,
    /// Decodes finished, shared with the jobs that finish them.
    ready: Arc<AtomicU64>,
    /// Moves when the active screen or the cell size changes.
    epoch: u64,
    visible: Vec<Visible>,
    /// The layout generation, viewport and placeholder generation the last
    /// frame was built for.
    built: Option<(u64, Viewport, u64)>,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            main: Store::default(),
            alt: Store::default(),
            alt_active: false,
            load: Load::default(),
            payload: Vec::new(),
            cell: CellSize::new(1, 1),
            waker: None,
            ready: Arc::default(),
            epoch: 0,
            visible: Vec::new(),
            built: None,
        }
    }
}

impl Graphics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the size of one cell in device pixels, which sizes placements and
    /// their offsets.
    pub fn set_cell_pixels(&mut self, width: u32, height: u32) {
        let cell = CellSize::new(width, height);
        if cell == self.cell {
            return;
        }
        self.cell = cell;
        self.main.rescale(cell);
        self.alt.rescale(cell);
        self.epoch += 1;
    }

    /// Set what runs, on a pool thread, when an image finishes decoding and
    /// the pane should draw it.
    pub fn set_waker(&mut self, waker: impl Fn() + Send + Sync + 'static) {
        self.waker = Some(Arc::new(waker));
    }

    /// Set where the files a client names are, for a client that sees
    /// another filesystem than this process does. `map` turns a path as the
    /// client spells it into the path opened here, once the spelling has
    /// passed kitty's checks.
    pub fn set_client_paths(&mut self, map: impl Fn(&str) -> PathBuf + Send + Sync + 'static) {
        self.load.client_paths = Some(Box::new(map));
    }

    /// Set the decoded bytes each screen buffer may hold.
    pub fn set_storage_limit(&mut self, bytes: usize) {
        self.main.quota = bytes;
        self.alt.quota = bytes;
    }

    /// Changes whenever the next [`Graphics::build_frame`] may differ from
    /// the last for the same viewport and cell size, a decode finishing
    /// included.
    pub fn layout_generation(&self) -> u64 {
        self.main
            .generation
            .wrapping_add(self.alt.generation)
            .wrapping_add(self.epoch)
            .wrapping_add(self.ready.load(Ordering::Acquire))
    }

    /// Images the active screen holds.
    pub fn image_count(&self) -> usize {
        self.active().image_count()
    }

    /// Placements on the active screen, drawn or not.
    pub fn placement_count(&self) -> usize {
        self.active().placement_count()
    }

    /// The decoded pixels of the active screen's image with client id `id`,
    /// once its decode finished.
    pub fn pixels(&self, id: u32) -> Option<Arc<Pixels>> {
        let store = self.active();
        store.index_by_id(id).and_then(|index| store.decoded(index)).cloned()
    }

    /// Whether an image of the active screen is still decoding.
    pub fn is_decoding(&self) -> bool {
        self.active().is_decoding()
    }

    /// Rebuild `frame` when the layout, the viewport, the cell size or the
    /// placeholders changed since the last call, and say whether it did. A
    /// steady screen costs one comparison. Pass the same frame every time.
    pub fn update_frame(
        &mut self,
        frame: &mut ImageFrame,
        viewport: Viewport,
        placeholders: &Placeholders,
    ) -> bool {
        if self.built == Some((self.layout_generation(), viewport, placeholders.generation())) {
            return false;
        }
        self.build_frame(frame, viewport, placeholders);
        true
    }

    /// Fill `frame` with the active screen's decoded placements `viewport`
    /// shows and the tiles of the placeholders on it, in draw order: z, then
    /// image creation order, then placement creation order.
    pub fn build_frame(
        &mut self,
        frame: &mut ImageFrame,
        viewport: Viewport,
        placeholders: &Placeholders,
    ) {
        // Read before building, so a decode landing meanwhile rebuilds next time.
        self.built = Some((self.layout_generation(), viewport, placeholders.generation()));
        let Self { main, alt, alt_active, cell, visible, .. } = self;
        let store = if *alt_active { alt } else { main };
        frame.clear();
        visible.clear();
        store.collect(viewport, *cell, visible);
        store.collect_placeholders(placeholders, *cell, visible);
        visible.sort_unstable_by_key(|visible| visible.key);
        for visible in visible.iter() {
            frame.push(Band::of(visible.key.0), store.pixels(visible.image), visible.quad);
        }
    }

    /// Handle one APC string, everything between `ESC _` and its terminator.
    pub fn apc(&mut self, apc: &[u8], context: ApcContext) -> ApcOutcome {
        let Some(control) = apc.strip_prefix(b"G").filter(|control| !control.is_empty()) else {
            return ApcOutcome::default();
        };
        let (command, encoded) = match command::parse(control) {
            Ok(parsed) => parsed,
            Err(error) => {
                log::debug!("graphics: {error}");
                return ApcOutcome::default();
            },
        };
        self.payload.clear();
        if let Err(error) = BASE64.decode_vec(encoded, &mut self.payload) {
            log::debug!("graphics: invalid base64 payload: {error}");
            self.payload.clear();
            return ApcOutcome::default();
        }
        self.active_mut().set_history(context.history);

        if command.id != 0 && command.number != 0 {
            let error = Err(CommandError::IdAndNumber);
            return ApcOutcome { reply: reply(reply_to(&command), &error), cursor: None };
        }
        match command.action.unwrap_or(Action::Transmit) {
            Action::Transmit | Action::TransmitAndPut | Action::Query => {
                self.transmit(command, context)
            },
            Action::Put => self.put(command, context),
            Action::Delete => {
                self.load.abort();
                self.active_mut().delete(&command, (context.line, context.column));
                ApcOutcome::default()
            },
            Action::Animation => {
                log::debug!("graphics: animation commands are not supported");
                ApcOutcome::default()
            },
        }
    }

    /// Handle a sixel image, the data of `DCS P1;P2;P3 q ... ST` with its
    /// params, placed at the context's cursor and clipped to `max_rows`.
    /// Returns the rows it covers, 0 when it draws nothing.
    pub fn sixel(
        &mut self,
        params: [u16; 3],
        data: &[u8],
        context: ApcContext,
        max_rows: Option<usize>,
    ) -> u32 {
        let Some(geometry) = sixel::measure(params, data) else {
            return 0;
        };
        let (ready, waker, cell) = (Arc::clone(&self.ready), self.waker.clone(), self.cell);
        let store = self.active_mut();
        store.set_history(context.history);
        let size = (geometry.width, geometry.height);
        if !store.fits(size.0, size.1) {
            log::debug!("graphics: a {}x{} sixel image is over the quota", size.0, size.1);
            return 0;
        }
        let slot = Arc::new(Slot::default());
        let data = data.to_vec();
        let decode = move || Ok::<_, Infallible>(sixel::decode(&data, geometry));
        let job = decode::spawn(decode, Arc::clone(&slot), ready, waker);
        let cursor = (context.line, context.column);
        let (internal_id, rows) = store.add_sixel(size, (slot, job), cursor, max_rows, cell);
        store.apply_quota(Some(internal_id));
        rows
    }

    /// Scroll the images of the active screen with the grid: `delta` rows,
    /// negative for up. Insert and delete line must not call this, since
    /// they do not move images.
    #[inline]
    pub fn scroll(&mut self, region: ScrollRegion, delta: i32) {
        let cell = self.cell;
        let store = self.active_mut();
        if !store.is_empty() {
            store.scroll(&region, delta, cell);
        }
    }

    /// Text was printed into screen cell `(line, column)`. Sixel images stop
    /// drawing that cell, as in WezTerm; kitty placements stay, as in kitty.
    #[inline]
    pub fn print(&mut self, line: usize, column: usize) {
        let cell = self.cell;
        let store = self.active_mut();
        if store.has_sixels() {
            store.print((line, column), cell);
        }
    }

    /// Erase in display (ED 2): remove placements with a row on screen,
    /// keeping those wholly in scrollback, and images left unplaced.
    pub fn clear_screen(&mut self) {
        self.active_mut().clear(false);
    }

    /// Erase saved lines (ED 3): remove every placement of the active screen
    /// and images left unplaced, as kitty does.
    pub fn clear_all(&mut self) {
        self.active_mut().clear(true);
    }

    /// The alternate screen became active and was cleared.
    pub fn enter_alt_screen(&mut self) {
        self.alt.clear(true);
        self.alt_active = true;
        self.epoch += 1;
    }

    /// The main screen became active again.
    pub fn leave_alt_screen(&mut self) {
        self.alt_active = false;
        self.epoch += 1;
    }

    /// A full reset (RIS): back on the main screen, with its placements in
    /// scrollback kept and everything on the alternate screen removed.
    pub fn reset(&mut self) {
        self.alt_active = false;
        self.main.clear(false);
        self.alt.clear(true);
        self.epoch += 1;
    }

    /// The grid was resized. When the column count held, each screen's
    /// content moved by `main_shift` and `alt_shift` rows, which its images
    /// follow. A column change reflows text and leaves images where they
    /// were, as kitty's `grman_resize` does.
    pub fn resize(&mut self, columns_changed: bool, main_shift: i32, alt_shift: i32) {
        if !columns_changed {
            self.main.shift(main_shift);
            self.alt.shift(alt_shift);
        }
        self.epoch += 1;
    }

    fn active(&self) -> &Store {
        if self.alt_active { &self.alt } else { &self.main }
    }

    fn active_mut(&mut self) -> &mut Store {
        if self.alt_active { &mut self.alt } else { &mut self.main }
    }

    /// `a=t`, `a=T`, `a=q` and every chunk after the first: the transmit arm
    /// of kitty's `grman_handle_command`.
    fn transmit(&mut self, command: Command, context: ApcContext) -> ApcOutcome {
        let query = command.action == Some(Action::Query);
        if query && command.id == 0 {
            log::debug!("graphics: query without an image id");
            return ApcOutcome::default();
        }
        let continues =
            command.medium.unwrap_or_default() == Medium::Direct && self.load.target().is_some();
        let result =
            if continues { self.continue_load(&command) } else { self.begin_load(command) };

        if command.quiet != 0 {
            self.load.start.quiet = command.quiet;
        }
        let start = self.load.start;
        let to = if query {
            ReplyTo { id: command.id, quiet: command.quiet, ..ReplyTo::default() }
        } else {
            reply_to(&start)
        };
        let finished = result.clone().map(|target| target.is_some());

        let added = match result {
            Ok(Some(Target::Image(internal_id))) => Some(internal_id),
            _ => None,
        };
        let mut cursor = None;
        if let Some(internal_id) = added.filter(|_| start.action == Some(Action::TransmitAndPut)) {
            let store = self.active();
            if let Some(index) = store.index_by_internal(internal_id) {
                cursor = self.place_at(index, &start, context);
            }
        }
        self.active_mut().apply_quota(added);
        ApcOutcome { reply: reply(to, &finished), cursor }
    }

    /// Start a transmission from its first chunk. Returns what it filled
    /// once complete, `None` while chunks are still to come.
    fn begin_load(&mut self, mut command: Command) -> Result<Option<Target>, CommandError> {
        self.load.abort();
        self.load.start = command;
        if command.data_width > MAX_DIMENSION || command.data_height > MAX_DIMENSION {
            return Err(CommandError::TooLarge);
        }
        let store = self.active_mut();
        store.trim_for_transmission();
        let target = if command.action == Some(Action::Query) {
            Target::Query(command.id)
        } else {
            let (internal_id, id) =
                store.begin_image(command.id, command.number, command.usage & 1 != 0);
            command.id = id;
            Target::Image(internal_id)
        };
        self.load.begin(command, target)?;
        self.load_data(command.medium.unwrap_or_default(), command.more != 0)
    }

    fn continue_load(&mut self, command: &Command) -> Result<Option<Target>, CommandError> {
        self.load.start.more = command.more;
        if let Some(Target::Image(internal_id)) = self.load.target() {
            if self.active().index_by_internal(internal_id).is_none() {
                self.load.abort();
                return Err(CommandError::OrphanChunk);
            }
        }
        self.load_data(Medium::Direct, command.more != 0)
    }

    fn load_data(&mut self, medium: Medium, more: bool) -> Result<Option<Target>, CommandError> {
        let target = self.load.target().expect("a transmission in progress");
        let decode = match medium {
            Medium::Direct => {
                if !self.load.append(&self.payload, more)? {
                    return Ok(None);
                }
                self.load.finish()?
            },
            _ => self.load.open_file(&self.payload)?,
        };
        self.commit(target, decode)?;
        Ok(Some(target))
    }

    /// Store finished data in its image and start decoding it.
    fn commit(&mut self, target: Target, decode: Decode) -> Result<(), CommandError> {
        let Target::Image(internal_id) = target else {
            return Ok(());
        };
        let (ready, waker) = (Arc::clone(&self.ready), self.waker.clone());
        let store = self.active_mut();
        let Some(index) = store.index_by_internal(internal_id) else {
            return Err(CommandError::OrphanChunk);
        };
        let size = (decode.width, decode.height);
        if !store.fits(size.0, size.1) {
            store.remove_at(index);
            return Err(CommandError::OverQuota);
        }
        let slot = Arc::new(Slot::default());
        let job = decode.spawn(Arc::clone(&slot), ready, waker);
        store.commit(index, size, slot, job);
        Ok(())
    }

    /// `a=p`: kitty's `handle_put_command` and its reply.
    fn put(&mut self, command: Command, context: ApcContext) -> ApcOutcome {
        if command.id == 0 && command.number == 0 {
            log::debug!("graphics: put without an image id or number");
            return ApcOutcome::default();
        }
        let store = self.active();
        let index = match command.id {
            0 => store.index_by_number(command.number),
            id => store.index_by_id(id),
        };
        let (id, result) = match index {
            _ if command.unicode != 0 && command.parent != 0 => {
                (command.id, Err(CommandError::VirtualWithParent))
            },
            None => {
                let error = CommandError::NoImage { id: command.id, number: command.number };
                (command.id, Err(error))
            },
            Some(index) if !store.has_data(index) => {
                (store.client_id(index), Err(CommandError::NoData(command.id)))
            },
            Some(_) if command.parent != 0 => (command.id, Err(CommandError::Relative)),
            Some(index) => (store.client_id(index), Ok(self.place_at(index, &command, context))),
        };
        let cursor = result.clone().ok().flatten();
        let to = ReplyTo { id, ..reply_to(&command) };
        ApcOutcome { reply: reply(to, &result.map(|_| true)), cursor }
    }

    fn place_at(
        &mut self,
        index: usize,
        command: &Command,
        context: ApcContext,
    ) -> Option<CursorMove> {
        if command.parent != 0 {
            return None;
        }
        let cell = self.cell;
        self.active_mut().put(index, command, (context.line, context.column), cell)
    }
}

/// The reply to XTSMGRAPHICS, `CSI ? item ; action ; ... S`, given the
/// text area's size in pixels. The register count and the largest image
/// are fixed, so setting or resetting either reports what stays in force.
pub fn graphics_attribute_reply(item: u16, action: u16, text_area: (u32, u32)) -> String {
    const INVALID_ITEM: u8 = 1;
    const INVALID_ACTION: u8 = 2;
    const READ_MAX: u16 = 4;
    match (item, action) {
        (1 | 2, 0 | 5..) => format!("\x1b[?{item};{INVALID_ACTION}S"),
        (1, _) => format!("\x1b[?1;0;{}S", sixel::REGISTERS),
        (2, READ_MAX) => format!("\x1b[?2;0;{MAX_DIMENSION};{MAX_DIMENSION}S"),
        (2, _) => {
            let (width, height) = (text_area.0.min(MAX_DIMENSION), text_area.1.min(MAX_DIMENSION));
            format!("\x1b[?2;0;{width};{height}S")
        },
        _ => format!("\x1b[?{item};{INVALID_ITEM}S"),
    }
}

fn reply_to(command: &Command) -> ReplyTo {
    ReplyTo {
        id: command.id,
        number: command.number,
        placement: command.placement,
        quiet: command.quiet,
    }
}
