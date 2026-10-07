//! The task events of `alacritree hook`: `session-start` and
//! `user-prompt-submit`. They never block a turn. Every failure prints
//! nothing and exits 0, and the only thing they print is one JSON object the
//! harness adds to the model's context.

use std::path::{Path, PathBuf};

use alacritree_common::jobs;
use alacritree_tasks::scope::{GLOBAL, Harness, Place, SessionRef, node, sanitize};
use alacritree_tasks::{Filter, NodeMatch, Status, Task, TaskBackend, tree};
use pabal::{AddContext, AnyPayload, AnyView, Fields};

use crate::digest::stable_digest;
use crate::tasks::facts;

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
    if !matches!(view, AnyView::SessionStart(_) | AnyView::UserPromptSubmit(_)) {
        return None;
    }
    let session = payload
        .session_id()
        .filter(|id| !id.trim().is_empty())
        .map(|id| SessionRef { harness, id: id.to_string() });
    let (place, tasks) = jobs::on_this_thread(|b| {
        let (side, place) = facts::place_for(cwd, backends, b);
        let nodes = visible_nodes(&place, session.as_ref()).into_iter().map(NodeMatch::Exact);
        let tasks = backend.list(&side, &Filter { nodes: nodes.collect() }, b);
        tasks.map(|tasks| (place, tasks))
    })
    .ok()?;
    let text = context(backend, &place, session.as_ref(), &tasks);
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

    #[test]
    fn without_a_session_the_workspace_is_the_write_target() {
        let place = Place::Workspace { repo: "r".into(), branch: "main".into() };
        assert!(context(&FakeBackend::default(), &place, None, &[]).contains("`r.main`"));
    }
}
