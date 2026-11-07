//! `alacritree hook <event>`: the one command a harness's hook config calls.
//!
//! The context events (`session-start`, `user-prompt-submit`, `stop`) never
//! fail a turn; `tasks::hook` holds them. The worktree events keep every
//! checkout an agent makes alacritree's: `worktree-create` makes Claude Code's
//! worktree the way the sidebar's + button does, `worktree-remove` leaves it
//! for the sidebar to delete, and `pre-tool-use` turns away a shell
//! `git worktree add`. `worktree-create` and `worktree-remove` refuse with
//! the reason on stderr and exit 1, since Claude Code then gives the
//! operation up rather than falling back to its own git.

use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::Chars;

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
    Stop,
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
    #[error("alacritree created {}, which a harness inside WSL cannot reach", .0.display())]
    OutsideDistro(PathBuf),
    #[error("alacritree keeps {}; delete it from the sidebar when it is done", .0.display())]
    Kept(PathBuf),
}

const DENY_WORKTREE_ADD: &str =
    "alacritree manages the worktrees here, so `git worktree add` is turned away. Run `alacritree \
     worktree create <project-root> <branch>` (or the `create_worktree` MCP tool) instead: it \
     branches off the default branch, copies the LLM config files and shows the worktree in the \
     sidebar. For throwaway work, run `alacritree worktree create --temporary . <name>` and \
     remove it with `alacritree worktree remove <path>` when you are done.";

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
        Event::SessionStart | Event::UserPromptSubmit | Event::Stop => {
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
    let cwd = harness_cwd(payload);
    let integrations = super::task::configure_tools(config.dir, config.overrides);
    let state_dir = config.dir.map(Path::to_path_buf).or_else(crate::state::config_dir);
    let backend = crate::tasks::backend::Backend::from_config(&integrations);
    let backends = crate::vcs::backends(&integrations);
    let cwd = cwd.as_deref();
    crate::tasks::hook::run(&backend, payload, harness, cwd, state_dir.as_deref(), &backends)
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
/// session's cwd belongs to, whichever of its checkouts that is. A name a linked
/// checkout already has its branch out under comes back to that checkout, since
/// `worktree-remove` keeps every worktree and `claude --worktree` can re-enter
/// one. It is an ordinary worktree: filing a subagent's as temporary, which its
/// `agent-a<hex>` name would allow, also needs `worktree-remove` to remove it.
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
    let located =
        jobs::on_this_thread(|b| backends.iter().find_map(|vcs| Some((vcs, vcs.locate(&cwd, b)?))));
    let (vcs, located) = located.ok_or_else(|| VcsError::NotARepository(cwd.clone()))?;
    let main = located.main;
    let kept = jobs::on_this_thread(|b| vcs.discover(&main, &[], false, b))?
        .checkouts
        .into_iter()
        .find(|c| !c.is_main && !c.gone && c.head.name.as_deref() == Some(name));
    let path = match kept {
        Some(checkout) => checkout.path,
        None => {
            let request = IpcRequest::CreateWorktree {
                project_root: main,
                branch: name.to_string(),
                temporary: false,
                session_id: None,
            };
            let reply = super::dispatch(&request, &LocalSocket(socket), config)?;
            // Stdout is the path alone, and a failed checkout hook shows up here.
            let steps = reply["steps"].as_array().into_iter().flatten();
            for step in steps.filter_map(serde_json::Value::as_str) {
                eprintln!("{step}");
            }
            PathBuf::from(reply["path"].as_str().ok_or(HookError::NoPath)?)
        },
    };
    // A harness inside WSL reads the path in its own form.
    if payload.cwd().is_some_and(wsl::is_linux_path_on_windows) {
        wsl::windows_to_linux(&path).ok_or(HookError::OutsideDistro(path))
    } else {
        Ok(path.display().to_string())
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
        Some(cwd) => wsl::on_this_host(cwd.to_path_buf(), here),
        None => here,
    }
}

/// Whether a shell command runs `git worktree add` anywhere in its lists and
/// pipelines, including behind `sudo`, `env` or a variable assignment. Words
/// split as a shell splits them, so the phrase in a quoted argument, a comment
/// or a heredoc is not a call. A script, an alias or a `sh -c` string still gets
/// through: this catches the call an agent types, not every way to reach git.
fn adds_worktree(command: &str) -> bool {
    commands(command).iter().any(|words| runs_worktree_add(words))
}

/// Commands that run the command named after them.
const WRAPPERS: [&str; 8] = ["sudo", "env", "command", "exec", "nohup", "nice", "time", "xargs"];

/// Whether `words` run `git worktree add`, as opposed to mentioning it: git has
/// to be the command, the first word past any assignments and wrappers.
fn runs_worktree_add(words: &[String]) -> bool {
    let is_prefix = |word: &String| {
        is_assignment(word) || word.starts_with('-') || WRAPPERS.contains(&file_name(word))
    };
    let Some(at) = words.iter().position(|word| !is_prefix(word)) else { return false };
    is_git(&words[at]) && worktree_add(&words[at + 1..])
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    })
}

fn file_name(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

fn is_git(word: &str) -> bool {
    let name = file_name(word);
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
/// own, inside double quotes too. A comment and a heredoc's body hold none.
fn commands(line: &str) -> Vec<Vec<String>> {
    let mut commands = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut quote = None;
    // What closes each nested command, and the quote to go back to after it.
    let mut open: Vec<(char, Option<char>)> = Vec::new();
    let mut heredocs = Vec::new();
    let mut chars = line.chars().peekable();
    let end_command = |commands: &mut Vec<Vec<String>>, word: &mut Option<String>| {
        commands.last_mut().unwrap().extend(word.take());
        commands.push(Vec::new());
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('"'), '$') if chars.peek() == Some(&'(') => {
                chars.next();
                end_command(&mut commands, &mut word);
                open.push((')', quote.take()));
            },
            (Some('"'), '`') => {
                end_command(&mut commands, &mut word);
                open.push(('`', quote.take()));
            },
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some('\n') | None => {},
                Some(next) => {
                    let word = word.get_or_insert_default();
                    if !matches!(next, '$' | '`' | '"' | '\\') {
                        word.push('\\');
                    }
                    word.push(next);
                },
            },
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            },
            (None, '\\') => match chars.next() {
                Some('\n') | None => {},
                // No shell user writes `\g`, and it is how a PowerShell path reads.
                Some(next) if next.is_alphanumeric() => {
                    word.get_or_insert_default().extend(['\\', next]);
                },
                Some(next) => word.get_or_insert_default().push(next),
            },
            (None, '#') if word.is_none() => while chars.next_if(|c| *c != '\n').is_some() {},
            (None, '<') if chars.peek() == Some(&'<') => {
                chars.next();
                // `<<<` is a here-string, whose word is ordinary data.
                if chars.next_if_eq(&'<').is_none() {
                    heredocs.push(heredoc_delimiter(&mut chars));
                }
            },
            (None, '\n') => {
                end_command(&mut commands, &mut word);
                skip_heredocs(&mut chars, &mut heredocs);
            },
            (None, ';' | '&' | '|') => end_command(&mut commands, &mut word),
            (None, ')' | '`') if open.last().is_some_and(|(closer, _)| *closer == c) => {
                end_command(&mut commands, &mut word);
                quote = open.pop().and_then(|(_, quote)| quote);
            },
            (None, '(' | '`') => {
                end_command(&mut commands, &mut word);
                open.push((if c == '(' { ')' } else { '`' }, None));
            },
            (None, ')') => end_command(&mut commands, &mut word),
            (None, c) if c.is_whitespace() => commands.last_mut().unwrap().extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    commands.last_mut().unwrap().extend(word);
    commands.retain(|words| !words.is_empty());
    commands
}

/// What follows a `<<`: the delimiter with any quoting gone, and whether `<<-`
/// lets the closing line be indented with tabs.
fn heredoc_delimiter(chars: &mut Peekable<Chars<'_>>) -> (String, bool) {
    let strip_tabs = chars.next_if_eq(&'-').is_some();
    while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
    let mut delimiter = String::new();
    while let Some(c) =
        chars.next_if(|c| !c.is_whitespace() && !matches!(c, ';' | '&' | '|' | '(' | ')'))
    {
        if !matches!(c, '\'' | '"' | '\\') {
            delimiter.push(c);
        }
    }
    (delimiter, strip_tabs)
}

/// Pass the bodies of the heredocs a line just opened.
fn skip_heredocs(chars: &mut Peekable<Chars<'_>>, heredocs: &mut Vec<(String, bool)>) {
    for (delimiter, strip_tabs) in heredocs.drain(..) {
        while chars.peek().is_some() {
            let line: String = std::iter::from_fn(|| chars.next_if(|c| *c != '\n')).collect();
            chars.next();
            let line = if strip_tabs { line.trim_start_matches('\t') } else { &line };
            if line == delimiter {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_are_the_wire_names() {
        let names: [&str; 6] = [
            Event::SessionStart.into(),
            Event::UserPromptSubmit.into(),
            Event::Stop.into(),
            Event::PreToolUse.into(),
            Event::WorktreeCreate.into(),
            Event::WorktreeRemove.into(),
        ];
        let wire = [
            "SessionStart",
            "UserPromptSubmit",
            "Stop",
            "PreToolUse",
            "WorktreeCreate",
            "WorktreeRemove",
        ];
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
            "env FOO=1 git worktree add x",
            // A substitution runs inside double quotes too.
            "dir=\"$(git worktree add x)\"",
            "echo \"$(git worktree add x)\"",
            "echo \"`git worktree add y`\"",
            // An apostrophe in a comment or heredoc opens no quote.
            "# don't touch this\ngit worktree add x",
            "cat <<EOF\nit's\nEOF\ngit worktree add x",
            // A backslash-newline joins lines.
            "git \\\n  worktree add x",
            "git -C /repo \\\n  worktree add x",
            "git worktree \\\n  add x",
            // A Windows path keeps its separators.
            "\"C:\\Program Files\\Git\\cmd\\git.exe\" worktree add x",
            "C:\\Git\\cmd\\git.exe worktree add x",
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
            // An argument that only mentions it is not a call.
            "echo Run git worktree add first",
            "man git worktree add",
            "git grep git worktree add",
            // Nor is a comment, or a heredoc's body.
            "git status # not git worktree add",
            "# we avoid git worktree add here\ngit status",
            "cat > notes.md <<'EOF'\ngit worktree add ../x\nEOF",
            "cat <<-EOF\n\tgit worktree add x\n\tEOF\ngit status",
            "git commit -m \"$(cat <<'EOF'\nit's not git worktree add\nEOF\n)\"",
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
        let reason = denied["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("--temporary"), "throwaway work has somewhere to go: {reason}");
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
