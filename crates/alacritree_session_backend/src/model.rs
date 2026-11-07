//! What the app hands a session backend, and what a backend reports back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use alacritree_common::side::Side;
use alacritree_common::wsl_helper::WslProbe;
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;

/// A program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ShellCommand {
    pub fn new(program: String, args: Vec<String>) -> Self {
        Self { program, args }
    }
}

/// The grid a session's program draws into, and the listener its events
/// reach the app through. The app owns both, so every backend feeds the same
/// `Term` and wakes the UI the same way.
pub struct Screen<L> {
    pub term: Arc<FairMutex<Term<L>>>,
    pub listener: L,
    /// The pane's size when the terminal starts, so the program draws its
    /// first output at the size it keeps.
    pub window_size: WindowSize,
}

/// Whether a session's whole process tree is held in one job, which the
/// native backend creates when either setting wants it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessTree {
    /// Raise the tree one scheduling class while the user types into it.
    pub boost_on_focus: bool,
    /// End every process in the tree when the session closes.
    pub reap_on_close: bool,
}

/// Everything a backend needs to start a new terminal for a session.
pub struct OpenRequest<L> {
    /// The app's id for the session, which the program reads from
    /// [`crate::SESSION_ID_ENV`].
    pub session_id: u64,
    /// `None` runs the user's default shell.
    pub shell: Option<ShellCommand>,
    /// Quote each argument for Windows' single command line. Argv built in
    /// code needs it, so an argument with a space stays one argument. A
    /// config shell stays raw, as in upstream alacritty.
    pub escape_args: bool,
    pub working_directory: Option<PathBuf>,
    pub env: HashMap<String, String>,
    /// Set for a shell inside a WSL distro that a shim started, which is how
    /// the WSL helper finds its foreground program.
    pub wsl_probe: Option<WslProbe>,
    pub process_tree: ProcessTree,
    pub screen: Screen<L>,
}

/// A terminal that a backend runs and no session holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Terminal {
    pub side: Side,
    /// The backend's own name for the terminal, stable while it lives.
    pub id: String,
}

/// The program that holds a session's terminal right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProgram {
    pub side: Side,
    /// The pid on `side`. `None` when the side reports only a name.
    pub pid: Option<u32>,
    /// At least the program's name.
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// What a backend's probe can say about the programs behind a terminal.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Signals {
    /// A recognized coding agent in the foreground, by its canonical name.
    pub agent: Option<&'static str>,
    /// Something other than the shell itself is running.
    pub foreground_job: bool,
    /// A split-managing TUI such as vim or tmux is running.
    pub nav_tui: bool,
    /// The probe could ask about this terminal at all, so a caller can tell
    /// "no agent here" from "nobody could look".
    pub answered: bool,
}
