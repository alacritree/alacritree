//! The context events of `alacritree hook`. `session-start` and
//! `user-prompt-submit` add to the model's context, and `stop` asks an agent
//! to keep going when it still holds temporary worktrees. They never fail a
//! turn: a failure leaves out what it would have added and exits 0, and the
//! only thing they print is one JSON object for the harness.

use std::path::{Path, PathBuf};

use alacritree_common::jobs;
use alacritree_tasks::scope::{GLOBAL, Harness, Place, SessionRef, node, sanitize};
use alacritree_tasks::{Filter, NodeMatch, Status, Task, TaskBackend, tree};
use pabal::{AddContext, AnyPayload, AnyView, Fields};

use crate::digest::stable_digest;
use crate::tasks::facts;
use crate::temporary;

/// The agent's own session and every scope above it. Other agents' lists
/// stay out so none picks up another's work.
fn visible_nodes(place: &Place, session: Option<&SessionRef>) -> Vec<String> {
    let mut nodes = vec![GLOBAL.to_string()];
    let repo = match place {
        Place::Global => None,
        Place::Project { repo } | Place::Workspace { repo, .. } => Some(repo.clone()),
    };
    nodes.extend(repo.map(|repo| node(&Place::Project { repo }, None)));
    if let Place::Workspace { .. } = place {
        nodes.push(node(place, None));
        if session.is_some() {
            nodes.push(node(place, session));
        }
    }
    nodes
}

pub(crate) fn context(
    backend: &impl TaskBackend,
    place: &Place,
    session: Option<&SessionRef>,
    tasks: &[Task],
) -> String {
    let mut text = backend.agent_guide(&node(place, session));
    for scope in visible_nodes(place, session).iter().rev() {
        let in_scope: Vec<&Task> = tasks.iter().filter(|t| t.node() == scope).collect();
        if in_scope.is_empty() {
            continue;
        }
        text.push_str(&format!("\n## {scope}\n"));
        for row in tree::rows(&in_scope) {
            let mark = if row.status == Status::Completed { "x" } else { " " };
            let started = if row.started { " (in progress)" } else { "" };
            let short = row.id.get(..8).unwrap_or(&row.id);
            let indent = "  ".repeat(row.depth);
            text.push_str(&format!("{indent}- [{mark}] {}{started} ({short})\n", row.text));
        }
    }
    text
}

fn digest_path(state_dir: &Path, session: &SessionRef) -> PathBuf {
    let name = format!("{}-{}.digest", session.harness.prefix(), sanitize(&session.id));
    state_dir.join("task-hooks").join(name)
}

/// Ask an agent that stopped to deal with the temporary worktrees it still
/// holds, once for each set of them, so one it means to keep is not nagged
/// about at every stop. The record is written first: without it the agent
/// would be sent back at every stop, with nothing able to break the loop.
///
/// Both harnesses read this shape, and pabal has no builder for it.
fn stop_response(state_dir: &Path, session: &SessionRef) -> Option<String> {
    let open = temporary::still_open(state_dir, session);
    if open.is_empty() {
        return None;
    }
    let reason = temporary::stop_prompt(&open);
    let path = digest_path(state_dir, session).with_extension("stop-digest");
    let digest = format!("{:016x}", stable_digest(reason.as_bytes()));
    if std::fs::read_to_string(&path).is_ok_and(|seen| seen == digest) {
        return None;
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, digest).ok()?;
    Some(serde_json::json!({ "decision": "block", "reason": reason }).to_string())
}

/// `None` means print nothing, which is where every failure lands. `cwd` is
/// the payload's, already on this host.
pub(crate) fn run(
    backend: &impl TaskBackend,
    payload: &AnyPayload,
    harness: Harness,
    cwd: &Path,
    state_dir: Option<&Path>,
    backends: &[crate::vcs::Vcs],
) -> Option<String> {
    let view = payload.view();
    let session = payload
        .session_id()
        .filter(|id| !id.trim().is_empty())
        .map(|id| SessionRef { harness, id: id.to_string() });
    if matches!(view, AnyView::Stop(_)) {
        return stop_response(state_dir?, session.as_ref()?);
    }
    if !matches!(view, AnyView::SessionStart(_) | AnyView::UserPromptSubmit(_)) {
        return None;
    }
    let (place, tasks) = jobs::on_this_thread(|b| {
        let (side, place) = facts::place_for(cwd, backends, b);
        let nodes = visible_nodes(&place, session.as_ref()).into_iter().map(NodeMatch::Exact);
        (place, backend.list(&side, &Filter { nodes: nodes.collect() }, b))
    });
    let mut text = match tasks {
        Ok(tasks) => context(backend, &place, session.as_ref(), &tasks),
        // No task store is no reason to withhold the rest.
        Err(_) => String::new(),
    };
    // Outside a repository there is nothing to make a worktree of.
    if !matches!(place, Place::Global) {
        let open = match (state_dir, session.as_ref()) {
            (Some(dir), Some(session)) => temporary::still_open(dir, session),
            _ => Vec::new(),
        };
        text.push_str(&temporary::agent_guide(&open));
    }
    if text.is_empty() {
        return None;
    }
    // Session start records what it showed too, so the first prompt after it
    // does not repeat an unchanged list.
    if let (Some(dir), Some(session)) = (state_dir, session.as_ref()) {
        let path = digest_path(dir, session);
        let digest = format!("{:016x}", stable_digest(text.as_bytes()));
        let seen = std::fs::read_to_string(&path).is_ok_and(|s| s == digest);
        if seen && matches!(view, AnyView::UserPromptSubmit(_)) {
            return None;
        }
        if !seen {
            let _ = path.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(&path, digest);
        }
    }
    let response = match view {
        AnyView::SessionStart(start) => Some(start.add_context(&text)),
        AnyView::UserPromptSubmit(prompt) => prompt.add_context(&text),
        _ => None,
    };
    response.map(|r| r.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritree_tasks::fake::FakeBackend;

    fn task(id: &str, project: &str, text: &str, status: Status) -> Task {
        Task {
            description: text.into(),
            status,
            order: Some(1024),
            ..alacritree_tasks::fake::task(id, project)
        }
    }

    #[test]
    fn context_lists_each_visible_scope_and_names_the_write_target() {
        let place = Place::Workspace { repo: "r".into(), branch: "main".into() };
        let me = SessionRef { harness: Harness::Codex, id: "s1".into() };
        let tasks = [
            task("11111111-aaaa", "global", "global chore", Status::Pending),
            task("22222222-bbbb", "r.main", "ship it", Status::Completed),
            task("33333333-cccc", "r.main.codex-s1", "my step", Status::Pending),
            task("44444444-dddd", "r.main.claude-other", "not mine", Status::Pending),
        ];
        let text = context(&FakeBackend::default(), &place, Some(&me), &tasks);
        assert!(text.contains("`r.main.codex-s1`"));
        assert!(text.contains("- [ ] my step (33333333)"));
        assert!(text.contains("- [x] ship it (22222222)"));
        assert!(text.contains("global chore"));
        assert!(!text.contains("not mine"), "other agents' lists stay out");
    }

    fn stop_payload(state: &Path) -> Option<String> {
        let stdin = r#"{"session_id":"k1","cwd":"/nowhere","hook_event_name":"Stop"}"#;
        let payload =
            AnyPayload::parse_named(pabal::AnyHarness::ClaudeCode, "Stop", stdin).unwrap();
        let backend = FakeBackend::default();
        run(&backend, &payload, Harness::Claude, Path::new("/nowhere"), Some(state), &[])
    }

    fn agent() -> SessionRef {
        SessionRef { harness: Harness::Claude, id: "k1".into() }
    }

    /// Put `path` on the agent's list the way a create does.
    fn hold(state: &Path, path: &Path) {
        let ledger = state.join("task-hooks").join("claude-k1.temporary");
        std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        let mut text = std::fs::read_to_string(&ledger).unwrap_or_default();
        text.push_str(&format!("{}\n", path.display()));
        std::fs::write(ledger, text).unwrap();
    }

    #[test]
    fn stopping_with_nothing_open_says_nothing() {
        let state = tempfile::tempdir().unwrap();
        assert_eq!(stop_payload(state.path()), None);
    }

    #[test]
    fn stopping_with_a_worktree_open_asks_once_for_each_set_of_them() {
        let state = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        hold(state.path(), first.path());

        let reply = stop_payload(state.path()).expect("the agent is asked");
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["decision"], "block");
        let reason = reply["reason"].as_str().unwrap();
        assert!(reason.contains(first.path().to_str().unwrap()), "{reason}");

        // The agent may mean to keep it, and this is what ends the loop.
        assert_eq!(stop_payload(state.path()), None);

        let second = tempfile::tempdir().unwrap();
        hold(state.path(), second.path());
        assert!(stop_payload(state.path()).is_some(), "a new worktree is worth asking about");
    }

    /// Asking with no way to remember having asked would send the agent back
    /// at every stop.
    #[test]
    fn a_stop_it_cannot_record_is_let_through() {
        let state = tempfile::tempdir().unwrap();
        let held = tempfile::tempdir().unwrap();
        hold(state.path(), held.path());
        let digest = digest_path(state.path(), &agent()).with_extension("stop-digest");
        std::fs::create_dir(digest).unwrap();
        assert_eq!(stop_payload(state.path()), None);
    }

    #[test]
    fn without_a_session_the_workspace_is_the_write_target() {
        let place = Place::Workspace { repo: "r".into(), branch: "main".into() };
        assert!(context(&FakeBackend::default(), &place, None, &[]).contains("`r.main`"));
    }
}
