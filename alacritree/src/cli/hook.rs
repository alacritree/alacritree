//! `alacritree hook <event>`: the one command a harness's hook config calls.
//!
//! The task events (`session-start`, `user-prompt-submit`) never block a
//! turn; `tasks::hook` holds them. The worktree events keep every checkout an
//! agent makes alacritree's: `worktree-create` makes Claude Code's worktree
//! the way the sidebar's + button does, `worktree-remove` leaves it for the
//! sidebar to delete, and `pre-tool-use` turns away a shell
//! `git worktree add`. `worktree-create` and `worktree-remove` refuse with
//! the reason on stderr and exit 1, since Claude Code then gives the
//! operation up rather than falling back to its own git.

use std::path::{Path, PathBuf};

use alacritree_common::{jobs, wsl};
use alacritree_tasks::scope::Harness;
use alacritree_vcs::{VcsError, VersionControl};
use pabal::{AnyHarness, AnyPayload, AnyView, Deny, Fields, Tool};

use super::ConfigSource;
use crate::ipc::protocol::{IpcRequest, LocalSocket, SendError};

/// The event a hook config names. It fills in a payload that leaves out
/// `hook_event_name`, and a payload that sends one wins.
#[derive(Clone, Copy, Debug, clap::ValueEnum, strum::IntoStaticStr)]
#[strum(serialize_all = "PascalCase")]
pub(crate) enum Event {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    WorktreeCreate,
    WorktreeRemove,
}

/// Why a worktree event refused. Each reaches the harness as stderr.
#[derive(Debug, thiserror::Error)]
enum HookError {
    #[error("the hook payload is not a JSON object")]
    Payload(#[from] pabal::Error),
    #[error("{0} is a Claude Code event")]
    ClaudeOnly(&'static str),
    #[error("the {0} payload has no `{1}`")]
    Missing(&'static str, &'static str),
    #[error(transparent)]
    Vcs(#[from] VcsError),
    #[error(transparent)]
    Send(#[from] SendError),
    #[error("alacritree created the worktree but did not say where")]
    NoPath,
    #[error("alacritree keeps {}; delete it from the sidebar when it is done", .0.display())]
    Kept(PathBuf),
}

const DENY_WORKTREE_ADD: &str = "alacritree manages the worktrees here, so `git worktree add` is \
                                 turned away. Run `alacritree worktree create <project-root> \
                                 <branch>` (or the `create_worktree` MCP tool) instead: it \
                                 branches off the default branch, copies the LLM config files \
                                 and shows the worktree in the sidebar.";

pub(super) fn run(
    event: Event,
    harness: &str,
    socket: Option<&Path>,
    config: ConfigSource<'_>,
) -> i32 {
    let mut stdin = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin);
    let harness = Harness::parse(harness).expect("clap restricts --harness to known names");
    let kind = match harness {
        Harness::Claude => AnyHarness::ClaudeCode,
        Harness::Codex => AnyHarness::Codex,
    };
    let payload = AnyPayload::parse_named(kind, event.into(), &stdin);
    let printed = match event {
        // A harness waits on this before the model sees the turn, so it
        // answers from disk with no window and never fails the hook.
        Event::SessionStart | Event::UserPromptSubmit => {
            payload.ok().and_then(|payload| tasks(&payload, harness, config))
        },
        // Runs before every shell command, so it reads no config and lets
        // anything it cannot parse through.
        Event::PreToolUse => payload.ok().and_then(|payload| guard(&payload)),
        Event::WorktreeCreate => {
            let created = payload.map_err(HookError::from);
            match created.and_then(|payload| create(&payload, socket, config)) {
                Ok(path) => Some(path),
                Err(e) => return refuse(&e),
            }
        },
        Event::WorktreeRemove => {
            return refuse(&payload.map_or_else(HookError::from, |payload| kept(&payload)));
        },
    };
    if let Some(out) = printed {
        println!("{out}");
    }
    0
}

fn refuse(error: &HookError) -> i32 {
    eprintln!("alacritree: {error}");
    1
}

fn tasks(payload: &AnyPayload, harness: Harness, config: ConfigSource<'_>) -> Option<String> {
    let cwd = harness_cwd(payload)?;
    let integrations = super::task::configure_tools(config.dir, config.overrides);
    let state_dir = config.dir.map(Path::to_path_buf).or_else(crate::state::config_dir);
    let backend = crate::tasks::backend::Backend::from_config(&integrations);
    let backends = crate::vcs::backends(&integrations);
    crate::tasks::hook::run(&backend, payload, harness, &cwd, state_dir.as_deref(), &backends)
}

/// The deny for a shell command that adds a worktree, or `None` to leave the
/// call to the harness's own permission flow.
fn guard(payload: &AnyPayload) -> Option<String> {
    let AnyView::PreToolUse(pre) = payload.view() else { return None };
    let Some(Tool::Shell { command, .. }) = payload.tool() else { return None };
    adds_worktree(command).then(|| pre.deny(DENY_WORKTREE_ADD).to_string())
}

/// Create the worktree through a running alacritree, or the offline path
/// when none is listening, exactly as `alacritree worktree create` would.
/// The branch is the name Claude Code chose, and the project is the one the
/// session's cwd belongs to, whichever of its checkouts that is.
fn create(
    payload: &AnyPayload,
    socket: Option<&Path>,
    config: ConfigSource<'_>,
) -> Result<String, HookError> {
    let event: &'static str = Event::WorktreeCreate.into();
    if payload.harness() != AnyHarness::ClaudeCode {
        return Err(HookError::ClaudeOnly(event));
    }
    let name = payload.raw()["name"].as_str().filter(|name| !name.trim().is_empty());
    let name = name.ok_or(HookError::Missing(event, "name"))?;
    let cwd = harness_cwd(payload).ok_or(HookError::Missing(event, "cwd"))?;
    let integrations = super::task::configure_tools(config.dir, config.overrides);
    let backends = crate::vcs::backends(&integrations);
    let located = jobs::on_this_thread(|b| backends.iter().find_map(|vcs| vcs.locate(&cwd, b)));
    let main = located.ok_or_else(|| VcsError::NotARepository(cwd.clone()))?.main;
    let request = IpcRequest::CreateWorktree { project_root: main, branch: name.to_string() };
    let reply = super::dispatch(&request, &LocalSocket(socket), config)?;
    let path = reply["path"].as_str().ok_or(HookError::NoPath)?;
    // A harness inside WSL reads the path in its own form.
    if linux_cwd(payload) {
        wsl::windows_to_linux(Path::new(path)).ok_or(HookError::NoPath)
    } else {
        Ok(path.to_string())
    }
}

/// Claude Code calls this to remove a worktree its create hook made. Removing
/// it here would pull the directory out from under any session the sidebar
/// has open in it, so the worktree stays until the sidebar deletes it.
fn kept(payload: &AnyPayload) -> HookError {
    let event: &'static str = Event::WorktreeRemove.into();
    if payload.harness() != AnyHarness::ClaudeCode {
        return HookError::ClaudeOnly(event);
    }
    match payload.raw()["worktree_path"].as_str().filter(|path| !path.trim().is_empty()) {
        Some(path) => HookError::Kept(PathBuf::from(path)),
        None => HookError::Missing(event, "worktree_path"),
    }
}

/// The payload's cwd on this host, or this process's own.
fn harness_cwd(payload: &AnyPayload) -> Option<PathBuf> {
    let here = std::env::current_dir().ok();
    match payload.cwd() {
        Some(cwd) => on_this_host(cwd.to_path_buf(), here),
        None => here,
    }
}

fn linux_cwd(payload: &AnyPayload) -> bool {
    cfg!(windows) && payload.cwd().and_then(Path::to_str).is_some_and(|s| s.starts_with('/'))
}

/// A harness inside WSL reports a Linux cwd, while this Windows process
/// starts in the same directory's `\\wsl.localhost` form. The distro comes
/// from there, since WSL passes no variable naming it to Windows processes.
fn on_this_host(cwd: PathBuf, here: Option<PathBuf>) -> Option<PathBuf> {
    let linux = cfg!(windows) && cwd.to_str().is_some_and(|s| s.starts_with('/'));
    if !linux {
        return Some(cwd);
    }
    match here.as_deref().map(wsl::classify) {
        Some(wsl::Location::Wsl { distro, .. }) => {
            Some(wsl::linux_to_windows(cwd.to_str()?, &distro))
        },
        _ => here,
    }
}

/// Whether a shell command runs `git worktree add` anywhere in its lists and
/// pipelines, including behind `sudo`, `env` or a variable assignment. Words
/// split as a shell splits them, so the phrase inside a quoted argument is
/// not a call. A script, an alias or a `sh -c` string still gets through:
/// this catches the call an agent types, not every way to reach git.
fn adds_worktree(command: &str) -> bool {
    commands(command).iter().any(|words| {
        words.iter().enumerate().any(|(i, word)| is_git(word) && worktree_add(&words[i + 1..]))
    })
}

fn is_git(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name == "git" || name.eq_ignore_ascii_case("git.exe")
}

/// Whether the words after `git` are its global options, then `worktree add`.
fn worktree_add(args: &[String]) -> bool {
    let mut args = args.iter().map(String::as_str);
    let mut subcommands = std::iter::from_fn(|| {
        while let Some(arg) = args.next() {
            match arg {
                // The global options whose value is the next word.
                "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                    args.next();
                },
                _ if arg.starts_with('-') => {},
                _ => return Some(arg),
            }
        }
        None
    });
    subcommands.next() == Some("worktree") && subcommands.next() == Some("add")
}

/// The words of each simple command in `line`. Quotes and backslashes group
/// as in a POSIX shell. Every unquoted `;`, `&`, `|`, parenthesis, backtick
/// or newline ends a command, so `$(git worktree add x)` is a command of its
/// own.
fn commands(line: &str) -> Vec<Vec<String>> {
    let mut commands = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => word.get_or_insert_default().extend(chars.next()),
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            },
            (None, '\\') => word.get_or_insert_default().extend(chars.next()),
            (None, ';' | '&' | '|' | '(' | ')' | '`' | '\n') => {
                commands.last_mut().unwrap().extend(word.take());
                commands.push(Vec::new());
            },
            (None, c) if c.is_whitespace() => commands.last_mut().unwrap().extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    commands.last_mut().unwrap().extend(word);
    commands.retain(|words| !words.is_empty());
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_the_wire_names() {
        let names: [&str; 5] = [
            Event::SessionStart.into(),
            Event::UserPromptSubmit.into(),
            Event::PreToolUse.into(),
            Event::WorktreeCreate.into(),
            Event::WorktreeRemove.into(),
        ];
        let wire =
            ["SessionStart", "UserPromptSubmit", "PreToolUse", "WorktreeCreate", "WorktreeRemove"];
        assert_eq!(names, wire);
    }

    #[test]
    fn a_worktree_add_is_caught_wherever_it_sits() {
        for command in [
            "git worktree add ../topic",
            "git -C /repo worktree add -b topic ../topic",
            "git -c core.hooksPath=/dev/null worktree add x",
            "git --git-dir=/repo/.git worktree add x",
            "cd /repo && git worktree add x",
            "git fetch; git worktree add x origin/main",
            "echo $(git worktree add x)",
            "sudo /usr/bin/git worktree add x",
            "GIT_DIR=/repo/.git git worktree add x",
            "git -C 'my repo' worktree add x",
            "git worktree add x\n",
        ] {
            assert!(adds_worktree(command), "{command:?}");
        }
    }

    #[test]
    fn other_git_and_quoted_text_pass() {
        for command in [
            "git worktree list",
            "git worktree remove x",
            "git status",
            "git commit -m 'git worktree add is turned away'",
            "echo \"git worktree add\"",
            "alacritree worktree create . topic",
            "rg 'worktree add' src",
            "git log -- worktree add",
        ] {
            assert!(!adds_worktree(command), "{command:?}");
        }
    }

    #[test]
    fn words_group_like_a_shell() {
        let words = commands(r#"a "b c"'d' e\ f | g;h"#);
        let expected: Vec<Vec<String>> =
            vec![vec!["a".into(), "b cd".into(), "e f".into()], vec!["g".into()], vec!["h".into()]];
        assert_eq!(words, expected);
    }

    fn claude(event: &str, json: &str) -> AnyPayload {
        AnyPayload::parse_named(AnyHarness::ClaudeCode, event, json).unwrap()
    }

    #[test]
    fn only_a_shell_worktree_add_is_denied() {
        let bash = |command: &str| {
            let json = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "command": command },
            });
            claude("PreToolUse", &json.to_string())
        };
        let denied = guard(&bash("git worktree add ../x")).expect("a deny");
        let denied: serde_json::Value = serde_json::from_str(&denied).unwrap();
        assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(guard(&bash("git worktree list")).is_none());
        let read = r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{}}"#;
        assert!(guard(&claude("PreToolUse", read)).is_none());
    }

    #[test]
    fn a_remove_keeps_the_worktree_and_names_it() {
        let json = r#"{"hook_event_name":"WorktreeRemove","worktree_path":"/w/topic"}"#;
        let named = kept(&claude("WorktreeRemove", json));
        assert!(matches!(&named, HookError::Kept(path) if path == Path::new("/w/topic")));
        let unnamed = kept(&claude("WorktreeRemove", "{}"));
        assert!(matches!(unnamed, HookError::Missing(_, "worktree_path")));
    }

    #[test]
    fn worktree_events_are_claude_code_only() {
        let json = r#"{"worktree_path":"/w"}"#;
        let codex = AnyPayload::parse_named(AnyHarness::Codex, "WorktreeRemove", json).unwrap();
        assert!(matches!(kept(&codex), HookError::ClaudeOnly("WorktreeRemove")));
    }
}
