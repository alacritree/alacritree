//! One side's core: the resident `hc connect` that carries its streams, and
//! the route a one-shot call takes to it.
//!
//! The native side spawns `hc connect` itself. A distro's resident is one
//! `wsl.exe` for the whole run. Until it is up, the distro's one-shot calls
//! go through its WSL helper, so neither costs a `wsl.exe` launch per
//! session.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use alacritree_common::side::{self, Side};
use alacritree_common::tools::Tool;
use alacritree_common::wsl_helper::{self, HelperClient, TransportError};
use alacritree_common::{command_ext, wsl};
use serde_json::Value;

use crate::connection::{Connection, Waker};
use crate::elsewhere::{self, HcPaths};
use crate::{CoreError, one_shot};

/// How long a session waits between attempts to attach while its side has
/// no connection.
pub(crate) const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

enum Launch {
    /// `socket` replaces the socket hc resolves, once the user chose a core
    /// of another session.
    Native { program: String, socket: Mutex<Option<PathBuf>> },
    /// `program` is the configured `wsl_path`.
    Wsl { distro: String, program: Option<String> },
}

/// Prints the distro user's home directory from the passwd entry, which
/// holds whatever environment `wsl.exe` or the helper hands down.
const HOME_SCRIPT: &str = r#"getent passwd "$(id -un)" | cut -d: -f6"#;

pub(crate) struct SideCore {
    side: Side,
    launch: Launch,
    connection: Mutex<Slot>,
    waker: Waker,
    home: OnceLock<String>,
}

/// The side's connection, and when the last attempt to open one failed.
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SideCore {
    pub(crate) fn native(program: &str, waker: Waker) -> Self {
        let launch = Launch::Native { program: program.to_string(), socket: Mutex::new(None) };
        Self::new(Side::Native, launch, waker)
    }

    pub(crate) fn wsl(distro: &str, program: Option<String>, waker: Waker) -> Self {
        let launch = Launch::Wsl { distro: distro.to_string(), program };
        Self::new(Side::Wsl(distro.to_string()), launch, waker)
    }

    fn new(side: Side, launch: Launch, waker: Waker) -> Self {
        Self { side, launch, connection: Default::default(), waker, home: OnceLock::new() }
    }

    pub(crate) fn side(&self) -> &Side {
        &self.side
    }

    pub(crate) fn waker(&self) -> &Waker {
        &self.waker
    }

    /// The native core's socket override. `None` on a distro's core.
    pub(crate) fn socket(&self) -> Option<MutexGuard<'_, Option<PathBuf>>> {
        match &self.launch {
            Launch::Native { socket, .. } => Some(lock(socket)),
            Launch::Wsl { .. } => None,
        }
    }

    /// The resident connection, when it is up. Never waits, so the UI thread
    /// can ask while another thread holds the slot to open a connection,
    /// which can take seconds.
    pub(crate) fn live(&self) -> Option<Arc<Connection>> {
        let slot = match self.connection.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        slot.live()
    }

    /// The resident connection, opened first if there is none. Holding the
    /// slot while it opens keeps concurrent opens on one connection.
    pub(crate) fn connection(&self) -> Result<Arc<Connection>, CoreError> {
        self.open_in(&mut lock(&self.connection))
    }

    /// The resident connection, opened first unless an attempt failed within
    /// the last [`RECONNECT_INTERVAL`]. Every session that waits for the
    /// side's connection asks here, so they share one attempt per interval.
    pub(crate) fn reconnect(&self) -> Option<Arc<Connection>> {
        let mut slot = lock(&self.connection);
        if slot.live().is_none()
            && slot.failed_at.is_some_and(|at| at.elapsed() < RECONNECT_INTERVAL)
        {
            return None;
        }
        self.open_in(&mut slot)
            .inspect_err(|error| {
                log::debug!("herdr-core ({}) reconnect: {error}", self.side.name())
            })
            .ok()
    }

    fn open_in(&self, slot: &mut Slot) -> Result<Arc<Connection>, CoreError> {
        if let Some(connection) = slot.live() {
            return Ok(connection);
        }
        let opened = self.command().and_then(|(command, label)| {
            log::info!("herdr-core ({}): starting `{label}`", self.side.name());
            Connection::open(command, &label, self.waker.clone())
        });
        slot.failed_at = opened.is_err().then(Instant::now);
        let connection = opened?;
        slot.connection = Some(connection.clone());
        Ok(connection)
    }

    /// The `hc connect` command and how a log line names it. The native side
    /// refuses first when a core listens where hc does not look.
    fn command(&self) -> Result<(Command, String), CoreError> {
        match &self.launch {
            Launch::Native { program, socket } => {
                let socket = lock(socket).clone();
                if socket.is_none()
                    && let Some(found) = elsewhere::before_hc(&HcPaths::from_env())
                {
                    return Err(CoreError::Elsewhere(found));
                }
                let mut command = command_ext::hidden(program);
                command.arg("connect");
                if let Some(socket) = socket {
                    command.env("HC_SOCKET", socket);
                }
                Ok((command, format!("{program} connect")))
            },
            Launch::Wsl { distro, program } => {
                let hc = program.as_deref().unwrap_or(Tool::Hc.name());
                let command = side::wsl_command_with_clean_stdout(distro, hc, &["connect"]);
                Ok((command, format!("wsl.exe -d {distro} -- {hc} connect")))
            },
        }
    }

    /// Answers whether the native core already serves, without starting one.
    /// A distro's core is never asked, so it answers no.
    pub(crate) fn serving(&self) -> bool {
        let Launch::Native { program, socket } = &self.launch else { return false };
        let mut command = command_ext::hidden(program);
        command.args(["server", "status", "--json"]);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(socket) = lock(socket).as_ref() {
            command.env("HC_SOCKET", socket);
        }
        // Only the pane listing asks, from a thread of its own.
        #[allow(clippy::disallowed_methods)]
        command.status().is_ok_and(|status| status.success())
    }

    /// Kill the `hc connect` child, as a crash would.
    #[cfg(feature = "test-support")]
    pub(crate) fn kill_connection(&self) {
        if let Some(connection) = lock(&self.connection).connection.as_ref() {
            connection.kill();
        }
    }

    /// The distro's helper when it is up, and the `hc` it runs. That is the
    /// configured path, else where the helper's login shell found it.
    fn helper(&self) -> Option<(Arc<HelperClient>, String)> {
        let Launch::Wsl { distro, program } = &self.launch else { return None };
        let client = wsl_helper::client(distro)?;
        let program = program
            .clone()
            .or_else(|| client.capabilities()?.path(Tool::Hc.name()).map(str::to_string))
            .unwrap_or_else(|| Tool::Hc.name().to_string());
        Some((client, program))
    }

    /// The distro user's home directory, asked once. A terminal's cwd must
    /// be absolute, since the core resolves no `~`. Blocks, as backend calls
    /// may.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn home(&self) -> Option<String> {
        if let Some(home) = self.home.get() {
            return Some(home.clone());
        }
        let Launch::Wsl { distro, .. } = &self.launch else { return None };
        let output = match self.helper() {
            Some((client, _)) => client.run(HOME_SCRIPT, &[]).ok()?.1,
            None => {
                wsl::command(distro, None).args(["sh", "-c", HOME_SCRIPT]).output().ok()?.stdout
            },
        };
        let home = String::from_utf8_lossy(&output).trim().to_string();
        (!home.is_empty()).then(|| self.home.get_or_init(|| home).clone())
    }

    /// Whether a one-shot call goes out now without opening the resident.
    pub(crate) fn helper_ready(&self) -> bool {
        self.helper().is_some()
    }

    /// One call that opens no stream: on the resident connection while it is
    /// up, else through the distro's helper, else on a resident opened for
    /// it. A helper that took the call and gave no answer is an error, since
    /// the call may have run.
    pub(crate) fn call(&self, method: &str, params: Value) -> Result<Value, CoreError> {
        if let Some(connection) = self.live() {
            return connection.call(method, params);
        }
        if let Some((client, program)) = self.helper() {
            let [hello, request] = one_shot::lines(method, params.clone());
            match client.run(one_shot::SCRIPT, &[&program, &hello, &request]) {
                Ok((_, output)) => return one_shot::answer(&output),
                Err(TransportError::NotWritten(error)) => {
                    log::debug!("herdr-core ({}): helper unusable: {error}", self.side.name());
                },
                Err(error) => return Err(CoreError::Helper(error)),
            }
        }
        self.connection()?.call(method, params)
    }
}
