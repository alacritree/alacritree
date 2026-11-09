//! The session backend that runs each terminal in its side's herdr-core
//! core: the native core, or the core inside a WSL distro.
//!
//! Terminals start with `keep`. Closing a session kills a terminal that
//! `labels` marks as alacritree's, and only detaches from any other.
//! Quitting detaches, and the next launch adopts alacritree's own. A distro
//! whose core runs a terminal gets a keeper, so WSL does not stop it after
//! the app ends. A session's parser attaches its stream once the side's
//! connection is up, and again after a drop or a loss.
//! Terminals on machines that `hc machine add` connected are ignored.

mod connection;
mod core;
mod elsewhere;
mod install;
mod keeper;
mod labels;
mod one_shot;
mod panes;
mod parser;
mod pinned;
mod settings;
mod wire;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alacritree_common::wsl;
use alacritree_common::wsl_helper::{self, TransportError};
use alacritree_process_probe::ProbeHandle;
use alacritree_session_backend::{
    Adoptable, AttachRequest, ForegroundProgram, OpenRequest, Placement, Screen, SessionBackend,
    SessionBackendKind, SessionError, SessionHandle, Side, Signals, TerminalColors,
};
use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::vte::ansi::Rgb;
use serde_json::{Value, json};

use self::connection::{Answer, Connection, Facts, Foreground, State, Waker};
use self::core::{RECONNECT_INTERVAL, SideCore};
use self::elsewhere::HcPaths;
pub use self::elsewhere::{CoreElsewhere, HcConfigError, Remedy};
pub use self::install::{InstallError, Installer};
pub use self::panes::HerdrCorePanes;
use self::parser::Chunk;
pub use self::settings::{HerdrCoreConfig, RawHerdrCore, SessionBackendChoice};

/// The glyph that marks a herdr-core pane: herdr's ram's head from the
/// bundled symbol font, since herdr-core is herdr's terminal core.
pub const DEFAULT_ICON: &str = "\u{10FF00}";

/// Why the core could not be reached, or refused a call.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("could not start `{program}`: {source}")]
    Spawn { program: String, source: std::io::Error },
    #[error("`hc connect` exited before it answered: {0}")]
    Refused(String),
    /// No `hc` on the side, where its installer would put one.
    #[error("hc is not installed on the {} side", .0.name())]
    Missing(Side),
    #[error("a core listens at {}, where hc does not look", .0.socket.display())]
    Elsewhere(CoreElsewhere),
    #[error("the core's answer was not herdr-core's protocol: {0}")]
    Protocol(String),
    #[error("the connection to the core closed")]
    Closed,
    #[error("the core did not answer {0} in time")]
    Timeout(String),
    #[error("the core refused the call ({kind}): {message}{}", hint.as_ref().map(|hint| format!(". {hint}")).unwrap_or_default())]
    Rpc { kind: String, message: String, hint: Option<String> },
    /// The WSL helper took the call and gave no answer, so it may have run.
    #[error(transparent)]
    Helper(TransportError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl CoreError {
    /// A JSON-RPC error names its kind under `data`. `hc connect`'s own
    /// refusal, sent before any request, names it beside its message.
    fn rpc(error: &Value) -> Self {
        let field = |name: &str| error["data"][name].as_str().or_else(|| error[name].as_str());
        Self::Rpc {
            kind: field("kind").unwrap_or("unknown").to_string(),
            message: error["message"].as_str().unwrap_or_default().to_string(),
            hint: field("hint").map(str::to_string),
        }
    }

    /// Where the core listens, when hc refused because it looked for the
    /// core at another socket. hc sends no field for the socket, so this
    /// reads it from the wording of hc's message.
    fn listening_at(&self) -> Option<String> {
        match self {
            Self::Rpc { kind, message, .. } if kind == "unavailable" => {
                let at = message.strip_prefix("the core listens at ")?;
                Some(at.split_once(", but this shell looks at ")?.0.to_string())
            },
            Self::Refused(text) => text.lines().find_map(|line| {
                let refusal: Value = serde_json::from_str(line).ok()?;
                Self::rpc(refusal.get("error")?).listening_at()
            }),
            _ => None,
        }
    }

    /// The call went out and no answer came back, so it may have run.
    fn unanswered(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Helper(TransportError::NoReply(_)))
    }

    /// The core refused the call for now, such as an attach to a terminal
    /// that waits for its new shell.
    fn is_unavailable(&self) -> bool {
        matches!(self, Self::Rpc { kind, .. } if kind == "unavailable")
    }

    fn unavailable(self) -> SessionError {
        SessionError::Unavailable { backend: SessionBackendKind::HerdrCore, source: Box::new(self) }
    }
}

/// The side whose missing `hc` kept a terminal out of its core, when that
/// is why `error` happened.
pub fn hc_missing(error: &SessionError) -> Option<&Side> {
    let SessionError::Unavailable { source, .. } = error else { return None };
    match source.downcast_ref::<CoreError>()? {
        CoreError::Missing(side) => Some(side),
        _ => None,
    }
}

/// Why [`HerdrCore::connect`] did not reach the core.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct Unreachable {
    pub error: CoreError,
    /// A core that serves hc's state where hc does not look, when one
    /// answers there.
    pub elsewhere: Option<CoreElsewhere>,
}

impl From<CoreError> for SessionError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Io(error) => SessionError::Io(error),
            error if error.unanswered() => SessionError::Unconfirmed {
                backend: SessionBackendKind::HerdrCore,
                source: Box::new(error),
            },
            error => SessionError::Io(std::io::Error::other(error)),
        }
    }
}

/// Runs terminals in one side's core. Clones share one connection, which
/// opens on first use and again after it drops.
#[derive(Clone)]
pub struct HerdrCore {
    core: Arc<SideCore>,
    claims: Arc<Mutex<Claims>>,
}

/// The terminals this process holds, so none is adopted twice.
#[derive(Default)]
struct Claims {
    /// Every terminal a session opened, or that was handed out to adopt.
    held: HashSet<String>,
    /// Opens whose terminal the core may already report but whose address
    /// is not in `held` yet. Adoption waits while any is in flight.
    opening: usize,
    /// The distro's terminals as its helper listed them, before the
    /// resident connection reports them.
    listing: Listing,
    /// What the helper's listing said of each terminal, for an attach that
    /// comes before the resident connection reports it.
    listed: HashMap<String, Facts>,
}

#[derive(Default)]
enum Listing {
    #[default]
    Unasked,
    Asked,
    Listed(HashMap<String, Facts>),
    Taken,
}

impl HerdrCore {
    /// The native side's core. `program` is the `hc` binary, as
    /// `[integrations.herdr_core] path` names it.
    pub fn new(program: &str) -> Self {
        Self::on(SideCore::native(program, Waker::default()))
    }

    /// `distro`'s core. `program` is `[integrations.herdr_core] wsl_path`,
    /// or `None` to find `hc` through the distro's login shell.
    pub fn wsl(distro: &str, program: Option<String>) -> Self {
        Self::on(SideCore::wsl(distro, program, Waker::default()))
    }

    fn on(core: SideCore) -> Self {
        Self { core: Arc::new(core), claims: Default::default() }
    }

    /// Call `wake` whenever the core reports a terminal that may be
    /// adoptable or listed, so a terminal an agent starts shows up in an
    /// idle app. Only the first call counts.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.core.waker().set(Box::new(wake));
    }

    /// Connect, as `ready` does. When the native side's hc refuses, also
    /// look for a core that serves hc's state on a socket hc does not
    /// resolve. A socket the user already chose is not looked past.
    pub fn connect(&self) -> Result<(), Unreachable> {
        self.core.connection().map(drop).map_err(|error| {
            let unchosen = self.core.socket().is_some_and(|socket| socket.is_none());
            let elsewhere = match &error {
                CoreError::Elsewhere(found) => Some(found.clone()),
                CoreError::Refused(_) if unchosen => elsewhere::find(&HcPaths::from_env()),
                _ => None,
            };
            Unreachable { error, elsewhere }
        })
    }

    /// Reach the core that `found` names the way its remedy says. The core
    /// and its terminals run on untouched.
    pub fn use_core(&self, found: &CoreElsewhere) -> Result<(), HcConfigError> {
        match found.remedy {
            Remedy::WriteConfig => elsewhere::point_hc_at(&HcPaths::from_env(), &found.socket),
            Remedy::SetEnv => {
                if let Some(mut socket) = self.core.socket() {
                    *socket = Some(found.socket.clone());
                }
                Ok(())
            },
        }
    }

    /// Kill the `hc connect` child, as a crash would.
    #[cfg(feature = "test-support")]
    pub fn kill_connection(&self) {
        self.core.kill_connection();
    }

    /// Answers whether a keeper holds this side up.
    #[cfg(feature = "test-support")]
    pub fn keeper_runs(&self) -> bool {
        self.core.keeper_runs()
    }

    /// Open the side's connection in the background, so the first stream
    /// does not wait for `hc connect` to start.
    pub fn warm(&self) {
        let core = self.core.clone();
        let warming = std::thread::Builder::new().name("hc-warm".into()).spawn(move || {
            if let Err(error) = core.connection() {
                log::warn!("herdr-core ({}): {error}", core.side().name());
            }
        });
        if let Err(error) = warming {
            log::warn!("herdr-core warm thread: {error}");
        }
    }

    /// The distro's terminals from its helper, asked once while the
    /// resident connection is not up yet. Empty until the answer is in.
    fn listed(&self, claims: &mut Claims) -> HashMap<String, Facts> {
        match std::mem::replace(&mut claims.listing, Listing::Taken) {
            Listing::Unasked if self.core.helper_ready() => {
                claims.listing = Listing::Asked;
                self.list_through_helper();
                HashMap::new()
            },
            Listing::Listed(listed) => listed,
            unchanged => {
                claims.listing = unchanged;
                HashMap::new()
            },
        }
    }

    fn list_through_helper(&self) {
        let (core, claims) = (self.core.clone(), self.claims.clone());
        let listing = std::thread::Builder::new().name("hc-list".into()).spawn(move || {
            let listed = core.call("terminal.list", json!({}));
            let mut claims = lock(&claims);
            claims.listing = match listed {
                Ok(result) => Listing::Listed(records(&result["terminals"])),
                Err(error) => {
                    log::warn!("herdr-core ({}): could not list: {error}", core.side().name());
                    Listing::Taken
                },
            };
            drop(claims);
            if let Some(wake) = core.waker().get() {
                wake();
            }
        });
        if let Err(error) = listing {
            log::warn!("herdr-core list thread: {error}");
        }
    }

    /// Show `terminal` in `screen`: attached at once while the side's
    /// connection is up, else by its parser once it is. An attach that fails
    /// at once ends nothing, and the caller decides whether the terminal
    /// lives on.
    fn show<L>(
        &self,
        terminal: Terminal,
        Screen { term, listener, window_size, history }: Screen<L>,
    ) -> Result<HerdrCoreHandle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let Terminal { address, started, display, first_paint_history, placement, access } =
            terminal;
        let size = (window_size.num_cols, window_size.num_lines);
        let mut link = Link {
            connection: None,
            channel: 0,
            automatic_input: false,
            size,
            display,
            focused: None,
            placement,
            held: Vec::new(),
            pid: started.pid,
        };
        let stream = match self.core.live() {
            Some(connection) => {
                let view =
                    View { size, display: &link.display, history: first_paint_history, access };
                let opened = open_stream(&connection, &address, view)?;
                link.connection = Some(connection);
                link.channel = opened.channel;
                link.automatic_input = opened.automatic_input;
                Some(opened.stream)
            },
            None => None,
        };
        let attached = Arc::new(Attached {
            core: self.core.clone(),
            address,
            history,
            access,
            link: Mutex::new(link),
            closed: AtomicBool::new(false),
        });
        if let Err(error) = parser::spawn(term, listener, stream, attached.clone()) {
            attached.link().send(wire::CLOSE, &[]);
            return Err(error.into());
        }

        let native = matches!(self.core.side(), Side::Native);
        Ok(HerdrCoreHandle {
            attached,
            probe: native.then(|| RefCell::new((started.pid, ProbeHandle::new(started.pid, None)))),
            started,
            killed: false,
        })
    }
}

/// The core of every side, one per side for the whole run. Every core shares
/// one waker.
#[derive(Clone)]
pub struct HerdrCores {
    native: HerdrCore,
    wsl_path: Option<String>,
    waker: Waker,
    distros: Arc<Mutex<HashMap<String, HerdrCore>>>,
}

impl HerdrCores {
    pub fn new(config: &HerdrCoreConfig) -> Self {
        let waker = Waker::default();
        Self {
            native: HerdrCore::on(SideCore::native(&config.path, waker.clone())),
            wsl_path: config.wsl_path.clone(),
            waker,
            distros: Default::default(),
        }
    }

    pub fn on(&self, side: &Side) -> HerdrCore {
        match side {
            Side::Native => self.native.clone(),
            Side::Wsl(distro) => lock(&self.distros)
                .entry(distro.clone())
                .or_insert_with(|| {
                    let core = SideCore::wsl(distro, self.wsl_path.clone(), self.waker.clone());
                    HerdrCore::on(core)
                })
                .clone(),
        }
    }

    /// Every side's core that this run has used so far, with its side.
    pub fn all(&self) -> Vec<(Side, HerdrCore)> {
        let distros = lock(&self.distros);
        let wsl = distros.iter().map(|(distro, core)| (Side::Wsl(distro.clone()), core.clone()));
        std::iter::once((Side::Native, self.native.clone())).chain(wsl).collect()
    }

    /// Call `wake` whenever any side's core reports a terminal that may be
    /// adoptable. Only the first call counts.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.waker.set(Box::new(wake));
    }

    /// Leave a keeper for each distro this run used whose core may still
    /// have a live terminal, in case the keeper its connection started is
    /// gone. Answers each keeper's pid.
    pub fn leave_keepers(&self) -> Vec<u32> {
        lock(&self.distros).values().filter_map(|core| core.core.leave_keeper()).collect()
    }

    /// Open a distro's resident connection whenever its WSL helper starts,
    /// so the two pay the distro's start together and a session finds both.
    pub fn open_beside_wsl_helpers(&self) {
        let cores = self.clone();
        wsl_helper::on_spawn(move |distro| cores.on(&Side::Wsl(distro.to_string())).warm());
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

/// `dir` without a trailing separator, as a native core's `cwd`.
///
/// The core sets the shell's `PWD` from `cwd`, and a shell such as nushell
/// refuses one that ends in a separator. A root keeps its separator.
fn core_cwd(dir: &Path) -> String {
    dir.components().collect::<PathBuf>().display().to_string()
}

/// The terminal identity a PTY child inherits from alacritree's own
/// environment, which a core terminal starts without.
const IDENTITY_ENV: [&str; 2] = ["TERM", "COLORTERM"];

impl<H: From<HerdrCoreHandle>> SessionBackend<H> for HerdrCore {
    /// A distro whose helper is up takes a terminal at once, through the
    /// helper, while its resident connection may still be opening.
    fn ready(&self) -> Result<(), SessionError> {
        if self.core.helper_ready() {
            return Ok(());
        }
        self.core.connection().map(drop).map_err(CoreError::unavailable)
    }

    /// A distro's terminal runs the core's own shell in the distro, since
    /// the request's shell is a `wsl.exe` launch that runs on Windows. It
    /// starts where that launch would.
    fn open<L>(&self, request: OpenRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let OpenRequest { shell, working_directory, mut env, colors, placement, screen, .. } =
            request;
        let (argv, cwd) = match self.core.side() {
            Side::Native => {
                ensure_working_directory(working_directory.as_deref())?;
                let argv: Vec<String> = shell
                    .map(|shell| std::iter::once(shell.program).chain(shell.args).collect())
                    .unwrap_or_default();
                (argv, working_directory.as_deref().map(core_cwd))
            },
            Side::Wsl(_) => {
                let launch_dir =
                    shell.as_ref().and_then(|shell| wsl_helper::launch_dir(&shell.args));
                let dir = launch_dir.or_else(|| Some(working_directory?.display().to_string()));
                (Vec::new(), wsl::linux_dir(dir.as_deref(), || self.core.home()))
            },
        };
        for name in IDENTITY_ENV {
            if let Ok(value) = std::env::var(name) {
                env.entry(name.to_string()).or_insert(value);
            }
        }
        let window_size = screen.window_size;
        let display = DisplayFacts::new(colors, window_size);
        lock(&self.claims).opening += 1;
        let run = self.core.call(
            "terminal.run",
            json!({
                "argv": argv,
                "cwd": cwd,
                "env": env,
                "size": { "cols": window_size.num_cols, "rows": window_size.num_lines },
                "display": display.json(),
                "no_parent": true,
                "no_notify": true,
                "keep": true,
                "labels": labels::for_run(&placement),
            }),
        );
        let (address, started) = {
            let mut claims = lock(&self.claims);
            claims.opening -= 1;
            let (address, started) = record_facts(&run?);
            claims.held.insert(address.clone());
            (address, started)
        };

        let terminal = Terminal {
            address: address.clone(),
            started,
            display,
            first_paint_history: 0,
            placement,
            access: Access::Own,
        };
        self.show(terminal, screen).map(H::from).inspect_err(|_| {
            end(self.core.clone(), address, KILL_AND_REMOVE);
        })
    }

    /// Before a distro's resident connection is up, its terminals come from
    /// one listing through its helper.
    fn adoptable(&self) -> Vec<Adoptable> {
        let mut claims = lock(&self.claims);
        if claims.opening > 0 {
            return Vec::new();
        }
        let found: Vec<Adoptable> = match self.core.live() {
            Some(connection) => {
                connection.pick_terminals(|address, facts| adoptable(&claims, address, facts))
            },
            None => {
                let listed = self.listed(&mut claims);
                let found = listed
                    .iter()
                    .filter_map(|(address, facts)| adoptable(&claims, address, facts))
                    .collect();
                claims.listed.extend(listed);
                found
            },
        };
        claims.held.extend(found.iter().map(|adoptable| adoptable.terminal.clone()));
        found
    }

    fn attach<L>(&self, request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        self.take_over(request, None).map(H::from)
    }

    fn watch<L>(&self, request: AttachRequest<L>) -> Result<H, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        self.take_over(request, Some(Access::Watch)).map(H::from)
    }
}

impl HerdrCore {
    /// Show a running terminal in a session. Without an `access`, the
    /// terminal's labels say whether it is alacritree's own.
    fn take_over<L>(
        &self,
        AttachRequest { terminal, colors, screen }: AttachRequest<L>,
        access: Option<Access>,
    ) -> Result<HerdrCoreHandle, SessionError>
    where
        L: EventListener + Send + 'static,
    {
        let facts = self
            .core
            .live()
            .and_then(|connection| connection.facts(&terminal))
            .or_else(|| lock(&self.claims).listed.remove(&terminal))
            .unwrap_or_default();
        let placement = labels::placement(&facts.labels);
        let access =
            access.unwrap_or(if placement.is_some() { Access::Own } else { Access::Foreign });
        let terminal = Terminal {
            address: terminal,
            placement: placement.unwrap_or_default(),
            started: facts,
            display: DisplayFacts::new(colors, screen.window_size),
            first_paint_history: screen.history,
            access,
        };
        self.show(terminal, screen)
    }

    /// End the program in `address` and keep its record, off the caller's
    /// thread. The record is for its creator to remove. With no connection
    /// open there is no core to ask, and connecting could start one.
    pub fn kill_terminal(&self, address: &str) {
        let Some(connection) = self.core.live() else {
            log::warn!("herdr-core: no core to kill {address} in");
            return;
        };
        let terminal = json!({ "terminal": address });
        let killed = std::thread::Builder::new()
            .name("hc-kill".into())
            .spawn(move || heard(&connection, &["terminal.kill"], &terminal));
        if let Err(error) = killed {
            log::warn!("herdr-core kill thread: {error}");
        }
    }
}

/// What a session may do to the terminal it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// A terminal alacritree created. The session types into it, sizes it
    /// and records its place, and closing the session kills it.
    Own,
    /// A terminal another client created. The session types into it and
    /// sizes it, and closing the session only detaches.
    Foreign,
    /// Any terminal, shown without typing into it or sizing it. Closing the
    /// session only detaches.
    Watch,
}

fn adoptable(claims: &Claims, address: &str, facts: &Facts) -> Option<Adoptable> {
    let placement = labels::placement(&facts.labels)?;
    (facts.state == State::Live && !claims.held.contains(address))
        .then(|| Adoptable { terminal: address.to_string(), placement })
}

/// A terminal in the core, about to be shown in a session.
struct Terminal {
    address: String,
    /// The facts until the side's connection reports this terminal.
    started: Facts,
    display: DisplayFacts,
    /// Scrollback lines the first paint restores. A new terminal has none,
    /// while an adopted one restores what the grid keeps.
    first_paint_history: usize,
    /// What the terminal's labels record.
    placement: Placement,
    access: Access,
}

/// Each terminal record of a `terminal.list` answer, by address.
fn records(terminals: &Value) -> HashMap<String, Facts> {
    let all = terminals.as_array().into_iter().flatten().map(record_facts);
    all.filter(|(address, _)| connection::on_this_machine(address)).collect()
}

/// A terminal record's address and facts. A record without a foreground
/// report has its shell in the foreground, as the core started it.
fn record_facts(record: &Value) -> (String, Facts) {
    let address = record["address"].as_str().unwrap_or_default().to_string();
    let pid = record["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok());
    let argv: Vec<String> = serde_json::from_value(record["argv"].clone()).unwrap_or_default();
    let started = match (argv.first(), pid, record["cwd"].as_str()) {
        (Some(arg0), Some(pid), Some(cwd)) => Some(Foreground {
            pid: i32::try_from(pid).unwrap_or_default(),
            name: arg0.rsplit(['/', '\\']).next().unwrap_or(arg0).to_string(),
            argv: argv.clone(),
            cwd: PathBuf::from(cwd),
        }),
        _ => None,
    };
    let text = |key: &str| record[key].as_str().map(str::to_string);
    let facts = Facts {
        foreground: serde_json::from_value(record["foreground"].clone()).ok().or(started),
        state: State::of_record(record["state"].as_str().unwrap_or_default(), pid),
        pid,
        labels: serde_json::from_value(record["labels"].clone()).unwrap_or_default(),
        name: text("name"),
        title: text("title"),
        cwd: text("cwd").map(PathBuf::from),
        app: record["status"]["app"].as_str().map(str::to_string),
    };
    (address, facts)
}

/// A stream that `terminal.attach` opened.
struct Opened {
    channel: u32,
    stream: Receiver<Chunk>,
    automatic_input: bool,
}

/// How a session attaches to its terminal.
struct View<'a> {
    size: (u16, u16),
    display: &'a DisplayFacts,
    /// Scrollback lines the first paint restores.
    history: usize,
    access: Access,
}

/// Attach to `address` on `connection` as `view` says.
fn open_stream(
    connection: &Connection,
    address: &str,
    View { size: (cols, rows), display, history, access }: View<'_>,
) -> Result<Opened, CoreError> {
    let attached = connection.call_streaming(
        "terminal.attach",
        json!({
            "terminal": address,
            "cols": cols,
            "rows": rows,
            "display": display.json(),
            "history": history,
            // The core takes no input and no size from a watching stream.
            "observe": access == Access::Watch,
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

/// The calls that end a terminal alacritree created and remove its record.
const KILL_AND_REMOVE: &[&str] = &["terminal.kill", "terminal.rm"];

/// Make the `methods` calls on a terminal, off the caller's thread, since a
/// kill escalates through signals and can take seconds. Without the side's
/// connection it tries again every [`RECONNECT_INTERVAL`], so a closed
/// session never leaves its terminal running.
fn end(core: Arc<SideCore>, address: String, methods: &'static [&'static str]) {
    let ended = std::thread::Builder::new().name("hc-kill".into()).spawn(move || {
        let terminal = json!({ "terminal": address });
        loop {
            if let Some(connection) = core.reconnect()
                && heard(&connection, methods, &terminal)
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

/// Answers whether the core heard every call. A refusal counts as heard,
/// such as a kill of a terminal that already finished.
fn heard(connection: &Connection, methods: &[&str], terminal: &Value) -> bool {
    for method in methods {
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

/// A session's terminal in its side's core, shared by its handle and its
/// parser.
struct Attached {
    core: Arc<SideCore>,
    address: String,
    /// Scrollback lines a reattach restores.
    history: usize,
    access: Access,
    link: Mutex<Link>,
    /// The session let go of the stream, by a detach, a kill or a drop. It
    /// changes only under the `link` lock, and a wait on the facts reads it
    /// without that lock.
    closed: AtomicBool,
}

/// The stream a session rides on now, and what the core must hear again on
/// a new one. The parser sets the stream once the side's connection is up,
/// and replaces it when the connection drops.
struct Link {
    /// `None` until the first attach.
    connection: Option<Arc<Connection>>,
    channel: u32,
    /// The stream takes AUTOMATIC_INPUT end to end.
    automatic_input: bool,
    size: (u16, u16),
    display: DisplayFacts,
    focused: Option<bool>,
    /// Where the session sits, which the terminal's labels record.
    placement: Placement,
    /// Input typed before the first attach, sent in order once it is up.
    held: Vec<(u8, Vec<u8>)>,
    /// The program the stream shows.
    pid: Option<u32>,
}

impl Link {
    /// Before the first attach, input waits in `held` and every other frame
    /// is dropped, since the attach carries the state it would set.
    fn send(&mut self, kind: u8, payload: &[u8]) {
        match &self.connection {
            Some(connection) => connection.send(self.channel, kind, payload),
            None if matches!(kind, wire::DATA | wire::AUTOMATIC_INPUT) => {
                self.held.push((kind, payload.to_vec()));
            },
            None => {},
        }
    }

    /// Never blocks, since nothing waits for the label call's answer.
    fn send_labels(&self, address: &str) {
        if let Some(connection) = &self.connection {
            let labels = labels::of(&self.placement);
            connection.cast("terminal.label", json!({ "terminal": address, "labels": labels }));
        }
    }

    fn send_size(&mut self) {
        let [c0, c1] = self.size.0.to_le_bytes();
        let [r0, r1] = self.size.1.to_le_bytes();
        self.send(wire::RESIZE, &[c0, c1, r0, r1]);
    }

    fn send_display(&mut self) {
        let display = self.display.json().to_string();
        self.send(wire::FACTS, display.as_bytes());
    }

    fn send_focus(&mut self) {
        if let Some(focused) = self.focused {
            self.send(wire::FOCUS, &[u8::from(focused)]);
        }
    }

    /// Input held for the first attach, in the order it was sent. A reply
    /// goes as DATA when the stream does not take AUTOMATIC_INPUT.
    fn take_held(&mut self) -> Vec<(u8, Vec<u8>)> {
        let reply = if self.automatic_input { wire::AUTOMATIC_INPUT } else { wire::DATA };
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(kind, bytes)| (if kind == wire::AUTOMATIC_INPUT { reply } else { kind }, bytes))
            .collect()
    }
}

/// What a session's parser follows once it waited for a connection, or for
/// the shell the core starts after it lost the program.
pub(crate) enum Resumed {
    /// A new stream, whose first paint restores the screen.
    Stream(Receiver<Chunk>),
    /// The core no longer has the terminal.
    Gone,
    /// The session let go of the terminal while it waited.
    Released,
    /// The connection dropped while the session waited for a new shell.
    Dropped,
}

/// What the core says became of a terminal whose stream it closed.
pub(crate) enum Ending {
    /// The core starts a new shell in place of the lost program.
    Lost,
    /// The program ended for good, or the terminal is gone.
    Final,
    /// The session let go of the terminal while it waited.
    Released,
    /// The connection dropped before the core said.
    Dropped,
}

impl Attached {
    fn link(&self) -> MutexGuard<'_, Link> {
        lock(&self.link)
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Tell the core that `bytes` of the current stream were parsed.
    pub(crate) fn credit(&self, bytes: usize) {
        let link = self.link();
        if let Some(connection) = &link.connection {
            connection.credit(link.channel, bytes);
        }
    }

    /// Attach once the side's connection is up, trying every
    /// [`RECONNECT_INTERVAL`] until the core answers. `wait` holds the first
    /// try back by one interval, as after a drop. Blocks, so only the parser
    /// thread calls it.
    pub(crate) fn resume(&self, mut wait: bool) -> Resumed {
        loop {
            if self.closed() {
                return Resumed::Released;
            }
            if std::mem::replace(&mut wait, true) {
                std::thread::sleep(RECONNECT_INTERVAL);
            }
            let Some(connection) = self.core.reconnect() else { continue };
            match self.reattach(connection) {
                Ok(resumed) => return resumed,
                // The core refuses an attach while it starts a lost
                // program's new shell.
                Err(error @ CoreError::Rpc { .. }) if error.is_unavailable() => {
                    log::debug!("herdr-core attach of {}: {error}", self.address);
                },
                // A session that closed while it waited may have killed and
                // removed the terminal itself, and must not report an exit.
                Err(CoreError::Rpc { .. } | CoreError::Protocol(_)) if self.closed() => {
                    return Resumed::Released;
                },
                Err(error @ (CoreError::Rpc { .. } | CoreError::Protocol(_))) => {
                    log::warn!("herdr-core could not attach {}: {error}", self.address);
                    return Resumed::Gone;
                },
                Err(error) => log::debug!("herdr-core attach of {}: {error}", self.address),
            }
        }
    }

    /// The connection the stream rides on, and the program it shows.
    fn shown(&self) -> (Option<Arc<Connection>>, Option<u32>) {
        let link = self.link();
        (link.connection.clone(), link.pid)
    }

    /// Wait until the core's events say whether the stream it closed ended
    /// the terminal or lost its program. Blocks, so only the parser thread
    /// calls it.
    pub(crate) fn ending(&self) -> Ending {
        let (Some(connection), shown) = self.shown() else { return Ending::Dropped };
        connection
            .wait(|terminals| {
                if self.closed() {
                    return Some(Ending::Released);
                }
                let Some(facts) = terminals.get(&self.address) else { return Some(Ending::Final) };
                match facts.state {
                    State::Finished => Some(Ending::Final),
                    State::Lost => Some(Ending::Lost),
                    // The core already restarted it, before this client
                    // applied the loss.
                    State::Live => (shown.is_some() && facts.pid != shown).then_some(Ending::Lost),
                }
            })
            .unwrap_or(Ending::Dropped)
    }

    /// Wait for the shell the core starts after it lost the terminal's
    /// program, and attach to it. Blocks, so only the parser thread calls it.
    pub(crate) fn restart(&self) -> Resumed {
        loop {
            let (Some(connection), shown) = self.shown() else { return Resumed::Dropped };
            let restarted = connection.wait(|terminals| {
                if self.closed() {
                    return Some(Err(Resumed::Released));
                }
                let Some(facts) = terminals.get(&self.address) else {
                    return Some(Err(Resumed::Gone));
                };
                match facts.state {
                    State::Finished => Some(Err(Resumed::Gone)),
                    State::Lost => None,
                    State::Live => (facts.pid != shown).then_some(Ok(())),
                }
            });
            match restarted {
                None => return Resumed::Dropped,
                Some(Err(resumed)) => return resumed,
                Some(Ok(())) => {},
            }
            match self.reattach(connection) {
                Ok(resumed) => return resumed,
                Err(error) => {
                    log::debug!(
                        "herdr-core reattach of {} to its new shell: {error}",
                        self.address
                    );
                    std::thread::sleep(RECONNECT_INTERVAL);
                },
            }
        }
    }

    /// Move the session onto a new stream on `connection`, and repeat what
    /// changed while it was being opened. Input held for the first attach
    /// goes out last.
    fn reattach(&self, connection: Arc<Connection>) -> Result<Resumed, CoreError> {
        let (size, display) = {
            let link = self.link();
            (link.size, link.display.clone())
        };
        let view = View { size, display: &display, history: self.history, access: self.access };
        let opened = open_stream(&connection, &self.address, view)?;
        let recorded = connection.facts(&self.address).and_then(|f| labels::placement(&f.labels));

        let mut link = self.link();
        link.pid = connection.facts(&self.address).and_then(|facts| facts.pid);
        link.connection = Some(connection);
        link.channel = opened.channel;
        link.automatic_input = opened.automatic_input;
        if self.closed() {
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
        if self.access == Access::Own && recorded.as_ref() != Some(&link.placement) {
            link.send_labels(&self.address);
        }
        for (kind, bytes) in link.take_held() {
            link.send(kind, &bytes);
        }
        Ok(Resumed::Stream(opened.stream))
    }
}

/// A session's terminal in its side's core, and the stream it is attached
/// on, or waits to attach on.
pub struct HerdrCoreHandle {
    attached: Arc<Attached>,
    /// Reads the programs under the terminal's shell, for a native core, and
    /// that shell's pid. On Windows the core reports only the program it
    /// started.
    probe: Option<RefCell<(Option<u32>, ProbeHandle)>>,
    /// The facts until the side's connection reports this terminal.
    started: Facts,
    killed: bool,
}

impl HerdrCoreHandle {
    /// The terminal's address in its core, such as `t7`.
    pub fn terminal(&self) -> &str {
        &self.attached.address
    }

    /// The session watches the terminal: it neither types into it nor
    /// sizes it.
    fn watching(&self) -> bool {
        self.attached.access == Access::Watch
    }

    /// What the current connection last heard about the terminal.
    fn facts(&self) -> Facts {
        let connection = self.attached.link().connection.clone();
        connection
            .and_then(|connection| connection.facts(&self.attached.address))
            .unwrap_or_else(|| self.started.clone())
    }

    fn close_stream(&self) {
        let connection = {
            let mut link = self.attached.link();
            if self.attached.closed.swap(true, Ordering::Relaxed) {
                return;
            }
            link.send(wire::CLOSE, &[]);
            link.connection.clone()
        };
        if let Some(connection) = connection {
            connection.wake();
        }
    }
}

impl SessionHandle for HerdrCoreHandle {
    fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() && !self.watching() {
            self.attached.link().send(wire::DATA, &bytes);
        }
    }

    /// Without AUTOMATIC_INPUT on the whole path a reply goes as DATA, which
    /// the core counts as typing. A reply held for the first attach waits to
    /// learn the path.
    fn reply(&self, bytes: Vec<u8>) {
        if bytes.is_empty() || self.watching() {
            return;
        }
        let mut link = self.attached.link();
        let automatic = link.automatic_input || link.connection.is_none();
        let kind = if automatic { wire::AUTOMATIC_INPUT } else { wire::DATA };
        link.send(kind, &bytes);
    }

    fn resize(&mut self, size: WindowSize) {
        if self.watching() {
            return;
        }
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
        if self.watching() {
            return false;
        }
        let mut link = self.attached.link();
        if link.focused.replace(focused) != Some(focused) {
            link.send_focus();
        }
        false
    }

    /// Only a terminal alacritree created ends here. The session lets go of
    /// any other one, so a close never kills a terminal another client
    /// started, and its record stays for that client.
    fn kill(&mut self) {
        if std::mem::replace(&mut self.killed, true) {
            return;
        }
        self.close_stream();
        if self.attached.access == Access::Own {
            end(self.attached.core.clone(), self.attached.address.clone(), KILL_AND_REMOVE);
        }
    }

    /// The terminal runs on in the core, where the next launch adopts it.
    fn detach(&mut self) -> Result<(), SessionError> {
        self.close_stream();
        Ok(())
    }

    /// A move while the stream is not up reaches the labels on attach. A
    /// terminal another client created keeps its labels as they are.
    fn place(&mut self, placement: &Placement) {
        if self.attached.access != Access::Own {
            return;
        }
        let mut link = self.attached.link();
        if *placement != link.placement {
            link.placement = placement.clone();
            link.send_labels(&self.attached.address);
        }
    }

    /// As fresh as the core's last foreground poll, as this machine reaches
    /// the path.
    fn cwd(&self) -> Option<PathBuf> {
        Some(cwd_here(self.attached.core.side(), &self.facts().foreground?.cwd))
    }

    /// The cwd as the side spells it.
    fn foreground(&self) -> Option<ForegroundProgram> {
        Some(foreground_program(self.attached.core.side(), self.facts().foreground?))
    }

    /// Follows the shell the stream shows, which a lost program replaces.
    fn signals(&self) -> Signals {
        let pid = self.attached.link().pid;
        match &self.probe {
            Some(probe) => {
                let mut probe = probe.borrow_mut();
                if probe.0 != pid {
                    *probe = (pid, ProbeHandle::new(pid, None));
                }
                probe.1.signals()
            },
            None => alacritree_process_probe::reported_signals(self.foreground().as_ref(), pid),
        }
    }
}

/// `cwd`, which a core on `side` reported, as this machine reaches it.
fn cwd_here(side: &Side, cwd: &Path) -> PathBuf {
    match side {
        Side::Native => cwd.to_path_buf(),
        Side::Wsl(distro) => wsl::linux_to_windows(&cwd.to_string_lossy(), distro),
    }
}

/// The program a core on `side` reported in the foreground, with its pid and
/// cwd as the side spells them.
fn foreground_program(side: &Side, foreground: Foreground) -> ForegroundProgram {
    ForegroundProgram {
        side: side.clone(),
        pid: u32::try_from(foreground.pid).ok(),
        argv: if foreground.argv.is_empty() { vec![foreground.name] } else { foreground.argv },
        cwd: Some(foreground.cwd),
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

    fn colors() -> TerminalColors {
        TerminalColors {
            foreground: Rgb { r: 0xff, g: 0x80, b: 0 },
            background: Rgb { r: 0, g: 0, b: 0x10 },
            cursor: None,
        }
    }

    fn size(num_cols: u16, num_lines: u16) -> WindowSize {
        WindowSize { num_lines, num_cols, cell_width: 9, cell_height: 18 }
    }

    #[test]
    fn a_helper_listing_keeps_only_this_machines_terminals() {
        let live = |address: &str| json!({ "address": address, "pid": 10, "state": "live" });

        let listed = records(&json!([live("t1"), live("box/t1")]));

        assert_eq!(listed.keys().collect::<Vec<_>>(), ["t1"]);
    }

    /// hc names where the core listens, in an RPC error from a call and in
    /// the line `hc connect` prints before it exits.
    #[test]
    fn a_refusal_names_where_the_core_listens() {
        let message = "the core listens at /tmp/hc.sock, but this shell looks at \
                       /run/user/1000/herdr-core/default.sock";
        let rpc =
            CoreError::Rpc { kind: "unavailable".into(), message: message.into(), hint: None };
        let refusal = json!({ "error": { "kind": "unavailable", "message": message } });
        let refused = CoreError::Refused(refusal.to_string());
        let after_a_warning = CoreError::Refused(format!(
            "wsl: Failed to translate 'C:\\tools'\n{refusal}\nsh: 1: trailing noise"
        ));
        let other =
            CoreError::Rpc { kind: "not_found".into(), message: message.into(), hint: None };

        assert_eq!(rpc.listening_at().as_deref(), Some("/tmp/hc.sock"));
        assert_eq!(refused.listening_at().as_deref(), Some("/tmp/hc.sock"));
        assert_eq!(after_a_warning.listening_at().as_deref(), Some("/tmp/hc.sock"));
        assert_eq!(other.listening_at(), None);
        assert_eq!(CoreError::Refused("sh: 1: hc: not found".into()).listening_at(), None);
    }

    #[test]
    fn a_core_cwd_drops_a_trailing_separator_but_keeps_a_roots() {
        let (dir, root) =
            if cfg!(windows) { (r"C:\Users\dev", r"C:\") } else { ("/home/dev", "/") };
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(core_cwd(Path::new(&format!("{dir}{sep}"))), dir);
        assert_eq!(core_cwd(Path::new(dir)), dir);
        assert_eq!(core_cwd(Path::new(root)), root);
    }

    /// A distro core's foreground program keeps the distro's pid and Linux
    /// cwd, which the sub-agent sources read against the distro's files,
    /// while the sidebar's cwd is the path this machine reaches.
    #[test]
    fn a_distro_cores_program_keeps_the_distros_spelling() {
        let ubuntu = Side::Wsl("Ubuntu".into());
        let reported = |argv: &[&str]| Foreground {
            pid: 4242,
            name: "claude".into(),
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            cwd: PathBuf::from("/home/dev/app"),
        };
        assert_eq!(foreground_program(&ubuntu, reported(&[])), ForegroundProgram {
            side: ubuntu.clone(),
            pid: Some(4242),
            argv: vec!["claude".into()],
            cwd: Some(PathBuf::from("/home/dev/app")),
        });
        let program = foreground_program(&ubuntu, reported(&["/usr/bin/node", "cli.js"]));
        assert_eq!(program.argv, ["/usr/bin/node", "cli.js"]);
        assert_eq!(
            cwd_here(&ubuntu, Path::new("/home/dev/app")),
            wsl::linux_to_windows("/home/dev/app", "Ubuntu")
        );
    }

    /// A session whose side had no connection when it opened, as a distro
    /// session is while its resident connection starts.
    fn waiting_for_its_first_attach() -> HerdrCoreHandle {
        let link = Link {
            connection: None,
            channel: 0,
            automatic_input: false,
            size: (80, 24),
            display: DisplayFacts::new(colors(), size(80, 24)),
            focused: None,
            placement: Placement::default(),
            held: Vec::new(),
            pid: None,
        };
        let core = Arc::new(SideCore::native("hc", Waker::default()));
        let attached = Arc::new(Attached {
            core,
            address: "t1".into(),
            history: 0,
            access: Access::Own,
            link: Mutex::new(link),
            closed: AtomicBool::new(false),
        });
        HerdrCoreHandle { attached, probe: None, started: Facts::default(), killed: false }
    }

    #[test]
    fn display_facts_spell_colors_as_the_core_reads_them() {
        assert_eq!(
            DisplayFacts::new(colors(), size(80, 24)).json(),
            json!({ "fg": "#ff8000", "bg": "#000010", "cursor": null, "cell": [9, 18] })
        );
    }

    /// Keys typed while the resident connection opens are not lost: they go
    /// out once the stream attaches, in the order typed, and a query reply
    /// still goes as automatic input. A resize waits for the attach, which
    /// sends the latest size itself.
    #[test]
    fn input_sent_before_the_first_attach_goes_out_in_order() {
        let mut handle = waiting_for_its_first_attach();
        handle.write(b"ls".to_vec());
        handle.reply(b"\x1b[0n".to_vec());
        handle.resize(size(120, 40));
        handle.write(b"\r".to_vec());

        let mut link = handle.attached.link();
        link.automatic_input = true;
        assert_eq!(link.take_held(), vec![
            (wire::DATA, b"ls".to_vec()),
            (wire::AUTOMATIC_INPUT, b"\x1b[0n".to_vec()),
            (wire::DATA, b"\r".to_vec()),
        ]);
        assert_eq!(link.size, (120, 40));
    }

    /// Without AUTOMATIC_INPUT on the stream, a held reply goes as DATA.
    #[test]
    fn a_held_reply_goes_as_data_on_a_stream_without_automatic_input() {
        let handle = waiting_for_its_first_attach();
        handle.reply(b"\x1b[0n".to_vec());

        assert_eq!(handle.attached.link().take_held(), vec![(wire::DATA, b"\x1b[0n".to_vec())]);
    }

    /// A session let go before its first attach never takes a stream.
    #[test]
    fn a_session_detached_before_its_first_attach_never_attaches() {
        let mut handle = waiting_for_its_first_attach();
        handle.detach().expect("a detach");

        assert!(matches!(handle.attached.resume(false), Resumed::Released));
    }
}
