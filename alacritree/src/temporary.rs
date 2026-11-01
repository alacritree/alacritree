//! Temporary worktrees: checkouts an agent makes for one job and removes when
//! the job is done.
//!
//! What makes a checkout temporary is where it lives,
//! `<project dir>/.temporary/<origin>/<branch>`, so discovery, the CLI with no
//! window and a restart all agree without reading anything but the path.
//! `<origin>` names the checkout it was made from, where its row goes when
//! nothing better claims it.
//!
//! Which session made one is held by the window alone, in [`Sections`]. A
//! session id means nothing after a restart, and falling back to the origin is
//! the right answer then. Which agent made one is a list per harness session
//! beside the hook digests, which is what lets the hook remind an agent of its
//! own worktrees and nobody else's.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use alacritree_tasks::scope::{SessionRef, sanitize, session_from_env};
use serde_json::{Value, json};

use crate::ipc::protocol::IpcRequest;
use crate::projects::Project;
use crate::session::{SESSION_ID_ENV, SessionId};
use crate::sidebar_nav::{ListedRows, TemporaryOwner, TemporarySection, WorkspaceEntry};
use crate::workspace::WorkspaceKey;

/// The directory under a project's worktree directory that marks its contents.
pub(crate) const DIR: &str = ".temporary";

/// The directory name of the checkout `path` was made from, when `path` is a
/// temporary worktree.
pub(crate) fn origin_name(path: &Path) -> Option<&OsStr> {
    let origin = path.parent()?;
    (origin.parent()?.file_name()? == DIR).then(|| origin.file_name()).flatten()
}

pub(crate) fn is_temporary(path: &Path) -> bool {
    origin_name(path).is_some()
}

/// The window session this process runs inside, if any.
pub(crate) fn own_session(get: impl Fn(&str) -> Option<String>) -> Option<SessionId> {
    get(SESSION_ID_ENV)?.trim().parse().ok()
}

/// The reply to a create, the same from a running window and the offline path.
pub(crate) fn created_reply(path: &Path, steps: Vec<String>) -> Value {
    let mut reply = json!({ "path": path, "steps": steps });
    if is_temporary(path) {
        reply["cleanup"] = json!(cleanup_note(path));
    }
    reply
}

fn cleanup_note(path: &Path) -> String {
    format!(
        "Temporary worktree. When you are done with it, run `alacritree worktree remove {}` or \
         the remove_worktree MCP tool. That deletes its branch too.",
        path.display()
    )
}

/// What an agent is told about temporary worktrees whenever the hook speaks,
/// with the ones it has not removed yet.
pub(crate) fn agent_guide(open: &[PathBuf]) -> String {
    let mut text = String::from(
        "\n## Temporary worktrees\nFor throwaway work, such as a build, a test run or a look at \
         other code, make a temporary worktree instead of a normal one: run `alacritree worktree \
         create --temporary . <name>` from your checkout, or call `create_worktree` with \
         `temporary: true`. It prints the path, and the sidebar files it under your session in a \
         collapsed section. Cleaning it up is your job: when you are done, leave the directory \
         and run `alacritree worktree remove <path>` or call `remove_worktree`. That deletes its \
         branch too, so push anything worth keeping first. Use a normal worktree for work the \
         user will come back to.\n",
    );
    if !open.is_empty() {
        text.push_str("You still have these open. Remove each one you are done with:\n");
        text.push_str(&path_list(open));
    }
    text
}

/// What ends an agent's turn when it stops with worktrees still open.
pub(crate) fn stop_prompt(open: &[PathBuf]) -> String {
    format!(
        "You still have temporary worktrees open:\n{}If you are done with one, remove it with \
         `alacritree worktree remove <path>`. If you still need it, say so and stop: you will not \
         be asked about it again.",
        path_list(open)
    )
}

fn path_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|path| format!("- {}\n", path.display())).collect()
}

fn ledger_path(state_dir: &Path, session: &SessionRef) -> PathBuf {
    let name = format!("{}-{}.temporary", session.harness.prefix(), sanitize(&session.id));
    state_dir.join("task-hooks").join(name)
}

/// Add `path` to `session`'s list. A list that cannot be written costs a
/// reminder, never the worktree that was already made.
fn remember(state_dir: &Path, session: &SessionRef, path: &Path) {
    let ledger = ledger_path(state_dir, session);
    let mut text = std::fs::read_to_string(&ledger).unwrap_or_default();
    if text.lines().any(|line| Path::new(line) == path) {
        return;
    }
    text.push_str(&format!("{}\n", path.display()));
    let _ = ledger.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(&ledger, text);
}

/// The worktrees on `session`'s list that are still on disk. A stat rather
/// than a question to git, so the hook stays cheap, and a removal needs no
/// bookkeeping however it happened.
pub(crate) fn still_open(state_dir: &Path, session: &SessionRef) -> Vec<PathBuf> {
    let ledger = ledger_path(state_dir, session);
    let Ok(text) = std::fs::read_to_string(&ledger) else { return Vec::new() };
    let listed: Vec<PathBuf> = text.lines().map(PathBuf::from).collect();
    let open: Vec<PathBuf> = listed.iter().filter(|path| path.exists()).cloned().collect();
    if open.len() != listed.len() {
        let kept: String = open.iter().map(|path| format!("{}\n", path.display())).collect();
        let _ = std::fs::write(&ledger, kept);
    }
    open
}

/// Put a successful temporary create on the calling agent's list.
pub(crate) fn note_created(request: &IpcRequest, reply: &Value, state_dir: Option<&Path>) {
    let IpcRequest::CreateWorktree { temporary: true, .. } = request else { return };
    let (Some(dir), Some(path)) = (state_dir, reply["path"].as_str()) else { return };
    if let Some(session) = session_from_env(|key| std::env::var(key).ok()) {
        remember(dir, &session, Path::new(path));
    }
}

/// The window's memory of who made which temporary worktree and which sections
/// are open. Neither is persisted: a section starts collapsed because these
/// rows are rarely worth looking at.
#[derive(Debug, Default)]
pub(crate) struct Sections {
    owners: HashMap<PathBuf, SessionId>,
    open: HashSet<TemporaryOwner>,
    generation: u64,
}

impl Sections {
    pub(crate) fn claim(&mut self, path: PathBuf, session: SessionId) {
        self.owners.insert(path, session);
        self.generation += 1;
    }

    pub(crate) fn is_open(&self, owner: &TemporaryOwner) -> bool {
        self.open.contains(owner)
    }

    pub(crate) fn set_open(&mut self, owner: &TemporaryOwner, open: bool) {
        let changed = if open { self.open.insert(owner.clone()) } else { self.open.remove(owner) };
        if changed {
            self.generation += 1;
        }
    }

    /// Advances when a claim or an open section moves the rows.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Where `wt` hangs and what its section is keyed by: under the session
    /// that made it while that session's row is on one of this project's
    /// checkouts, and otherwise under a checkout. A lone session folds into
    /// its checkout's row, so it has no row of its own to hang anything off.
    fn place(
        &self,
        project: &Project,
        wt: &alacritree_vcs::Checkout,
        session_workspace: &impl Fn(SessionId) -> Option<WorkspaceKey>,
        listed: &ListedRows,
    ) -> Option<(PathBuf, TemporaryOwner)> {
        let claimed = self.owners.get(&wt.path).and_then(|id| {
            let ws = session_workspace(*id)??;
            let host = project.top_level().find(|checkout| checkout.path == ws)?;
            Some((*id, host.path.clone()))
        });
        Some(match claimed {
            Some((id, host)) => {
                let key = Some(host.clone());
                let has_row = listed
                    .get(&key)
                    .is_some_and(|entries| entries.iter().any(|e| e.session() == Some(id)));
                let owner = if has_row { TemporaryOwner::Session(id) } else { host.clone().into() };
                (host, owner)
            },
            None => {
                let host = project.host_of(wt)?.path.clone();
                (host.clone(), host.into())
            },
        })
    }

    /// Add each project's sections to the rows its checkouts list: right after
    /// the session that owns one, or after everything else on the checkout. A
    /// section holding the workspace on screen is open whatever the user last
    /// did, or that workspace would have no row.
    pub(crate) fn list_into(
        &self,
        projects: &[Project],
        session_workspace: impl Fn(SessionId) -> Option<WorkspaceKey>,
        current: Option<&Path>,
        listed: &mut ListedRows,
    ) {
        for project in projects {
            let mut sections: Vec<(PathBuf, TemporarySection)> = Vec::new();
            for wt in project.checkouts.iter().filter(|wt| project.nests(wt)) {
                let Some((host, owner)) = self.place(project, wt, &session_workspace, listed)
                else {
                    continue;
                };
                match sections.iter_mut().find(|(_, section)| section.owner == owner) {
                    Some((_, section)) => section.worktrees.push(wt.path.clone()),
                    None => {
                        let worktrees = vec![wt.path.clone()];
                        sections.push((host, TemporarySection { owner, open: false, worktrees }));
                    },
                }
            }
            for (host, mut section) in sections {
                section.open = self.is_open(&section.owner)
                    || section.worktrees.iter().any(|path| Some(path.as_path()) == current);
                let entries = listed.entry(Some(host)).or_default();
                let after_owner = match section.owner {
                    TemporaryOwner::Session(id) => {
                        entries.iter().position(|e| e.session() == Some(id)).map(|at| at + 1)
                    },
                    TemporaryOwner::Checkout(_) => None,
                };
                let at = after_owner.unwrap_or(entries.len());
                entries.insert(at, WorkspaceEntry::Temporary(section));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alacritree_tasks::scope::Harness;
    use alacritree_vcs::{Checkout, Head};

    use super::*;
    use crate::sidebar_nav::tests::project;

    fn temp(name: &str) -> PathBuf {
        PathBuf::from(format!("/wt/repo-1/{DIR}/feature/{name}"))
    }

    fn checkout(path: PathBuf) -> Checkout {
        Checkout {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            path,
            head: Head::default(),
            is_main: false,
            gone: false,
            upstream: None,
        }
    }

    /// `/a` is the main checkout and `/wt/repo-1/feature` a linked one.
    fn repo(temporaries: &[&str]) -> Vec<Project> {
        let mut project = project("/a", true, &["/a", "/wt/repo-1/feature"]);
        project.checkouts[0].is_main = true;
        project.checkouts.extend(temporaries.iter().map(|name| checkout(temp(name))));
        vec![project]
    }

    fn agent() -> SessionRef {
        SessionRef { harness: Harness::Claude, id: "k1".into() }
    }

    fn feature() -> PathBuf {
        PathBuf::from("/wt/repo-1/feature")
    }

    fn sections_in(listed: &ListedRows, ws: &Path) -> Vec<TemporarySection> {
        listed
            .get(&Some(ws.to_path_buf()))
            .into_iter()
            .flatten()
            .filter_map(|entry| match entry {
                WorkspaceEntry::Temporary(section) => Some(section.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_the_marked_directory_makes_a_checkout_temporary() {
        assert_eq!(origin_name(&temp("x")), Some(OsStr::new("feature")));
        assert!(!is_temporary(Path::new("/wt/repo-1/feature")));
        assert!(!is_temporary(Path::new("/wt/repo-1/.temporary")));
        assert!(!is_temporary(Path::new("/")));
    }

    #[test]
    fn a_normal_create_reply_carries_no_cleanup_note() {
        let reply = created_reply(Path::new("/wt/repo-1/feature"), vec![]);
        assert!(reply.get("cleanup").is_none());
        let reply = created_reply(&temp("x"), vec![]);
        assert!(reply["cleanup"].as_str().unwrap().contains("alacritree worktree remove"));
    }

    #[test]
    fn a_removed_worktree_drops_off_the_list_and_a_repeat_is_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, removed) = (dir.path().join("kept"), dir.path().join("removed"));
        std::fs::create_dir(&kept).unwrap();
        std::fs::create_dir(&removed).unwrap();
        remember(dir.path(), &agent(), &kept);
        remember(dir.path(), &agent(), &kept);
        remember(dir.path(), &agent(), &removed);
        assert_eq!(still_open(dir.path(), &agent()), [kept.clone(), removed.clone()]);

        std::fs::remove_dir(&removed).unwrap();
        assert_eq!(still_open(dir.path(), &agent()), [kept]);
    }

    #[test]
    fn another_agent_never_sees_a_list() {
        let dir = tempfile::tempdir().unwrap();
        remember(dir.path(), &agent(), dir.path());
        let other = SessionRef { harness: Harness::Codex, id: "k1".into() };
        assert!(still_open(dir.path(), &other).is_empty());
    }

    #[test]
    fn with_no_claim_a_worktree_hangs_under_the_checkout_it_came_from() {
        let projects = repo(&["x"]);
        let mut listed = ListedRows::new();
        Sections::default().list_into(&projects, |_| None, None, &mut listed);
        let section = &sections_in(&listed, &feature())[0];
        assert_eq!(section.owner, TemporaryOwner::Checkout(feature()));
        assert_eq!(section.worktrees, [temp("x")]);
        assert!(!section.open, "a section starts collapsed");
    }

    /// An origin that has been removed must not hide the worktree.
    #[test]
    fn a_gone_origin_falls_back_to_the_main_checkout() {
        let mut projects = repo(&["x"]);
        projects[0].checkouts.remove(1);
        let mut listed = ListedRows::new();
        Sections::default().list_into(&projects, |_| None, None, &mut listed);
        assert_eq!(sections_in(&listed, Path::new("/a")).len(), 1);
    }

    #[test]
    fn a_claimed_worktree_sits_right_after_its_sessions_row() {
        let projects = repo(&["x", "y"]);
        let mut sections = Sections::default();
        sections.claim(temp("x"), 7);
        sections.claim(temp("y"), 7);
        let mut listed = ListedRows::from([(Some(feature()), vec![
            WorkspaceEntry::Session(7),
            WorkspaceEntry::Session(8),
        ])]);
        sections.list_into(&projects, |_| Some(Some(feature())), None, &mut listed);

        let entries = &listed[&Some(feature())];
        assert_eq!(entries[0], WorkspaceEntry::Session(7));
        assert!(matches!(&entries[1], WorkspaceEntry::Temporary(section)
            if section.owner == TemporaryOwner::Session(7) && section.worktrees.len() == 2));
        assert_eq!(entries[2], WorkspaceEntry::Session(8));
    }

    /// One session folds into its checkout's row, so there is no row to hang
    /// the section from.
    #[test]
    fn a_session_with_no_row_of_its_own_leaves_the_section_on_its_checkout() {
        let projects = repo(&["x"]);
        let mut sections = Sections::default();
        sections.claim(temp("x"), 7);
        let mut listed = ListedRows::new();
        sections.list_into(&projects, |_| Some(Some(feature())), None, &mut listed);
        assert_eq!(sections_in(&listed, &feature())[0].owner, TemporaryOwner::Checkout(feature()));
    }

    #[test]
    fn a_closed_session_gives_its_worktrees_back_to_their_origin() {
        let projects = repo(&["x"]);
        let mut sections = Sections::default();
        sections.claim(temp("x"), 7);
        let mut listed = ListedRows::new();
        sections.list_into(&projects, |_| None, None, &mut listed);
        assert_eq!(sections_in(&listed, &feature())[0].owner, TemporaryOwner::Checkout(feature()));
    }

    /// A session in Home, or in another project, is no place to file this
    /// project's worktree.
    #[test]
    fn a_session_outside_the_project_does_not_claim_the_row() {
        let projects = repo(&["x"]);
        let mut sections = Sections::default();
        sections.claim(temp("x"), 7);
        let mut listed = ListedRows::from([(None, vec![WorkspaceEntry::Session(7)])]);
        sections.list_into(&projects, |_| Some(None), None, &mut listed);
        assert_eq!(listed[&None], [WorkspaceEntry::Session(7)]);
        assert_eq!(sections_in(&listed, &feature()).len(), 1);
    }

    #[test]
    fn the_workspace_on_screen_opens_its_section_without_recording_it() {
        let projects = repo(&["x"]);
        let sections = Sections::default();
        let mut listed = ListedRows::new();
        sections.list_into(&projects, |_| None, Some(&temp("x")), &mut listed);
        let section = &sections_in(&listed, &feature())[0];
        assert!(section.open);
        assert!(!sections.is_open(&section.owner));
    }

    #[test]
    fn only_a_real_change_moves_the_generation() {
        let mut sections = Sections::default();
        let owner = TemporaryOwner::Session(1);
        sections.set_open(&owner, false);
        assert_eq!(sections.generation(), 0);
        sections.set_open(&owner, true);
        sections.set_open(&owner, true);
        assert_eq!(sections.generation(), 1);
    }
}
