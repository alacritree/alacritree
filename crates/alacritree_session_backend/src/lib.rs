//! What alacritree asks of whatever runs a session's terminal.
//!
//! The app reaches a backend only through [`SessionBackend`], and a running
//! session only through its [`SessionHandle`]. Every backend feeds a `Term`
//! the app owns and wakes the UI through the app's listener, so the grid and
//! its repaint rules are the same on every backend.

// The traits' signatures are copied verbatim into the app crate by
// ambassador's delegation macro, so they name types by absolute path, and
// this crate must answer to its own name for those paths to resolve here too.
extern crate self as alacritree_session_backend;

mod model;
mod programs;
#[cfg(any(test, feature = "test-support"))]
mod scripted;

use std::path::PathBuf;

pub use alacritree_common::side::Side;
pub use alacritree_common::wsl_helper::WslProbe;
pub use alacritree_multiplexer::PaneKey;
use strum::Display;

pub use self::model::{ForegroundProgram, OpenRequest, ProcessTree, Screen, ShellCommand, Signals};
pub use self::programs::{AGENT_PROCESS_NAMES, is_nav_tui_name};
#[cfg(any(test, feature = "test-support"))]
pub use self::scripted::{Call, Record, Scripted, ScriptedHandle};

/// How a process inside a session names itself to `alacritree session move`
/// and the MCP tools.
pub const SESSION_ID_ENV: &str = "ALACRITREE_SESSION_ID";

/// The session backends alacritree can run a terminal on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display)]
#[strum(serialize_all = "snake_case")]
pub enum SessionBackendKind {
    Native,
    /// Answers from a script instead of a terminal, so app behavior can be
    /// tested at the trait.
    #[cfg(any(test, feature = "test-support"))]
    Scripted,
}

/// An operation that a backend may not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
#[strum(serialize_all = "lowercase")]
pub enum Operation {
    Attach,
    Watch,
    Detach,
}

/// Why a backend gave no terminal, or could not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Checked up front, because the PTY layer's own error for this reads
    /// "The directory name is invalid" on Windows.
    #[error("working directory no longer exists: {}", .0.display())]
    MissingDirectory(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the {backend} session backend cannot {operation} a terminal")]
    Unsupported { backend: SessionBackendKind, operation: Operation },
    #[cfg(any(test, feature = "test-support"))]
    #[error("{0}")]
    Scripted(String),
}

/// What the app asks of a session backend. `H` is the app's own handle type,
/// which each backend's handle converts into. Every method may block for
/// milliseconds.
#[ambassador::delegatable_trait]
pub trait SessionBackend<H> {
    /// Start a new terminal for a session.
    fn open<L>(
        &self,
        request: ::alacritree_session_backend::OpenRequest<L>,
    ) -> ::std::result::Result<H, ::alacritree_session_backend::SessionError>
    where
        L: ::alacritty_terminal::event::EventListener + ::std::marker::Send + 'static;

    /// The panes this backend runs that no session holds and that a session
    /// may take over.
    fn adoptable(&self) -> ::std::vec::Vec<::alacritree_session_backend::PaneKey>;

    /// Show `pane` in a session that types into it and sizes it.
    fn attach<L>(
        &self,
        pane: &::alacritree_session_backend::PaneKey,
        screen: ::alacritree_session_backend::Screen<L>,
    ) -> ::std::result::Result<H, ::alacritree_session_backend::SessionError>
    where
        L: ::alacritty_terminal::event::EventListener + ::std::marker::Send + 'static;

    /// Show `pane` in a session that neither types into it nor resizes it.
    fn watch<L>(
        &self,
        pane: &::alacritree_session_backend::PaneKey,
        screen: ::alacritree_session_backend::Screen<L>,
    ) -> ::std::result::Result<H, ::alacritree_session_backend::SessionError>
    where
        L: ::alacritty_terminal::event::EventListener + ::std::marker::Send + 'static;
}

/// What the app asks of one running session.
///
/// Dropping a handle ends nothing on its own except a native PTY, which
/// cannot outlive its handle. A session that closes calls [`Self::kill`].
#[ambassador::delegatable_trait]
pub trait SessionHandle {
    /// Send bytes to the program as if the user typed them. Empty input
    /// sends nothing, because a zero-byte write hangs the terminal.
    fn write(&self, bytes: ::std::vec::Vec<u8>);

    /// Tell the program the pane's new size.
    fn resize(&mut self, size: ::alacritty_terminal::event::WindowSize);

    /// Whether the user is typing into this session. Answers whether the
    /// session now holds a scheduling boost for its process tree.
    fn set_focused(&self, focused: bool) -> bool;

    /// End the program and its terminal.
    fn kill(&mut self);

    /// Let go of the terminal and leave its program running. A detach never
    /// kills a terminal, so a backend that cannot keep one alive refuses.
    fn detach(&mut self) -> ::std::result::Result<(), ::alacritree_session_backend::SessionError>;

    /// The program that holds the terminal, when the backend can name it.
    fn foreground(&self) -> ::std::option::Option<::alacritree_session_backend::ForegroundProgram>;

    /// What the backend's probe says about the programs behind the terminal.
    /// Answers are cached, so a frame may ask more than once.
    fn signals(&self) -> ::alacritree_session_backend::Signals;
}
