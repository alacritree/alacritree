//! The sub-agents a coding agent runs on its own behalf, and which of
//! alacritree's sessions started them.
//!
//! A harness such as Claude Code or Codex keeps each sub-agent's transcript
//! on disk, so a backend reads those files rather than asking the agent. A
//! session in a WSL distro keeps them in the distro, which [`Home`] reaches.
//! [`SubagentSource`] takes the program each session's backend reports in
//! the foreground and answers which sub-agents each session has running.
//! Another harness is a new backend crate with its own config section, and a
//! source the app's `subagents` module builds from that section. A scan reads
//! files, so it blocks, and the app runs it on a pool worker.

// The trait's signatures are copied verbatim into the app crate by
// ambassador's delegation macro, so they name types by absolute path, and
// this crate must answer to its own name for those paths to resolve here too.
extern crate self as alacritree_subagents;

#[cfg(any(test, feature = "test-support"))]
mod fake;
pub mod follow;
mod home;
pub mod tail;

use std::io;
use std::path::PathBuf;

pub use alacritree_session_backend::{ForegroundProgram, Side};

#[cfg(any(test, feature = "test-support"))]
pub use self::fake::FakeSource;
pub use self::home::Home;

/// A session a harness may run in, as its session backend reports it. The
/// foreground program's pid and cwd are as its side spells them, so a
/// source reads them against that side's [`Home`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The program that holds the session's terminal.
    pub foreground: ForegroundProgram,
    /// The coding agent the backend's probe recognizes in the session, by
    /// its canonical name, such as `claude` or `codex`.
    pub agent: Option<&'static str>,
}

/// A sub-agent that is still working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    /// The harness's id for it, stable for its life.
    pub id: String,
    /// What a row calls it: the task it was given, where the harness names
    /// one.
    pub name: String,
    /// What else the harness says about it, such as its type, its workflow or
    /// its nickname, for hover text.
    pub detail: Option<String>,
}

/// One scan's answer: each host's running sub-agents, oldest first, in the
/// order the hosts were given, and what could not be read on the way. A file
/// that fails costs the sub-agents it would have shown, never the scan.
#[derive(Debug, Default)]
pub struct Scan {
    pub running: Vec<Vec<Subagent>>,
    pub errors: Vec<SubagentError>,
}

impl Scan {
    /// Nothing running in any of `hosts` hosts.
    pub fn empty(hosts: usize) -> Self {
        Self { running: vec![Vec::new(); hosts], errors: Vec::new() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("could not list {}", path.display())]
    List {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[ambassador::delegatable_trait]
pub trait SubagentSource {
    /// The sub-agents running in each of `hosts`. Blocking: it reads the
    /// harness's files.
    fn scan(&mut self, hosts: &[::alacritree_subagents::Host]) -> ::alacritree_subagents::Scan;
}
