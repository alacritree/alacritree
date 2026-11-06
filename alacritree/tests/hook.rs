//! `alacritree hook` as a harness runs it: a fresh process, a payload on
//! stdin, and what the harness reads on stdout. The task and tool events
//! print JSON or nothing and exit 0 whatever happens; the worktree events
//! print a path or fail.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use alacritree_common::command_ext::hidden;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_alacritree")
}

// A test has no UI thread for a blocking wait to stall.
#[allow(clippy::disallowed_methods)]
fn succeeds(program: &str, args: &[&str]) -> bool {
    hidden(program).args(args).output().is_ok_and(|o| o.status.success())
}

/// Runs a setup step that must succeed, and fails with what it printed when
/// it does not. wsl.exe prints its own errors on stdout, in UTF-16 unless
/// `WSL_UTF8` is set.
#[allow(clippy::disallowed_methods)]
fn run(program: &str, args: &[&str]) {
    let out = hidden(program).env("WSL_UTF8", "1").args(args).output().expect("setup runs");
    assert!(
        out.status.success(),
        "{program} {args:?} failed ({}): {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository, a private taskrc and a private config dir, so nothing the
/// developer owns is read or written. Taskwarrior 3 has no Windows build, so
/// a host without a native one keeps the repository and taskrc inside the
/// default distro, where the hook's own side detection sends `task`.
struct Sandbox {
    state: tempfile::TempDir,
    _work: tempfile::TempDir,
    repo: PathBuf,
    env: Vec<(&'static str, String)>,
}

fn with_task() -> Option<Sandbox> {
    let state = tempfile::tempdir().unwrap();
    if succeeds("task", &["--version"]) {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();
        run("git", &["-C", repo.to_str()?, "init", "-q", "-b", "main"]);
        std::fs::write(work.path().join("taskrc"), "").unwrap();
        let root = work.path().to_str()?.to_string();
        let env = vec![("TASKRC", format!("{root}/taskrc")), ("TASKDATA", format!("{root}/data"))];
        return Some(Sandbox { state, _work: work, repo, env });
    }
    let distro = alacritree_common::wsl::distros().into_iter().find(|d| d.is_default)?.name;
    if !succeeds("wsl.exe", &["-d", &distro, "-e", "sh", "-lc", "command -v task"]) {
        return None;
    }
    let tmp = PathBuf::from(format!(r"\\wsl.localhost\{distro}\tmp"));
    let work = tempfile::Builder::new().prefix("alacritree-hook").tempdir_in(tmp).ok()?;
    let root = alacritree_common::wsl::windows_to_linux(work.path())?;
    let repo = work.path().join("myrepo");
    std::fs::create_dir(&repo).unwrap();
    let linux_repo = format!("{root}/myrepo");
    let init = ["-d", &distro, "-e", "git", "-C", &linux_repo, "init", "-q", "-b", "main"];
    run("wsl.exe", &init);
    std::fs::write(work.path().join("taskrc"), "").unwrap();
    let env = vec![
        ("TASKRC", format!("{root}/taskrc")),
        ("TASKDATA", format!("{root}/data")),
        ("WSLENV", "TASKRC:TASKDATA".to_string()),
    ];
    Some(Sandbox { state, _work: work, repo, env })
}

/// A repository on this machine and no taskwarrior settings at all.
fn without_task() -> Sandbox {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let repo = work.path().join("myrepo");
    std::fs::create_dir(&repo).unwrap();
    succeeds("git", &["-C", repo.to_str().unwrap(), "init", "-q", "-b", "main"]);
    Sandbox { state, _work: work, repo, env: Vec::new() }
}

fn fixture(name: &str, cwd: &Path) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook").join(name);
    let raw = std::fs::read_to_string(path).unwrap();
    raw.replace("\"CWD\"", &serde_json::to_string(cwd.to_str().unwrap()).unwrap())
}

fn hook(sandbox: &Sandbox, args: &[&str], stdin: &str) -> Output {
    let home = sandbox.state.path();
    #[allow(clippy::disallowed_methods)]
    let mut child = hidden(binary())
        .args(args)
        .envs(sandbox.env.iter().map(|(k, v)| (*k, v.as_str())))
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("HOME", home)
        .current_dir(&sandbox.repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    #[allow(clippy::disallowed_methods)]
    child.wait_with_output().unwrap()
}

#[test]
fn session_start_prints_one_json_object_for_both_harnesses() {
    for (harness, file) in
        [("claude", "claude-session-start.json"), ("codex", "codex-session-start.json")]
    {
        let Some(sandbox) = with_task() else { return };
        let args = ["hook", "session-start", "--harness", harness];
        let out = hook(&sandbox, &args, &fixture(file, &sandbox.repo));
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("stdout is JSON ({e}): {}", String::from_utf8_lossy(&out.stderr))
        });
        assert_eq!(json["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let ctx = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.contains(&format!("myrepo.main.{harness}-")), "{ctx}");
    }
}

/// A harness inside WSL reports its cwd as a Linux path, and the Windows
/// binary it runs through interop starts in the same directory's UNC form.
#[test]
fn a_linux_cwd_from_inside_wsl_names_the_workspace() {
    let Some(sandbox) = with_task() else { return };
    let Some(linux) = alacritree_common::wsl::windows_to_linux(&sandbox.repo) else { return };
    if !linux.starts_with('/') || !cfg!(windows) {
        return;
    }
    let args = ["hook", "session-start", "--harness", "claude"];
    let out = hook(&sandbox, &args, &fixture("claude-session-start.json", Path::new(&linux)));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ctx = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("myrepo.main.claude-"), "{ctx}");
}

#[test]
fn an_unchanged_list_is_injected_once() {
    for (harness, file) in
        [("claude", "claude-user-prompt-submit.json"), ("codex", "codex-user-prompt-submit.json")]
    {
        let Some(sandbox) = with_task() else { return };
        let args = ["hook", "user-prompt-submit", "--harness", harness];
        let first = hook(&sandbox, &args, &fixture(file, &sandbox.repo));
        assert!(!first.stdout.is_empty(), "the first prompt sees the list");
        let second = hook(&sandbox, &args, &fixture(file, &sandbox.repo));
        assert!(second.status.success());
        assert!(second.stdout.is_empty(), "an unchanged list is not repeated");
    }
}

/// No task store is no reason to withhold what an agent needs to know about
/// worktrees, and the task guide must not appear for a store that is not there.
#[test]
fn a_missing_task_binary_still_explains_temporary_worktrees() {
    let sandbox = without_task();
    let args = [
        "-o",
        "integrations.taskwarrior.path='alacritree-no-such-task-binary'",
        "-o",
        "integrations.taskwarrior.wsl_path='alacritree-no-such-task-binary'",
        "hook",
        "session-start",
        "--harness",
        "claude",
    ];
    let out = hook(&sandbox, &args, &fixture("claude-session-start.json", &sandbox.repo));
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ctx = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("alacritree worktree create --temporary"), "{ctx}");
    assert!(ctx.contains("alacritree worktree remove"), "{ctx}");
    assert!(!ctx.contains("taskwarrior"), "{ctx}");
}

/// Outside a repository there is nothing to make a worktree of, and with no
/// task store either the hook has nothing to say.
#[test]
fn outside_a_repository_a_missing_task_binary_prints_nothing() {
    let sandbox = without_task();
    let outside = tempfile::tempdir().unwrap();
    let args = [
        "-o",
        "integrations.taskwarrior.path='alacritree-no-such-task-binary'",
        "-o",
        "integrations.taskwarrior.wsl_path='alacritree-no-such-task-binary'",
        "hook",
        "session-start",
        "--harness",
        "claude",
    ];
    let out = hook(&sandbox, &args, &fixture("claude-session-start.json", outside.path()));
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
}

/// A configured command answers the hook in taskwarrior's place, guide and
/// lists both.
#[cfg(unix)]
#[test]
fn a_task_command_answers_the_hook() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = without_task();
    let bin = sandbox.state.path().join("todo");
    std::fs::write(&bin, "#!/bin/sh\ncat \"$(dirname \"$0\")/tasks.json\"\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tasks = r#"[{"id":"t1","description":"from the command","status":"pending","project":"myrepo.main"},
        {"id":"t2","description":"another repo","status":"pending","project":"other"}]"#;
    std::fs::write(sandbox.state.path().join("tasks.json"), tasks).unwrap();
    let path = format!("integrations.tasks.command.path='{}'", bin.display());
    let args = [
        "-o",
        "integrations.tasks.command.enabled=true",
        "-o",
        &path,
        "-o",
        "integrations.tasks.command.list=['list']",
        "-o",
        "integrations.tasks.command.agent_guide='Use todo for {project}.'",
        "hook",
        "session-start",
        "--harness",
        "claude",
    ];
    let out = hook(&sandbox, &args, &fixture("claude-session-start.json", &sandbox.repo));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout is JSON ({e}): {}", String::from_utf8_lossy(&out.stderr))
    });
    let ctx = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.starts_with("Use todo for myrepo.main.claude-"), "{ctx}");
    assert!(ctx.contains("from the command"), "{ctx}");
    assert!(!ctx.contains("another repo"), "{ctx}");
}

/// What an agent lives through, on the real binary: make a temporary worktree,
/// get asked about it once on stopping, remove it, and hear no more. `--socket`
/// names nothing, so a developer's running window is never touched.
#[test]
fn stopping_asks_about_a_temporary_worktree_until_it_is_removed() {
    let mut sandbox = without_task();
    let repo = sandbox.repo.to_str().unwrap().to_string();
    let identity = ["-c", "user.name=t", "-c", "user.email=t@example.com"];
    run(
        "git",
        &[&["-C", &repo], &identity[..], &["commit", "-q", "--allow-empty", "-m", "x"]].concat(),
    );
    run("git", &["-C", &repo, "remote", "add", "origin", &repo]);
    sandbox.env.push(("CLAUDE_CODE_SESSION_ID", "k-123".to_string()));
    sandbox.env.push(("CODEX_SESSION_ID", String::new()));
    let dead = sandbox.state.path().join("no-such-socket");
    let dead = dead.to_str().unwrap();
    let stop = format!(
        r#"{{"session_id":"k-123","cwd":{},"hook_event_name":"Stop"}}"#,
        serde_json::to_string(&repo).unwrap()
    );
    let stop_args = ["hook", "stop", "--harness", "claude"];

    assert!(hook(&sandbox, &stop_args, &stop).stdout.is_empty(), "nothing is open yet");

    let args = ["--socket", dead, "worktree", "create", "--temporary", ".", "scratch"];
    let made = hook(&sandbox, &args, "");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let path = String::from_utf8(made.stdout).unwrap().trim().to_string();
    assert!(path.contains(".temporary"), "{path}");
    assert!(
        String::from_utf8_lossy(&made.stderr).contains("worktree remove"),
        "the create itself says how to clean up"
    );

    let asked = hook(&sandbox, &stop_args, &stop);
    let reply: serde_json::Value = serde_json::from_slice(&asked.stdout).unwrap();
    assert_eq!(reply["decision"], "block");
    assert!(reply["reason"].as_str().unwrap().contains(&path));
    assert!(hook(&sandbox, &stop_args, &stop).stdout.is_empty(), "asked once, not every stop");

    let prompt = hook(
        &sandbox,
        &["hook", "user-prompt-submit", "--harness", "claude"],
        &stop.replace("Stop", "UserPromptSubmit"),
    );
    let reply: serde_json::Value = serde_json::from_slice(&prompt.stdout).unwrap();
    let ctx = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains(&path), "the next prompt lists what is still open: {ctx}");

    let removed = hook(&sandbox, &["--socket", dead, "worktree", "remove", &path], "");
    assert!(removed.status.success(), "{}", String::from_utf8_lossy(&removed.stderr));
    assert!(!Path::new(&path).exists());
    assert!(hook(&sandbox, &stop_args, &stop).stdout.is_empty(), "nothing left to ask about");
}

#[test]
fn garbage_stdin_exits_zero() {
    let sandbox = without_task();
    for event in ["session-start", "pre-tool-use"] {
        for stdin in ["", "not json", "{}", "[1,2]"] {
            let out = hook(&sandbox, &["hook", event, "--harness", "codex"], stdin);
            assert!(out.status.success(), "{event} {stdin:?}");
            if !out.stdout.is_empty() {
                serde_json::from_slice::<serde_json::Value>(&out.stdout)
                    .expect("JSON when printed");
            }
        }
    }
}

/// With no window listening, Claude Code's worktree comes from the offline
/// create, under `[workspace] worktree_dir`, and stdout is its path alone.
#[test]
fn worktree_create_prints_the_path_it_made() {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let repo = alacritree_git::test_support::clone_with_origin(work.path());
    let sandbox = Sandbox { state, _work: work, repo, env: Vec::new() };
    // An explicit socket that nothing listens on keeps the developer's own
    // window out of the test.
    let socket = sandbox.state.path().join("no-window.sock");
    let worktrees = sandbox.state.path().join("worktrees");
    let worktree_dir = format!("workspace.worktree_dir='{}'", worktrees.display());
    let args = [
        "--socket",
        socket.to_str().unwrap(),
        "-o",
        &worktree_dir,
        "hook",
        "worktree-create",
        "--harness",
        "claude",
    ];
    let out = hook(&sandbox, &args, &fixture("claude-worktree-create.json", &sandbox.repo));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let path = Path::new(stdout.trim_end());
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    assert!(path.starts_with(&worktrees), "{path:?}");
    assert!(path.join(".git").exists(), "{path:?} is a checkout");
}

/// `worktree-remove` keeps every worktree, so a `claude --worktree` session
/// started again under its old name has to get back in.
#[test]
fn worktree_create_comes_back_to_a_worktree_it_kept() {
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let repo = alacritree_git::test_support::clone_with_origin(work.path());
    let sandbox = Sandbox { state, _work: work, repo, env: Vec::new() };
    let socket = sandbox.state.path().join("no-window.sock");
    let worktree_dir =
        format!("workspace.worktree_dir='{}'", sandbox.state.path().join("worktrees").display());
    let args = [
        "--socket",
        socket.to_str().unwrap(),
        "-o",
        &worktree_dir,
        "hook",
        "worktree-create",
        "--harness",
        "claude",
    ];
    let payload = fixture("claude-worktree-create.json", &sandbox.repo);
    let first = hook(&sandbox, &args, &payload);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let second = hook(&sandbox, &args, &payload);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(
        Path::new(String::from_utf8(first.stdout).unwrap().trim_end()).canonicalize().unwrap(),
        Path::new(String::from_utf8(second.stdout).unwrap().trim_end()).canonicalize().unwrap()
    );
}

#[test]
fn worktree_create_outside_a_repository_fails_with_the_reason() {
    let sandbox = without_task();
    let elsewhere = sandbox.state.path().join("plain");
    std::fs::create_dir(&elsewhere).unwrap();
    let socket = sandbox.state.path().join("no-window.sock");
    let args =
        ["--socket", socket.to_str().unwrap(), "hook", "worktree-create", "--harness", "claude"];
    let out = hook(&sandbox, &args, &fixture("claude-worktree-create.json", &elsewhere));
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a"), "names why");
}

/// A remove fails so Claude Code keeps the worktree rather than falling back
/// to its own `git worktree remove`.
#[test]
fn worktree_remove_keeps_the_worktree() {
    let sandbox = without_task();
    let stdin = r#"{"hook_event_name":"WorktreeRemove","worktree_path":"/w/topic"}"#;
    let out = hook(&sandbox, &["hook", "worktree-remove", "--harness", "claude"], stdin);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("/w/topic"));
}

#[test]
fn pre_tool_use_denies_a_shell_worktree_add_for_both_harnesses() {
    let sandbox = without_task();
    let call = |command: &str| {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "cwd": sandbox.repo,
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string()
    };
    for harness in ["claude", "codex"] {
        let args = ["hook", "pre-tool-use", "--harness", harness];
        let out = hook(&sandbox, &args, &call("git worktree add ../topic"));
        assert!(out.status.success());
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(json["hookSpecificOutput"]["permissionDecision"], "deny", "{harness}");
        let out = hook(&sandbox, &args, &call("git worktree list"));
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "{harness} lets other git through");
    }
}
