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
#[cfg(any(test, feature = "test-support"))]
mod scripted;

use std::path::PathBuf;

pub use alacritree_common::side::Side;
pub use alacritree_common::wsl_helper::WslProbe;
use strum::Display;

pub use self::model::{
    Adoptable, AttachRequest, ForegroundProgram, OpenRequest, Placement, ProcessTree, Screen,
    ShellCommand, Signals, TerminalColors,
};
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
    HerdrCore,
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
    /// The backend has nothing to open a terminal on, such as a core that
    /// does not answer.
    #[error("the {backend} session backend is unavailable: {source}")]
    Unavailable { backend: SessionBackendKind, source: Box<dyn std::error::Error + Send + Sync> },
    /// The request went out and no answer came back, so the terminal may
    /// run. Opening it on another backend could run the program twice.
    #[error("the {backend} session backend may have started the terminal: {source}")]
    Unconfirmed { backend: SessionBackendKind, source: Box<dyn std::error::Error + Send + Sync> },
    #[cfg(any(test, feature = "test-support"))]
    #[error("{0}")]
    Scripted(String),
}

/// What the app asks of a session backend. `H` is the app's own handle type,
/// which each backend's handle converts into. Every method may block for
/// milliseconds.
#[ambassador::delegatable_trait]
pub trait SessionBackend<H> {
    /// Whether this backend can open a terminal now, connecting to whatever
    /// runs its terminals if it must. The app asks before [`Self::open`], so
    /// a session that cannot open here can open on another backend.
    fn ready(&self) -> ::std::result::Result<(), ::alacritree_session_backend::SessionError>;

    /// Start a new terminal for a session.
    fn open<L>(
        &self,
        request: ::alacritree_session_backend::OpenRequest<L>,
    ) -> ::std::result::Result<H, ::alacritree_session_backend::SessionError>
    where
        L: ::alacritty_terminal::event::EventListener + ::std::marker::Send + 'static;

    /// The terminals this backend keeps that no session holds, each handed
    /// out once, even when its attach fails. Never blocks on the backend,
    /// since it answers from what the backend last heard. A caller takes
    /// each one over with [`Self::attach`].
    fn adoptable(&self) -> ::std::vec::Vec<::alacritree_session_backend::Adoptable>;

    /// Show a running terminal in a session that types into it and sizes it.
    fn attach<L>(
        &self,
        request: ::alacritree_session_backend::AttachRequest<L>,
    ) -> ::std::result::Result<H, ::alacritree_session_backend::SessionError>
    where
        L: ::alacritty_terminal::event::EventListener + ::std::marker::Send + 'static;

    /// Show a running terminal in a session that neither types into it nor
    /// resizes it.
    fn watch<L>(
        &self,
        request: ::alacritree_session_backend::AttachRequest<L>,
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

    /// Send bytes the terminal generated to answer the program's own query,
    /// which a backend may tell apart from typing.
    fn reply(&self, bytes: ::std::vec::Vec<u8>);

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

    /// Record where the session now sits in the app. A backend whose
    /// terminals die with the app records nothing.
    fn place(&mut self, placement: &::alacritree_session_backend::Placement);

    /// The program's working directory as the backend tracks it on its own,
    /// cheap enough to read every frame. `None` for a backend that tracks
    /// none, such as a PTY, whose cwd arrives through the shell's OSC 7.
    fn cwd(&self) -> ::std::option::Option<::std::path::PathBuf>;

    /// The program that holds the terminal, when the backend can name it.
    fn foreground(&self) -> ::std::option::Option<::alacritree_session_backend::ForegroundProgram>;

    /// What the backend's probe says about the programs behind the terminal.
    /// Answers are cached, so a frame may ask more than once.
    fn signals(&self) -> ::alacritree_session_backend::Signals;
}
