//! Which sub-agents each session's coding agent has running, for the
//! sidebar's sub-agent rows.
//!
//! Each harness reads its own files behind `alacritree_subagents`'s
//! [`SubagentSource`]. This side knows the sessions. It hands every source
//! each one as a [`Host`], which carries the program its session backend
//! reports in the foreground, native or herdr-core alike.
//!
//! A scan stats files, so it runs on the job pool, one at a time and at most
//! every `SCAN_EVERY`, and only while some harness's `subagents` key is on.
//! Frames drive it. A running sub-agent's row animates, which keeps them
//! coming for as long as there is something to drop when it finishes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use alacritree_claude::subagents::ClaudeSubagents;
use alacritree_codex::CodexSubagents;
use alacritree_common::jobs;
#[cfg(test)]
use alacritree_subagents::FakeSource;
use alacritree_subagents::{Host, Scan, Subagent, SubagentSource, ambassador_impl_SubagentSource};
use ambassador::Delegate;

use crate::session::SessionId;

/// How long a scan's answer stands. Sub-agents run for minutes, so a row that
/// lags its agent by a couple of seconds reads as live.
const SCAN_EVERY: Duration = Duration::from_secs(2);

/// The harnesses whose sub-agents this build reads, each a backend crate.
#[derive(Delegate)]
#[delegate(SubagentSource)]
pub(crate) enum Source {
    Claude(ClaudeSubagents),
    Codex(CodexSubagents),
    /// Answers from a script, so the watch can be tested without any
    /// harness's files.
    #[cfg(test)]
    Fake(FakeSource),
}

/// Which harnesses the user asked to list sub-agents for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Harnesses {
    pub(crate) claude: bool,
    pub(crate) codex: bool,
}

impl Harnesses {
    pub(crate) fn any(self) -> bool {
        self.claude || self.codex
    }
}

/// Where each harness keeps its files.
#[derive(Clone, Debug)]
pub(crate) struct Homes {
    pub(crate) claude: Option<PathBuf>,
    pub(crate) codex: Option<PathBuf>,
}

impl Default for Homes {
    fn default() -> Self {
        Self {
            claude: alacritree_claude::subagents::config_dir(),
            codex: alacritree_codex::home_dir(),
        }
    }
}

impl Homes {
    fn sources(&self, harnesses: Harnesses) -> Vec<Source> {
        let mut sources = Vec::new();
        if harnesses.claude {
            sources.push(Source::Claude(ClaudeSubagents::at(self.claude.clone())));
        }
        if harnesses.codex {
            sources.push(Source::Codex(CodexSubagents::at(self.codex.clone())));
        }
        sources
    }
}

/// A session a scan covers, and what a source sees of it.
pub(crate) type Hosted = (SessionId, Host);

type Running = HashMap<SessionId, Vec<Subagent>>;

#[derive(Default)]
pub(crate) struct SubagentWatch {
    pub(crate) homes: Homes,
    harnesses: Harnesses,
    running: Running,
    /// Lent to the scan in flight and handed back with its answer, so what
    /// one scan read is not read again by the next.
    sources: Option<Vec<Source>>,
    job: Option<jobs::Job<(Vec<Source>, Running)>>,
    next_scan: Option<Instant>,
}

impl SubagentWatch {
    /// Whether no session has a sub-agent running.
    pub(crate) fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// The sub-agents `session` has running, oldest first.
    pub(crate) fn of(&self, session: SessionId) -> &[Subagent] {
        self.running.get(&session).map_or(&[], Vec::as_slice)
    }

    /// Take a finished scan's answer, then start the next scan once one is
    /// due. `hosts` lists the sessions a harness could run in, and is asked
    /// only when a scan starts. A change to `harnesses` drops everything, the
    /// scan in flight included, so a harness turned off stops listing at once.
    pub(crate) fn poll(
        &mut self,
        ctx: &egui::Context,
        harnesses: Harnesses,
        hosts: impl FnOnce() -> Vec<Hosted>,
    ) {
        if harnesses != self.harnesses {
            *self = Self { homes: self.homes.clone(), harnesses, ..Self::default() };
        }
        if !harnesses.any() {
            return;
        }
        match self.job.as_ref().map(|job| (job.poll(), job.failed())) {
            Some((Some((sources, running)), _)) => {
                self.sources = Some(sources);
                self.job = None;
                // The pool wakes a frame at every job end, but this one runs
                // after the sidebar painted, so the change waits for another.
                if running != self.running {
                    self.running = running;
                    ctx.request_repaint();
                }
            },
            Some((None, false)) => return,
            // A panicked scan took its sources with it; the next starts over.
            Some((None, true)) => self.job = None,
            None => {},
        }

        let now = Instant::now();
        if self.next_scan.is_some_and(|due| now < due) {
            return;
        }
        self.next_scan = Some(now + SCAN_EVERY);
        let hosts = hosts();
        if hosts.is_empty() {
            self.running.clear();
            return;
        }
        let mut sources = self.sources.take().unwrap_or_else(|| self.homes.sources(harnesses));
        self.job = Some(jobs::pool().spawn(jobs::Priority::Background, move |_| {
            let running = scan(&mut sources, &hosts);
            (sources, running)
        }));
    }

    /// How long until the next scan is due, for a caller keeping the rows
    /// fresh while nothing else asks for a frame.
    pub(crate) fn wait(&self, now: Instant) -> Option<Duration> {
        self.next_scan.map(|due| due.saturating_duration_since(now))
    }

    /// Pretend a scan found `agents` running in `session`.
    #[cfg(test)]
    pub(crate) fn set_for_test(&mut self, session: SessionId, agents: Vec<Subagent>) {
        self.running.insert(session, agents);
    }

    /// Make the next poll scan, rather than wait out `SCAN_EVERY`.
    #[cfg(test)]
    pub(crate) fn scan_due(&mut self) {
        self.next_scan = None;
    }
}

/// Every source's answer, merged per session.
fn scan(sources: &mut [Source], hosted: &[Hosted]) -> Running {
    let hosts: Vec<Host> = hosted.iter().map(|(_, host)| host.clone()).collect();
    let mut running = Running::new();
    for source in sources {
        let Scan { running: found, errors } = source.scan(&hosts);
        for e in errors {
            log::debug!("sub-agents: {e}");
        }
        for ((id, _), agents) in hosted.iter().zip(found) {
            if !agents.is_empty() {
                running.entry(*id).or_default().extend(agents);
            }
        }
    }
    running
}

#[cfg(test)]
mod tests {
    use alacritree_subagents::{ForegroundProgram, Side};

    use super::*;

    fn agent(name: &str) -> Subagent {
        Subagent { id: name.into(), name: name.into(), detail: None }
    }

    fn hosted(id: SessionId, cwd: &str) -> Hosted {
        let foreground = ForegroundProgram {
            side: Side::Native,
            pid: Some(id as u32),
            argv: vec!["claude".into()],
            cwd: Some(PathBuf::from(cwd)),
        };
        (id, Host { foreground, agent: Some("claude") })
    }

    #[test]
    fn every_sources_answer_lands_on_its_session() {
        let mut sources = vec![
            Source::Fake(FakeSource::default().running_in("/a", vec![agent("explore")])),
            Source::Fake(FakeSource::default().running_in("/a", vec![agent("review")])),
            Source::Fake(FakeSource::default().running_in("/b", vec![agent("plan")])),
        ];
        let running = scan(&mut sources, &[hosted(1, "/a"), hosted(2, "/b"), hosted(3, "/c")]);
        let names = |id| running.get(&id).map(|a| a.iter().map(|a| a.name.clone()).collect());
        assert_eq!(names(1), Some(vec!["explore".to_owned(), "review".to_owned()]));
        assert_eq!(names(2), Some(vec!["plan".to_owned()]));
        assert_eq!(names(3), None, "a session with nothing running has no entry");
    }

    #[test]
    fn the_harnesses_turned_on_pick_the_sources() {
        let kinds = |h: Harnesses| {
            Homes::default()
                .sources(h)
                .iter()
                .map(|s| match s {
                    Source::Claude(_) => "claude",
                    Source::Codex(_) => "codex",
                    Source::Fake(_) => "fake",
                })
                .collect::<Vec<_>>()
        };
        assert!(kinds(Harnesses::default()).is_empty());
        assert_eq!(kinds(Harnesses { claude: true, codex: true }), ["claude", "codex"]);
        assert_eq!(kinds(Harnesses { claude: false, codex: true }), ["codex"]);
    }
}
