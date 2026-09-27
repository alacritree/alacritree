//! A forge that answers from a script and records what it was asked, for
//! tests of the code that schedules lookups. Clones share one record, so a
//! test keeps a clone to read after handing the forge away.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use alacritree_common::jobs::Blocking;

use crate::{ForgeError, Head, PrInfo, PullRequests, RemoteForge};

#[derive(Debug, Clone, Default)]
pub struct FakeForge {
    prs: HashMap<String, PrInfo>,
    failing: Vec<String>,
    calls: Arc<Mutex<Vec<Vec<Head>>>>,
    gate: Option<Release>,
}

/// Holds a [`FakeForge::paused`] forge's lookups between declaring their
/// steps and finishing them, until released.
#[derive(Debug, Clone, Default)]
pub struct Release(Arc<(Mutex<bool>, Condvar)>);

impl Release {
    pub fn release(&self) {
        *self.0.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0.1.notify_all();
    }

    fn wait(&self) {
        let mut open = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = self.0.1.wait(open).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl FakeForge {
    /// Answers `pr` for any checkout on `branch`, and no PR for the rest.
    pub fn with_pr(mut self, branch: &str, pr: PrInfo) -> Self {
        self.prs.insert(branch.to_string(), pr);
        self
    }

    /// Fails the lookup of any checkout on `branch`.
    pub fn failing_on(mut self, branch: &str) -> Self {
        self.failing.push(branch.to_string());
        self
    }

    /// Holds every lookup after it has declared its steps, until the
    /// returned handle releases it, so a test can watch one mid-flight.
    pub fn paused(mut self) -> (Self, Release) {
        let release = Release::default();
        self.gate = Some(release.clone());
        (self, release)
    }

    /// The heads of each call so far, in call order.
    pub fn calls(&self) -> Vec<Vec<Head>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl RemoteForge for FakeForge {
    /// One step per head, labelled by its branch, the way a backend reports
    /// one per request.
    fn pull_requests(&self, heads: Vec<Head>, blocking: &Blocking) -> PullRequests {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(heads.clone());
        blocking.set_steps(heads.iter().map(|h| h.branch.clone()).collect());
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        heads
            .into_iter()
            .map(|head| {
                let answer = if self.failing.contains(&head.branch) {
                    Err(ForgeError::Malformed { program: "fake" })
                } else {
                    Ok(self.prs.get(&head.branch).cloned())
                };
                let step = answer.as_ref().map(|_| ()).map_err(|e| e.to_string());
                blocking.step_done(&head.branch, step);
                (head.path, answer)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use alacritree_common::jobs;

    use super::*;

    fn head(branch: &str) -> Head {
        Head { path: PathBuf::from(format!("/{branch}")), branch: branch.into(), remotes: None }
    }

    #[test]
    fn the_fake_reports_one_step_per_head() {
        let forge = FakeForge::default().failing_on("bad");
        let (_, snap) = jobs::recorded(|b| forge.pull_requests(vec![head("good"), head("bad")], b));
        let steps: Vec<_> =
            snap.steps.iter().map(|s| (s.label.as_str(), s.outcome.clone())).collect();
        let error = ForgeError::Malformed { program: "fake" }.to_string();
        assert_eq!(steps, [("good", Some(Ok(()))), ("bad", Some(Err(error)))]);
    }
}
