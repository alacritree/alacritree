//! alacritree is a GUI-subsystem binary with no console of its own, so on
//! Windows each `git`/`gh`/`cmd` child gets a fresh console window unless we
//! pass `CREATE_NO_WINDOW`. `hidden` is the crate's one sanctioned way to
//! build a `Command`, so that flag can never be forgotten at a call site.

use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};

/// The flag that suppresses the console window. Public because creation flags
/// are one field, so a caller that names another flag has to pass this one too
/// rather than calling [`CommandExt::hide_console`].
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub trait CommandExt {
    /// Suppress the console window Windows would spawn for this child. No-op
    /// elsewhere.
    fn hide_console(&mut self) -> &mut Self;
}

impl CommandExt for Command {
    #[cfg(windows)]
    fn hide_console(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt as _;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn hide_console(&mut self) -> &mut Self {
        self
    }
}

/// Build a `Command` for `program`, pre-armed to skip the console window
/// Windows would otherwise pop for it. No-op elsewhere.
#[allow(clippy::disallowed_methods)] // the sanctioned spawner
pub fn hidden(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.hide_console();
    cmd
}

/// Start `command` so that it outlives alacritree, with no stdio of ours and
/// no visible console.
///
/// On Windows the child breaks away from any job alacritree runs in, since a
/// job that ends its members as it closes would end the child too. A job that
/// refuses the breakaway still gets the child. Blocks for the spawn.
// For a process that outlives the app. It starts on a thread of its own, or
// as the app quits after the window is gone, so no frame waits on it.
#[allow(clippy::disallowed_methods)]
pub fn spawn_detached(command: &mut Command) -> std::io::Result<Child> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        match command.spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                log::warn!("a job refused the breakaway, so the child ends with the job: {error}");
                command.creation_flags(CREATE_NO_WINDOW);
            },
            spawned => return spawned,
        }
    }
    command.spawn()
}
