//! The session backend that runs each terminal in the native side's
//! herdr-core core, over one `hc connect` child in framed mode.
//!
//! Terminals start with `keep` and outlive alacritree. Closing a session
//! kills its terminal, quitting only detaches, and the next launch adopts
//! each terminal that the `labels` module marks as alacritree's. A demux
//! thread routes each stream into a parser thread per session. When the
//! connection drops, each parser reattaches its session on a new one.

mod connection;
mod labels;
mod parser;
mod settings;
mod wire;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use alacritree_process_probe::ProbeHandle;
use alacritree_session_backend::{
    Adoptable, AttachRequest, ForegroundProgram, OpenRequest, Operation, Placement, Screen,
    SessionBackend, SessionBackendKind, SessionError, SessionHandle, Side, Signals, TerminalColors,
};
use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::vte::ansi::Rgb;
use serde_json::{Value, json};

use self::connection::{Answer, Connection, Facts, Waker};
use self::parser::Chunk;
pub use self::settings::{HerdrCoreConfig, RawHerdrCore, SessionBackendChoice};

/// The glyph that marks a herdr-core pane: herdr's ram's head from the
/// bundled symbol font, since herdr-core is herdr's terminal core.
pub const DEFAULT_ICON: &str = "\u{10FF00}";

/// Why the core could not be reached, or refused a call.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("could not start `{program} connect`: {source}")]
    Spawn { program: String, source: std::io::Error },
    #[error("`hc connect` exited before it answered: {0}")]
    Refused(String),
    #[error("the core's answer was not herdr-core's protocol: {0}")]
    Protocol(String),
    #[error("the connection to the core closed")]
    Closed,
    #[error("the core did not answer {0} in time")]
    Timeout(String),
    #[error("the core refused the call ({kind}): {message}")]
    Rpc { kind: String, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl CoreError {
    fn rpc(error: &Value) -> Self {
        Self::Rpc {
            kind: error["data"]["kind"].as_str().unwrap_or("unknown").to_string(),
            message: error["message"].as_str().unwrap_or_default().to_string(),
        }
    }

    fn unavailable(self) -> SessionError {
        SessionError::Unavailable { backend: SessionBackendKind::HerdrCore, source: Box::new(self) }
    }
}

impl From<CoreError> for SessionError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Io(error) => SessionError::Io(error),
            error => SessionError::Io(std::io::Error::other(error)),
        }
    }
}

/// Runs terminals in the native side's core. Clones share one connection,
/// which opens on first use and again after it drops.
#[derive(Clone)]
pub struct HerdrCore {
    program: Arc<str>,
    connection: Arc<Mutex<Slot>>,
    claims: Arc<Mutex<Claims>>,
    waker: Waker,
}

/// The connection that clones share, and when the last attempt to open
/// one failed.
#[derive(Default)]
struct Slot {
    connection: Option<Arc<Connection>>,
    failed_at: Option<Instant>,
}

impl Slot {
    fn live(&self) -> Option<Arc<Connection>> {
        self.connection.as_ref().filter(|connection| connection.alive()).cloned()
    }
}

/// The terminals this process holds, so none is adopted twice.
#[derive(Default)]
struct Claims {
    /// Every terminal a session opened, or that was handed out to adopt.
    held: HashSet<String>,
    /// Opens whose terminal the core may already report but whose address
    /// is not in `held` yet. Adoption waits while any is in flight.
    opening: usize,
}

impl HerdrCore {
    /// `program` is the `hc` binary, as `[integrations.herdr_core] path`
    /// names it.
    pub fn new(program: &str) -> Self {
        Self {
            program: program.into(),
            connection: Default::default(),
            claims: Default::default(),
            waker: Default::default(),
        }
    }

    /// Call `wake` whenever the core reports a terminal that may be
    /// adoptable, so a terminal an agent starts shows up in an idle app.
    /// Only the first call counts.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.waker.set(Box::new(wake));
    }

    /// The live connection, opened first if there is none. Holding the slot
    /// while it opens keeps concurrent opens on one connection.
    fn connection(&self) -> Result<Arc<Connection>, CoreError> {
        self.open_in(&mut lock(&self.connection))
    }

    /// The live connection, opened first unless an attempt failed within
    /// the last [`RECONNECT_INTERVAL`]. Every session that waits out a
    /// dropped connection asks here, so they share one attempt per interval.
    fn reconnect(&self) -> Option<Arc<Connection>> {
        let mut slot = lock(&self.connection);
        if slot.live().is_none()
            && slot.failed_at.is_some_and(|at| at.elapsed() < RECONNECT_INTERVAL)
        {
            return None;
        }
        self.open_in(&mut slot)
            .inspect_err(|error| log::debug!("herdr-core reconnect: {error}"))
            .ok()
    }

    fn open_in(&self, slot: &mut Slot) -> Result<Arc<Connection>, CoreError> {
        if let Some(connection) = slot.live() {
            return Ok(connection);
        }
        let opened = Connection::open(&self.program, self.waker.clone());
        slot.failed_at = opened.is_err().then(Instant::now);
        let connection = opened?;
        slot.connection = Some(connection.clone());
        Ok(connection)
    }

    /// The live connection, if one is open. Never waits, so the UI thread
    /// can ask while another thread holds the slot to open a connection,
    /// which can take seconds.
    fn open_connection(&self) -> Option<Arc<Connection>> {
        let slot = match self.connection.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        slot.live()
    }

    /// Kill the `hc connect` child, as a crash would.
    #[cfg(feature = "test-support")]
    pub fn kill_connection(&self) {
        if let Some(connection) = lock(&self.connection).connection.as_ref() {
            connection.kill();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a viewer's own terminal looks like, which the core answers the
/// program's display queries from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayFacts {
    colors: TerminalColors,
    cell: (u16, u16),
}

impl DisplayFacts {
    fn new(colors: TerminalColors, size: WindowSize) -> Self {
        Self { colors, cell: (size.cell_width, size.cell_height) }
    }

    fn json(&self) -> Value {
        let hex = |Rgb { r, g, b }: Rgb| format!("#{r:02x}{g:02x}{b:02x}");
        json!({
            "fg": hex(self.colors.foreground),
            "bg": hex(self.colors.background),
            "cursor": self.colors.cursor.map(hex),
            "cell": [self.cell.0, self.cell.1],
        })
    }
}

fn ensure_working_directory(dir: Option<&Path>) -> Result<(), SessionError> {
    match dir {
        Some(d) if !d.is_dir() => Err(SessionError::MissingDirectory(d.to_path_buf())),
        _ => Ok(()),
    }
}

/// The terminal identity a PTY child inherits from alacritree's own
/// environment, which a core terminal starts without.
const IDENTITY_ENV: [&str; 2] = ["TERM", "COLORTERM"];

impl<H: From<HerdrCoreHandle>> SessionBackend<H> for HerdrCore {
    fn ready(&self) -> Result<(), SessionError> {
        self.connection().map(drop).map_err(CoreError::unavailable)
    }

    fn open<L>(&self, request: OpenRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let OpenRequest { shell, working_directory, mut env, colors, placement, screen, .. } =
            request;
        ensure_working_directory(working_directory.as_deref())?;
        let connection = self.connection().map_err(CoreError::unavailable)?;

        for name in IDENTITY_ENV {
            if let Ok(value) = std::env::var(name) {
                env.entry(name.to_string()).or_insert(value);
            }
        }
        let argv: Vec<String> = shell
            .map(|shell| std::iter::once(shell.program).chain(shell.args).collect())
            .unwrap_or_default();
        let window_size = screen.window_size;
        let display = DisplayFacts::new(colors, window_size);
        lock(&self.claims).opening += 1;
        let run = connection.call(
            "terminal.run",
            json!({
                "argv": argv,
                "cwd": working_directory,
                "env": env,
                "size": { "cols": window_size.num_cols, "rows": window_size.num_lines },
                "display": display.json(),
                "no_parent": true,
                "no_notify": true,
                "keep": true,
                "labels": labels::for_run(&placement),
            }),
        );
        let (address, shell_pid) = {
            let mut claims = lock(&self.claims);
            claims.opening -= 1;
            let record = run?;
            let address = record["address"].as_str().unwrap_or_default().to_string();
            claims.held.insert(address.clone());
            (address, record["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()))
        };

        let stream = Stream {
            address: address.clone(),
            shell_pid,
            display,
            first_paint_history: 0,
            placement,
        };
        stream.attach(self, connection, screen).map(H::from).inspect_err(|_| {
            end(self.clone(), address);
        })
    }

    fn adoptable(&self) -> Vec<Adoptable> {
        let Some(connection) = self.open_connection() else { return Vec::new() };
        let mut claims = lock(&self.claims);
        if claims.opening > 0 {
            return Vec::new();
        }
        let found = connection.pick_terminals(|address, facts| {
            let placement = labels::placement(&facts.labels)?;
            (facts.live && !claims.held.contains(address))
                .then(|| Adoptable { terminal: address.to_string(), placement })
        });
        claims.held.extend(found.iter().map(|adoptable| adoptable.terminal.clone()));
        found
    }

    fn attach<L>(&self, request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let AttachRequest { terminal, colors, screen } = request;
        let connection = self.connection().map_err(CoreError::unavailable)?;
        let facts = connection.facts(&terminal).unwrap_or_default();
        let stream = Stream {
            address: terminal,
            shell_pid: facts.pid,
            display: DisplayFacts::new(colors, screen.window_size),
            first_paint_history: screen.history,
            placement: labels::placement(&facts.labels).unwrap_or_default(),
        };
        stream.attach(self, connection, screen).map(H::from)
    }

    fn watch<L>(&self, _request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        Err(unsupported(Operation::Watch))
    }
}

/// A terminal in the core, about to be shown in a session.
struct Stream {
    address: String,
    shell_pid: Option<u32>,
    display: DisplayFacts,
    /// Scrollback lines the first paint restores. A new terminal has none,
    /// while a reattach restores what the grid keeps.
    first_paint_history: usize,
    /// What the terminal's labels record.
    placement: Placement,
}

impl Stream {
    /// Attach to the terminal and feed `screen` from it. A failure ends
    /// nothing, and the caller decides whether the terminal lives on.
    fn attach<L>(
        self,
        core: &HerdrCore,
        connection: Arc<Connection>,
        Screen { term, listener, window_size, history }: Screen<L>,
    ) -> Result<HerdrCoreHandle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let size = (window_size.num_cols, window_size.num_lines);
        let opened =
            open_stream(&connection, &self.address, size, &self.display, self.first_paint_history)?;
        let link = Link {
            connection,
            channel: opened.channel,
            automatic_input: opened.automatic_input,
            size,
            display: self.display,
            focused: None,
            placement: self.placement,
            closed: false,
        };
        let attached = Arc::new(Attached {
            core: core.clone(),
            address: self.address,
            history,
            link: Mutex::new(link),
        });
        if let Err(error) = parser::spawn(term, listener, opened.stream, attached.clone()) {
            attached.link().send(wire::CLOSE, &[]);
            return Err(error.into());
        }

        Ok(HerdrCoreHandle {
            attached,
            probe: ProbeHandle::new(self.shell_pid, None),
            killed: false,
        })
    }
}

/// A stream that `terminal.attach` opened.
struct Opened {
    channel: u32,
    stream: Receiver<Chunk>,
    automatic_input: bool,
}

/// Attach to `address` on `connection`, with a first paint that restores
/// `history` scrollback lines.
fn open_stream(
    connection: &Connection,
    address: &str,
    (cols, rows): (u16, u16),
    display: &DisplayFacts,
    history: usize,
) -> Result<Opened, CoreError> {
    let attached = connection.call_streaming(
        "terminal.attach",
        json!({
            "terminal": address,
            "cols": cols,
            "rows": rows,
            "display": display.json(),
            "history": history,
        }),
    )?;
    match attached {
        Answer { result, stream: Some((channel, stream)) } => Ok(Opened {
            channel,
            stream,
            automatic_input: connection.automatic_input() && result["automatic_input"] == true,
        }),
        Answer { result, stream: None } => Err(CoreError::Protocol(result.to_string())),
    }
}

fn unsupported(operation: Operation) -> SessionError {
    SessionError::Unsupported { backend: SessionBackendKind::HerdrCore, operation }
}

/// Kill a terminal and remove its finished record, off the caller's thread,
/// since a kill escalates through signals and can take seconds. Through a
/// dropped connection it tries again every [`RECONNECT_INTERVAL`], so a
/// closed session never leaves its terminal running.
fn end(core: HerdrCore, address: String) {
    let ended = std::thread::Builder::new().name("hc-kill".into()).spawn(move || {
        let terminal = json!({ "terminal": address });
        loop {
            if let Some(connection) = core.reconnect()
                && kill_and_remove(&connection, &terminal)
            {
                return;
            }
            std::thread::sleep(RECONNECT_INTERVAL);
        }
    });
    if let Err(error) = ended {
        log::warn!("herdr-core kill thread: {error}");
    }
}

/// Answers whether the core heard both calls. A refusal counts as heard,
/// such as a kill of a terminal that already finished.
fn kill_and_remove(connection: &Connection, terminal: &Value) -> bool {
    for method in ["terminal.kill", "terminal.rm"] {
        match connection.call(method, terminal.clone()) {
            Ok(_) => {},
            Err(error @ CoreError::Rpc { .. }) => {
                log::debug!("herdr-core {method} {terminal}: {error}")
            },
            Err(error) => {
                log::debug!("herdr-core {method} {terminal}, trying again: {error}");
                return false;
            },
        }
    }
    true
}

/// How long a session waits between attempts to reattach after its
/// connection drops.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// A session's terminal in the core, shared by its handle and its parser.
struct Attached {
    core: HerdrCore,
    address: String,
    /// Scrollback lines a reattach restores.
    history: usize,
    link: Mutex<Link>,
}

/// The stream a session rides on now, and what the core must hear again on
/// a new one. The parser replaces the stream when the connection drops.
struct Link {
    connection: Arc<Connection>,
    channel: u32,
    /// The stream takes AUTOMATIC_INPUT end to end.
    automatic_input: bool,
    size: (u16, u16),
    display: DisplayFacts,
    focused: Option<bool>,
    /// Where the session sits, which the terminal's labels record.
    placement: Placement,
    /// The session let go of the stream, by a detach, a kill or a drop.
    closed: bool,
}

impl Link {
    fn send(&self, kind: u8, payload: &[u8]) {
        self.connection.send(self.channel, kind, payload);
    }

    /// Never blocks, since nothing waits for the label call's answer.
    fn send_labels(&self, address: &str) {
        let labels = labels::of(&self.placement);
        self.connection.cast("terminal.label", json!({ "terminal": address, "labels": labels }));
    }

    fn send_size(&self) {
        let [c0, c1] = self.size.0.to_le_bytes();
        let [r0, r1] = self.size.1.to_le_bytes();
        self.send(wire::RESIZE, &[c0, c1, r0, r1]);
    }

    fn send_display(&self) {
        self.send(wire::FACTS, self.display.json().to_string().as_bytes());
    }

    fn send_focus(&self) {
        if let Some(focused) = self.focused {
            self.send(wire::FOCUS, &[u8::from(focused)]);
        }
    }
}

/// What a session's parser follows after its connection drops.
pub(crate) enum Resumed {
    /// A new stream, whose first paint restores the screen.
    Stream(Receiver<Chunk>),
    /// The core no longer has the terminal.
    Gone,
    /// The session let go of the terminal while it waited.
    Released,
}

impl Attached {
    fn link(&self) -> MutexGuard<'_, Link> {
        lock(&self.link)
    }

    /// Tell the core that `bytes` of the current stream were parsed.
    pub(crate) fn credit(&self, bytes: usize) {
        let link = self.link();
        link.connection.credit(link.channel, bytes);
    }

    /// Attach again after the connection dropped, trying every
    /// [`RECONNECT_INTERVAL`] until the core answers. Blocks, so only the
    /// parser thread calls it.
    pub(crate) fn resume(&self) -> Resumed {
        loop {
            if self.link().closed {
                return Resumed::Released;
            }
            std::thread::sleep(RECONNECT_INTERVAL);
            let Some(connection) = self.core.reconnect() else { continue };
            match self.reattach(connection) {
                Ok(resumed) => return resumed,
                // A session that closed while it waited may have killed and
                // removed the terminal itself, and must not report an exit.
                Err(CoreError::Rpc { .. } | CoreError::Protocol(_)) if self.link().closed => {
                    return Resumed::Released;
                },
                Err(error @ (CoreError::Rpc { .. } | CoreError::Protocol(_))) => {
                    log::warn!("herdr-core could not reattach {}: {error}", self.address);
                    return Resumed::Gone;
                },
                Err(error) => log::debug!("herdr-core reattach of {}: {error}", self.address),
            }
        }
    }

    /// Move the session onto a new stream on `connection`, and repeat what
    /// changed while it was being opened.
    fn reattach(&self, connection: Arc<Connection>) -> Result<Resumed, CoreError> {
        let (size, display) = {
            let link = self.link();
            (link.size, link.display.clone())
        };
        let opened = open_stream(&connection, &self.address, size, &display, self.history)?;

        let mut link = self.link();
        link.connection = connection;
        link.channel = opened.channel;
        link.automatic_input = opened.automatic_input;
        if link.closed {
            link.send(wire::CLOSE, &[]);
            return Ok(Resumed::Released);
        }
        if link.size != size {
            link.send_size();
        }
        if link.display != display {
            link.send_display();
        }
        link.send_focus();
        let recorded =
            link.connection.facts(&self.address).and_then(|f| labels::placement(&f.labels));
        if recorded.as_ref() != Some(&link.placement) {
            link.send_labels(&self.address);
        }
        Ok(Resumed::Stream(opened.stream))
    }
}

/// A session's terminal in the core, and the stream it is attached on.
pub struct HerdrCoreHandle {
    attached: Arc<Attached>,
    /// Reads the programs under the terminal's shell. The core runs on this
    /// machine, and on Windows it reports only the program it started.
    probe: ProbeHandle,
    killed: bool,
}

impl HerdrCoreHandle {
    /// The terminal's address in its core, such as `t7`.
    pub fn terminal(&self) -> &str {
        &self.attached.address
    }

    /// What the current connection last heard about the terminal.
    fn facts(&self) -> Facts {
        let connection = self.attached.link().connection.clone();
        connection.facts(&self.attached.address).unwrap_or_default()
    }

    fn close_stream(&self) {
        let mut link = self.attached.link();
        if !std::mem::replace(&mut link.closed, true) {
            link.send(wire::CLOSE, &[]);
        }
    }
}

impl SessionHandle for HerdrCoreHandle {
    fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.attached.link().send(wire::DATA, &bytes);
        }
    }

    /// Without AUTOMATIC_INPUT on the whole path a reply goes as DATA, which
    /// the core counts as typing.
    fn reply(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let link = self.attached.link();
        link.send(if link.automatic_input { wire::AUTOMATIC_INPUT } else { wire::DATA }, &bytes);
    }

    fn resize(&mut self, size: WindowSize) {
        let mut link = self.attached.link();
        let cells = (size.num_cols, size.num_lines);
        if cells != link.size {
            link.size = cells;
            link.send_size();
        }
        let display = DisplayFacts::new(link.display.colors, size);
        if display != link.display {
            link.display = display;
            link.send_display();
        }
    }

    /// The core forwards focus to a program that asked for focus reports.
    /// Nothing here holds a scheduling boost, so it always answers false.
    fn set_focused(&self, focused: bool) -> bool {
        let mut link = self.attached.link();
        if link.focused.replace(focused) != Some(focused) {
            link.send_focus();
        }
        false
    }

    fn kill(&mut self) {
        if std::mem::replace(&mut self.killed, true) {
            return;
        }
        self.close_stream();
        end(self.attached.core.clone(), self.attached.address.clone());
    }

    /// The terminal runs on in the core, where the next launch adopts it.
    fn detach(&mut self) -> Result<(), SessionError> {
        self.close_stream();
        Ok(())
    }

    /// A move while the connection is down reaches the labels on reattach.
    fn place(&mut self, placement: &Placement) {
        let mut link = self.attached.link();
        if *placement != link.placement {
            link.placement = placement.clone();
            link.send_labels(&self.attached.address);
        }
    }

    /// As fresh as the core's last foreground poll.
    fn cwd(&self) -> Option<PathBuf> {
        self.facts().foreground.map(|foreground| foreground.cwd)
    }

    fn foreground(&self) -> Option<ForegroundProgram> {
        let foreground = self.facts().foreground?;
        Some(ForegroundProgram {
            side: Side::Native,
            pid: u32::try_from(foreground.pid).ok(),
            argv: if foreground.argv.is_empty() { vec![foreground.name] } else { foreground.argv },
            cwd: Some(foreground.cwd),
        })
    }

    fn signals(&self) -> Signals {
        self.probe.signals()
    }
}

/// Dropping a handle detaches its stream and leaves the terminal running.
impl Drop for HerdrCoreHandle {
    fn drop(&mut self) {
        self.close_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_facts_spell_colors_as_the_core_reads_them() {
        let colors = TerminalColors {
            foreground: Rgb { r: 0xff, g: 0x80, b: 0 },
            background: Rgb { r: 0, g: 0, b: 0x10 },
            cursor: None,
        };
        let size = WindowSize { num_lines: 24, num_cols: 80, cell_width: 9, cell_height: 18 };

        assert_eq!(
            DisplayFacts::new(colors, size).json(),
            json!({ "fg": "#ff8000", "bg": "#000010", "cursor": null, "cell": [9, 18] })
        );
    }
}
