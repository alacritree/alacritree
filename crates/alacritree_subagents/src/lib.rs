//! The sub-agents a coding agent runs on its own behalf, and which of
//! alacritree's sessions started them.
//!
//! A harness such as Claude Code or Codex keeps each sub-agent's transcript
//! on disk, so a backend reads those files rather than asking the agent.
//! Everything the app needs goes behind [`SubagentSource`]: given the
//! sessions alacritree runs, which sub-agents each has running. Another
//! harness is a new backend crate and a new variant of the app's dispatch
//! enum. A scan reads files, so it blocks, and the app runs it on a pool
//! worker.

// The trait's signatures are copied verbatim into the app crate by
// ambassador's delegation macro, so they name types by absolute path, and
// this crate must answer to its own name for those paths to resolve here too.
extern crate self as alacritree_subagents;

#[cfg(any(test, feature = "test-support"))]
mod fake;
pub mod tail;

use std::io;
use std::path::PathBuf;

#[cfg(any(test, feature = "test-support"))]
pub use self::fake::FakeSource;

/// A session a harness may run in, as the app sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Where the session's shell last said it was, else its workspace's
    /// directory.
    pub cwd: Option<PathBuf>,
    /// The agent the process probe sees in the session, by its canonical
    /// name, such as `claude` or `codex`.
    pub agent: Option<&'static str>,
}

/// Which host a process runs in, for a harness that names its processes by
/// pid.
pub trait Lineage {
    /// The index into the scanned hosts of the session whose shell `pid`
    /// descends from.
    fn host_of(&self, pid: u32) -> Option<usize>;
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
    #[error("could not read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[ambassador::delegatable_trait]
pub trait SubagentSource {
    /// The sub-agents running in each of `hosts`. Blocking: it reads the
    /// harness's files.
    fn scan(
        &mut self,
        hosts: &[::alacritree_subagents::Host],
        lineage: &dyn ::alacritree_subagents::Lineage,
    ) -> ::alacritree_subagents::Scan;
}
