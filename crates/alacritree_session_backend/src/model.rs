//! What the app hands a session backend, and what a backend reports back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use alacritree_common::side::Side;
use alacritree_common::wsl_helper::WslProbe;
use alacritty_terminal::event::WindowSize;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Rgb;

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
#[derive(Clone)]
pub struct Screen<L> {
    pub term: Arc<FairMutex<Term<L>>>,
    pub listener: L,
    /// The pane's size when the terminal starts, so the program draws its
    /// first output at the size it keeps.
    pub window_size: WindowSize,
}

/// The colors a session's grid draws with when the program sets none, for a
/// backend that answers the program's color queries itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Option<Rgb>,
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
#[derive(Clone)]
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
    pub colors: TerminalColors,
    pub placement: Placement,
    pub screen: Screen<L>,
}

/// Where a session sits in the app. A backend whose terminals outlive the
/// app records it with the terminal, so the next launch puts the terminal
/// back in the same place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// The checkout the session belongs to. `None` is the home tab.
    pub workspace: Option<PathBuf>,
    /// The session's index among its workspace's sessions.
    pub position: usize,
    /// How other clients name the workspace, as `<project> / <checkout>`.
    pub title: Option<String>,
}

/// A terminal that a backend keeps and no session holds, with the place
/// it last had in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoptable {
    /// The terminal's id in its backend, such as `t7`.
    pub terminal: String,
    pub placement: Placement,
}

/// Everything a backend needs to show a terminal it already runs in a
/// session.
#[derive(Clone)]
pub struct AttachRequest<L> {
    /// The terminal's id in its backend, such as `t7`.
    pub terminal: String,
    /// How many scrollback lines the first paint restores.
    pub history: usize,
    pub colors: TerminalColors,
    pub screen: Screen<L>,
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
