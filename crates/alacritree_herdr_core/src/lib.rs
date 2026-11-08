//! The session backend that runs each terminal in the native side's
//! herdr-core core, over one `hc connect` child in framed mode.
//!
//! Closing a session kills its terminal and removes the finished record. A
//! demux thread routes each stream into a parser thread per session, rather
//! than an `EventedPty` per session.

mod connection;
mod parser;
mod settings;
mod wire;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alacritree_process_probe::ProbeHandle;
use alacritree_session_backend::{
    ForegroundProgram, OpenRequest, Operation, PaneKey, Screen, SessionBackend, SessionBackendKind,
    SessionError, SessionHandle, Side, Signals, TerminalColors,
};
use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::vte::ansi::Rgb;
use serde_json::{Value, json};

use self::connection::{Answer, Connection, Facts};
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
    connection: Arc<Mutex<Option<Arc<Connection>>>>,
}

impl HerdrCore {
    /// `program` is the `hc` binary, as `[integrations.herdr_core] path`
    /// names it.
    pub fn new(program: &str) -> Self {
        Self { program: program.into(), connection: Default::default() }
    }

    /// The live connection, opened first if there is none. Holding the slot
    /// while it opens keeps concurrent opens on one connection.
    fn connection(&self) -> Result<Arc<Connection>, CoreError> {
        let mut slot = lock(&self.connection);
        if let Some(connection) = slot.as_ref().filter(|connection| connection.alive()) {
            return Ok(connection.clone());
        }
        let connection = Connection::open(&self.program)?;
        *slot = Some(connection.clone());
        Ok(connection)
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
        let OpenRequest {
            shell,
            working_directory,
            mut env,
            colors,
            screen: Screen { term, listener, window_size },
            ..
        } = request;
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
        let display = DisplayFacts::new(colors, window_size);
        let (cols, rows) = (window_size.num_cols, window_size.num_lines);
        let record = connection.call(
            "terminal.run",
            json!({
                "argv": argv,
                "cwd": working_directory,
                "env": env,
                "size": { "cols": cols, "rows": rows },
                "display": display.json(),
                "no_parent": true,
                "no_notify": true,
            }),
        )?;
        let address = record["address"].as_str().unwrap_or_default().to_string();
        let shell_pid = record["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok());

        let attached = connection.call_streaming(
            "terminal.attach",
            json!({ "terminal": address, "cols": cols, "rows": rows, "display": display.json() }),
        );
        let (answer, (channel, stream)) = match attached {
            Ok(Answer { result, stream: Some(stream) }) => (result, stream),
            Ok(Answer { result, stream: None }) => {
                end(connection, address);
                return Err(CoreError::Protocol(result.to_string()).into());
            },
            Err(error) => {
                end(connection, address);
                return Err(error.into());
            },
        };

        let credited = connection.clone();
        let credit = move |bytes| credited.credit(channel, bytes);
        if let Err(error) = parser::spawn(term, listener, stream, credit) {
            connection.send(channel, wire::CLOSE, &[]);
            end(connection, address);
            return Err(error.into());
        }

        Ok(HerdrCoreHandle {
            automatic_input: connection.automatic_input() && answer["automatic_input"] == true,
            connection,
            channel,
            address,
            probe: ProbeHandle::new(shell_pid, None),
            size: (cols, rows),
            display,
            focused: AtomicU8::new(UNKNOWN),
            killed: false,
        }
        .into())
    }

    fn adoptable(&self) -> Vec<PaneKey> {
        Vec::new()
    }

    fn attach<L>(&self, _pane: &PaneKey, _screen: Screen<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        Err(unsupported(Operation::Attach))
    }

    fn watch<L>(&self, _pane: &PaneKey, _screen: Screen<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        Err(unsupported(Operation::Watch))
    }
}

fn unsupported(operation: Operation) -> SessionError {
    SessionError::Unsupported { backend: SessionBackendKind::HerdrCore, operation }
}

/// Kill a terminal and remove its finished record, off the caller's thread,
/// since a kill escalates through signals and can take seconds.
fn end(connection: Arc<Connection>, address: String) {
    let ended = std::thread::Builder::new().name("hc-kill".into()).spawn(move || {
        let terminal = json!({ "terminal": address });
        if let Err(error) = connection.call("terminal.kill", terminal.clone()) {
            log::debug!("herdr-core kill of {address}: {error}");
        }
        if let Err(error) = connection.call("terminal.rm", terminal) {
            log::warn!("herdr-core could not remove {address}: {error}");
        }
    });
    if let Err(error) = ended {
        log::warn!("herdr-core kill thread: {error}");
    }
}

const UNKNOWN: u8 = 0;
const UNFOCUSED: u8 = 1;
const FOCUSED: u8 = 2;

/// A session's terminal in the core, and the stream it is attached on.
pub struct HerdrCoreHandle {
    connection: Arc<Connection>,
    channel: u32,
    address: String,
    /// Reads the programs under the terminal's shell. The core runs on this
    /// machine, and on Windows it reports only the program it started.
    probe: ProbeHandle,
    /// The stream takes AUTOMATIC_INPUT end to end.
    automatic_input: bool,
    size: (u16, u16),
    display: DisplayFacts,
    focused: AtomicU8,
    killed: bool,
}

impl HerdrCoreHandle {
    /// The terminal's address in its core, such as `t7`.
    pub fn terminal(&self) -> &str {
        &self.address
    }

    fn facts(&self) -> Facts {
        self.connection.facts(&self.address).unwrap_or_default()
    }
}

impl SessionHandle for HerdrCoreHandle {
    fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.connection.send(self.channel, wire::DATA, &bytes);
        }
    }

    /// Without AUTOMATIC_INPUT on the whole path a reply goes as DATA, which
    /// the core counts as typing.
    fn reply(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let kind = if self.automatic_input { wire::AUTOMATIC_INPUT } else { wire::DATA };
        self.connection.send(self.channel, kind, &bytes);
    }

    fn resize(&mut self, size: WindowSize) {
        let cells = (size.num_cols, size.num_lines);
        if cells != self.size {
            self.size = cells;
            let [c0, c1] = cells.0.to_le_bytes();
            let [r0, r1] = cells.1.to_le_bytes();
            self.connection.send(self.channel, wire::RESIZE, &[c0, c1, r0, r1]);
        }
        let display = DisplayFacts::new(self.display.colors, size);
        if display != self.display {
            self.connection.send(self.channel, wire::FACTS, display.json().to_string().as_bytes());
            self.display = display;
        }
    }

    /// The core forwards focus to a program that asked for focus reports.
    /// Nothing here holds a scheduling boost, so it always answers false.
    fn set_focused(&self, focused: bool) -> bool {
        let now = if focused { FOCUSED } else { UNFOCUSED };
        if self.focused.swap(now, Ordering::Relaxed) != now {
            self.connection.send(self.channel, wire::FOCUS, &[u8::from(focused)]);
        }
        false
    }

    fn kill(&mut self) {
        if std::mem::replace(&mut self.killed, true) {
            return;
        }
        self.connection.send(self.channel, wire::CLOSE, &[]);
        end(self.connection.clone(), self.address.clone());
    }

    /// The session opened this terminal, so it is the session's to end.
    fn detach(&mut self) -> Result<(), SessionError> {
        Err(unsupported(Operation::Detach))
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
        if !self.killed {
            self.connection.send(self.channel, wire::CLOSE, &[]);
        }
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
