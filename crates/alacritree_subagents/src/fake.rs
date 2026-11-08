//! A source that answers from a script, so the app's handling of a scan can
//! be tested without any harness's files on disk.

use std::path::PathBuf;

use crate::{Host, Scan, Subagent, SubagentSource};

/// Lists the sub-agents it was given for every host whose foreground program
/// runs in a directory.
#[derive(Debug, Default)]
pub struct FakeSource {
    running: Vec<(PathBuf, Vec<Subagent>)>,
}

impl FakeSource {
    /// Answer `agents` for every host whose foreground program runs in `cwd`.
    pub fn running_in(mut self, cwd: impl Into<PathBuf>, agents: Vec<Subagent>) -> Self {
        self.running.push((cwd.into(), agents));
        self
    }
}

impl SubagentSource for FakeSource {
    fn scan(&mut self, hosts: &[Host]) -> Scan {
        let running = hosts
            .iter()
            .map(|host| {
                self.running
                    .iter()
                    .filter(|(cwd, _)| host.foreground.cwd.as_ref() == Some(cwd))
                    .flat_map(|(_, agents)| agents.iter().cloned())
                    .collect()
            })
            .collect();
        Scan { running, errors: Vec::new() }
    }
}
