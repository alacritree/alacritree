//! Create, delete, and prune git worktrees off the UI thread.
//!
//! Creation streams its progress back over an `mpsc` channel as each step
//! starts; deletion and pruning report their single result through a
//! `jobs::Job`. Both submit to the shared pool rather than spawning their
//! own thread.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use alacritree_checkout_hooks::{CheckoutEvent, CheckoutHook, CheckoutHooks};
use alacritree_common::{jobs, wsl};
use alacritree_vcs::{ChangeKind, CreateCheckout, RemoveCheckout, VcsError, VersionControl};

use crate::checkout_hooks::Hook;
use crate::config::{Config, WorkspaceConfig};
use crate::repaint::Repaint;
use crate::temporary;
use crate::vcs::Vcs;

#[derive(Debug)]
pub(crate) enum Progress {
    Step(String),
    Done(Result<PathBuf, WorktreeError>),
}

/// Why creating, removing or pruning a worktree failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorktreeError {
    #[error("worktree create cancelled")]
    Cancelled,
    /// The worker unwound instead of returning. The pool records only that a
    /// panic happened, so a create's step list stops wherever it got to.
    #[error("the background worker panicked")]
    WorkerPanicked,
    /// Only a temporary worktree can be removed without the sidebar's dialog,
    /// so an agent cannot delete a checkout someone is relying on.
    #[error("{} is not a temporary worktree, so only the sidebar can remove it", .0.display())]
    NotTemporary(PathBuf),
    #[error("failed to create {}: {source}", path.display())]
    CreateDir { path: PathBuf, source: io::Error },
    #[error("could not locate home directory")]
    NoHome,
    #[error("could not query WSL home: {0}")]
    WslHome(#[source] wsl::BatchError),
    #[error("could not determine the distro home directory")]
    EmptyWslHome,
    #[error(transparent)]
    Vcs(#[from] VcsError),
}

/// What a create from IPC or the offline CLI reads from config: where the
/// worktree goes, the hooks that run once it exists, and the enabled version
/// control backends.
#[derive(Clone)]
pub(crate) struct CreateConfig {
    pub(crate) workspace: WorkspaceConfig,
    pub(crate) hooks: Vec<Hook>,
    pub(crate) vcs: Vec<crate::vcs::Vcs>,
}

/// No hooks, and the backends a config without `[integrations]` enables.
impl Default for CreateConfig {
    fn default() -> Self {
        Self {
            workspace: WorkspaceConfig::default(),
            hooks: Vec::new(),
            vcs: crate::vcs::backends(&crate::config::IntegrationsConfig::default()),
        }
    }
}

impl CreateConfig {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            workspace: config.workspace.clone(),
            hooks: crate::checkout_hooks::from_config(&config.integrations),
            vcs: crate::vcs::backends(&config.integrations),
        }
    }
}

pub(crate) struct CreateRequest {
    project_root: PathBuf,
    default_branch: Option<String>,
    branch: String,
    /// Base directory to create the worktree under; `None` uses the built-in
    /// `~/.alacritree/worktrees` default.
    base_dir: Option<PathBuf>,
    vcs: Vcs,
    /// Set for a temporary worktree: the directory name of the checkout it is
    /// made from.
    temporary_origin: Option<OsString>,
}

impl CreateRequest {
    /// The location comes from `[workspace]` here rather than from each
    /// caller, so the sidebar, IPC and the offline CLI put a worktree in the
    /// same place.
    pub(crate) fn new(
        project_root: PathBuf,
        default_branch: Option<String>,
        branch: String,
        workspace: &WorkspaceConfig,
        vcs: Vcs,
    ) -> Self {
        let base_dir = workspace.base_dir_for(&project_root);
        Self { project_root, default_branch, branch, base_dir, vcs, temporary_origin: None }
    }

    /// A temporary worktree made from the checkout `inside` sits in. Unlike
    /// [`Self::new`] any path in the repository will do, since an agent knows
    /// where it is working and not necessarily where the main checkout is. One
    /// made from a temporary worktree takes that worktree's origin, because a
    /// row cannot hang under a row that is itself in a section.
    pub(crate) fn temporary(
        inside: &Path,
        branch: String,
        workspace: &WorkspaceConfig,
        vcs: Vcs,
        blocking: &jobs::Blocking,
    ) -> Result<Self, WorktreeError> {
        let not_a_repository = || VcsError::NotARepository(inside.to_path_buf());
        let located = vcs.locate(inside, blocking).ok_or_else(not_a_repository)?;
        let from = located.checkout.as_deref().unwrap_or(&located.main);
        let origin = temporary::origin_name(from).or_else(|| from.file_name());
        let temporary_origin = Some(origin.ok_or_else(not_a_repository)?.to_os_string());
        let mut request = Self::new(located.main, None, branch, workspace, vcs);
        request.temporary_origin = temporary_origin;
        Ok(request)
    }

    /// The repository this create runs against, which a temporary create
    /// worked out for itself.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }
}

/// Run [`create`] on the pool, waking the UI for each step. A worktree
/// create is user-initiated, so it runs at interactive priority. The
/// streamed progress travels over the channel; the returned `Job` carries no
/// result of its own and exists only to be held. Dropping it would cancel
/// the create before it starts.
pub(crate) fn spawn_create<H: CheckoutHook + Send + 'static>(
    req: CreateRequest,
    hooks: Vec<H>,
    repaint: impl Repaint,
) -> (Receiver<Progress>, jobs::Job<()>) {
    let (tx, rx) = mpsc::channel();
    let job = jobs::pool().spawn(jobs::Priority::Interactive, move |blocking| {
        let result = create(
            &req,
            hooks.as_slice(),
            |step| {
                let _ = tx.send(Progress::Step(step.to_string()));
                repaint.wake();
            },
            blocking,
        );
        let _ = tx.send(Progress::Done(result));
        repaint.wake();
    });
    (rx, job)
}

/// Create the worktree on the calling thread, reporting each step as it starts.
///
/// Nothing here needs a window, so callers without one (the CLI, with no
/// running app to talk to) drive this directly through [`jobs::on_this_thread`]
/// rather than through [`spawn_create`].
pub(crate) fn create<H: CheckoutHooks + ?Sized>(
    req: &CreateRequest,
    hooks: &H,
    mut on_step: impl FnMut(&str),
    blocking: &jobs::Blocking,
) -> Result<PathBuf, WorktreeError> {
    let send = &mut on_step;
    // A cancel that lands between children has nothing to kill, so each step
    // asks before starting rather than running for a caller that is gone.
    macro_rules! bail_if_cancelled {
        () => {
            if blocking.cancelled() {
                return Err(WorktreeError::Cancelled);
            }
        };
    }

    let base = req
        .vcs
        .prepare_checkout(&req.project_root, req.default_branch.as_deref(), &mut *send, blocking)
        .map_err(create_error)?;
    let target = pick_worktree_path(req, blocking)?;
    let checkout = CreateCheckout {
        main: req.project_root.clone(),
        target: target.clone(),
        name: req.branch.clone(),
        base,
    };
    req.vcs.create_checkout(&checkout, blocking).map_err(create_error)?;

    bail_if_cancelled!();
    send("Copying LLM configurations…");
    let copied = copy_llm_configs(&req.project_root, &target);
    if copied > 0 {
        send(&format!("Copied {copied} LLM config item(s)"));
    }

    bail_if_cancelled!();
    let event = CheckoutEvent { main: &req.project_root, checkout: &target };
    crate::checkout_hooks::report(hooks.created(&event, blocking), |_, line| send(line));

    Ok(target)
}

/// The backend's cancellation is the app's, so callers see one cancel
/// whichever side noticed it.
fn create_error(error: VcsError) -> WorktreeError {
    match error {
        VcsError::Cancelled { .. } => WorktreeError::Cancelled,
        error => WorktreeError::Vcs(error),
    }
}

/// Worktrees live under `<base>/<project>-<hash>/<branch>`. `base` defaults
/// to `~/.alacritree/worktrees` so worktrees don't clutter the repo's parent
/// directory and stay grouped per app; a configured `workspace.worktree_dir`
/// relocates them. The path hash disambiguates same-named repos in different
/// locations. A temporary worktree goes under `.temporary/<origin>` there,
/// which is the whole of what makes it one: see [`temporary`].
fn pick_worktree_path(
    req: &CreateRequest,
    blocking: &jobs::Blocking,
) -> Result<PathBuf, WorktreeError> {
    let mut parent = project_worktree_dir(&req.project_root, req.base_dir.as_deref(), blocking)?;
    if let Some(origin) = &req.temporary_origin {
        parent = parent.join(temporary::DIR).join(origin);
    }
    std::fs::create_dir_all(&parent)
        .map_err(|source| WorktreeError::CreateDir { path: parent.clone(), source })?;
    let safe_branch: String =
        req.branch.chars().map(|c| if c == '/' || c.is_whitespace() { '-' } else { c }).collect();
    let mut candidate = parent.join(&safe_branch);
    let mut suffix = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{safe_branch}-{suffix}"));
        suffix += 1;
    }
    Ok(candidate)
}

/// Worktrees live under `<base>/<project>-<hash>/`. `base` is the configured
/// `[workspace]` override when set; otherwise `<home>/.alacritree/worktrees`,
/// using the *distro's* home for WSL repos so the worktree stays on the Linux
/// filesystem next to its repo instead of crossing onto 9P-mounted NTFS. The
/// path hash disambiguates same-named repos in different locations.
fn project_worktree_dir(
    repo: &Path,
    base: Option<&Path>,
    blocking: &jobs::Blocking,
) -> Result<PathBuf, WorktreeError> {
    let base = match base {
        Some(dir) => dir.to_path_buf(),
        None => {
            let home = match wsl::classify(repo) {
                wsl::Location::Windows(_) => home::home_dir().ok_or(WorktreeError::NoHome)?,
                wsl::Location::Wsl { distro, .. } => {
                    let stdout = wsl::run_batch(&distro, r#"printf '%s' "$HOME""#, &[], blocking)
                        .map_err(WorktreeError::WslHome)?;
                    let linux_home = String::from_utf8_lossy(&stdout).trim().to_string();
                    if linux_home.is_empty() {
                        return Err(WorktreeError::EmptyWslHome);
                    }
                    wsl::linux_to_windows(&linux_home, &distro)
                },
            };
            home.join(".alacritree").join("worktrees")
        },
    };
    let canonical = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let project_name = canonical
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "project".to_string());

    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish() as u32;

    Ok(base.join(format!("{project_name}-{hash:08x}")))
}

/// Filenames/dirs at the project root that look like AI assistant config.
const LLM_CONFIG_NAMES: &[&str] = &[
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".claude",
    ".clauderc",
    "AGENTS.md",
    ".cursorrules",
    ".cursor",
    ".aider.conf.yml",
    ".aiderignore",
    ".copilot-instructions.md",
    ".github/copilot-instructions.md",
    ".windsurfrules",
    ".roomodes",
    ".roo",
    ".codeium",
    ".continue",
];

fn copy_llm_configs(src_root: &Path, dst_root: &Path) -> usize {
    let mut copied = 0;
    for name in LLM_CONFIG_NAMES {
        let src = src_root.join(name);
        if !src.exists() {
            continue;
        }
        let dst = dst_root.join(name);
        if dst.exists() {
            continue;
        }
        match copy_path(&src, &dst) {
            Ok(()) => copied += 1,
            Err(e) => log::warn!("failed to copy {}: {e}", src.display()),
        }
    }
    copied
}

fn copy_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            // A checkout under a config directory, as `.claude/worktrees/` holds,
            // is not config, and every new worktree would pay for a whole tree.
            if entry.path().join(".git").exists() {
                continue;
            }
            let child_dst = dst.join(entry.file_name());
            copy_path(&entry.path(), &child_dst)?;
        }
        Ok(())
    } else if src.is_file() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    } else {
        Ok(())
    }
}

/// Remove `req.checkout`, or prune it when it is gone, and tell the hooks.
pub(crate) fn delete_worktree<H: CheckoutHooks + ?Sized>(
    vcs: &Vcs,
    req: &RemoveCheckout,
    hooks: &H,
    blocking: &jobs::Blocking,
) -> Result<(), WorktreeError> {
    // Resolve before removal: canonicalize needs the directory to still
    // exist, and the checkout hooks below run after git has deleted it.
    let scope_root = resolve_through_ancestors(&req.checkout.path);
    vcs.remove_checkout(req, blocking)?;
    let event = CheckoutEvent { main: &req.main, checkout: &scope_root };
    crate::checkout_hooks::report(hooks.removed(&event, blocking), |level, line| {
        log::log!(level, "{line} (removed {})", scope_root.display())
    });
    Ok(())
}

/// Remove the temporary worktree `path` sits in, with its branch, and answer
/// with the repository it belonged to. A tree with unsaved work is refused
/// unless `force` says otherwise, the same refusal the sidebar's dialog reads.
/// What the create itself added does not count: a refusal over those files
/// would meet every agent on its first removal and teach it to always force,
/// which would throw away the protection for the work that matters.
pub(crate) fn remove_temporary<H: CheckoutHooks + ?Sized>(
    vcs: &Vcs,
    path: &Path,
    force: bool,
    hooks: &H,
    blocking: &jobs::Blocking,
) -> Result<PathBuf, WorktreeError> {
    let not_temporary = || WorktreeError::NotTemporary(path.to_path_buf());
    let located =
        vcs.locate(path, blocking).ok_or_else(|| VcsError::NotARepository(path.to_path_buf()))?;
    let target = located.checkout.as_deref().ok_or_else(not_temporary)?;
    let repository = vcs.discover(&located.main, &[], false, blocking)?;
    let checkout = repository
        .checkouts
        .into_iter()
        .find(|c| c.path == target && !c.is_main && temporary::is_temporary(&c.path))
        .ok_or_else(not_temporary)?;
    let mut req = RemoveCheckout { main: located.main.clone(), checkout, force, delete_name: true };
    match delete_worktree(vcs, &req, hooks, blocking) {
        Err(WorktreeError::Vcs(VcsError::Unsaved { .. }))
            if holds_only_what_create_added(vcs, target, blocking) =>
        {
            req.force = true;
            delete_worktree(vcs, &req, hooks, blocking)?;
        },
        result => result?,
    }
    Ok(located.main)
}

/// Whether the only changes in `path` are untracked files under the names
/// [`copy_llm_configs`] copies, which is also where the Claude Code hook writes.
fn holds_only_what_create_added(vcs: &Vcs, path: &Path, blocking: &jobs::Blocking) -> bool {
    let Ok(status) = vcs.status(path, None, blocking) else { return false };
    status.staged.as_deref().unwrap_or_default().is_empty()
        && status.working.iter().all(|change| {
            change.kind == ChangeKind::Untracked
                && LLM_CONFIG_NAMES.iter().any(|name| Path::new(&change.path).starts_with(name))
        })
}

/// Canonicalize `path` through its nearest ancestor that still exists. A
/// gone checkout's directory can't be canonicalized, but a symlink above it
/// still resolves to what the path meant while the directory existed.
fn resolve_through_ancestors(path: &Path) -> PathBuf {
    path.ancestors()
        .find_map(|dir| {
            let canonical = std::fs::canonicalize(dir).ok()?;
            let rest = path.strip_prefix(dir).ok()?;
            Some(if rest.as_os_str().is_empty() { canonical } else { canonical.join(rest) })
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// Run [`delete_worktree`] on the pool, waking the window when it finishes.
/// The backend's commands and the checkout hooks are slow enough to stutter
/// paint, so the caller confirms the dialog, hands the work here, and adopts
/// the result (an error to surface, or nothing) from the returned handle. The
/// sidebar row shows a spinner until it lands, so this runs at interactive
/// priority.
pub(crate) fn spawn_delete<H: CheckoutHook + Send + 'static>(
    vcs: Vcs,
    req: RemoveCheckout,
    hooks: Vec<H>,
    repaint: impl Repaint,
) -> jobs::Job<Result<(), WorktreeError>> {
    jobs::pool().spawn(jobs::Priority::Interactive, move |blocking| {
        let result = delete_worktree(&vcs, &req, hooks.as_slice(), blocking);
        repaint.wake();
        result
    })
}

#[cfg(test)]
// Fixtures drive real processes and wait on them; no frame is pending.
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::repaint::Recorder;
    use alacritree_checkout_hooks::fake::{Event, FakeHook};
    use alacritree_git::test_support::{
        add_worktree, branch_exists, clone_with_origin, init_repo, worktree_exists,
    };
    use alacritree_vcs::fake::FakeVcs;

    fn git() -> crate::vcs::Vcs {
        crate::vcs::Vcs::Git(alacritree_git::GitBackend)
    }

    fn live_checkout(path: &Path, branch: &str) -> alacritree_vcs::Checkout {
        alacritree_vcs::Checkout {
            name: branch.into(),
            path: path.to_path_buf(),
            head: alacritree_vcs::Head { name: Some(branch.into()), ..Default::default() },
            is_main: false,
            gone: false,
            upstream: None,
        }
    }

    /// Prune `repo`'s one gone checkout the way the delete dialog does, with
    /// the checkout as discovery reports it.
    fn prune_gone_checkout(repo: &Path, hook: &FakeHook) {
        let discovered =
            jobs::on_this_thread(|b| git().discover(repo, &[], false, b)).expect("discover");
        let checkout = discovered.checkouts.into_iter().find(|c| c.gone).expect("a gone checkout");
        let req =
            RemoveCheckout { main: repo.to_path_buf(), checkout, force: false, delete_name: false };
        jobs::on_this_thread(|b| delete_worktree(&git(), &req, &[hook.clone()][..], b))
            .expect("prune succeeds");
    }

    /// A pruned checkout's tools still hold state for it, such as doppler's
    /// scopes, so the removal hooks run for it as for a live one.
    #[test]
    fn pruning_a_gone_checkout_runs_the_removal_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = init_repo(&tmp.path().join("repo"));
        let wt_path = add_worktree(&repo_dir, "stale");
        let canonical = wt_path.canonicalize().unwrap();
        std::fs::remove_dir_all(&wt_path).unwrap();
        let hook = FakeHook::silent();

        prune_gone_checkout(&repo_dir, &hook);

        assert!(!worktree_exists(&repo_dir, "stale"));
        assert_eq!(hook.events(), [Event::Removed { main: repo_dir, checkout: canonical }]);
    }

    /// Git records the path the worktree was added under, and a symlink in
    /// it would hand hooks a spelling that names no state they keyed by
    /// canonical path.
    #[cfg(unix)]
    #[test]
    fn pruning_resolves_symlinks_in_the_recorded_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let repo_dir = init_repo(&link.join("repo"));
        let wt_path = add_worktree(&repo_dir, "stale");
        let canonical = wt_path.canonicalize().unwrap();
        std::fs::remove_dir_all(&wt_path).unwrap();
        let hook = FakeHook::silent();

        prune_gone_checkout(&repo_dir, &hook);

        assert_eq!(hook.events(), [Event::Removed { main: repo_dir, checkout: canonical }]);
    }

    fn abs(tail: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\{tail}"))
        } else {
            PathBuf::from(format!("/{tail}"))
        }
    }

    #[test]
    fn base_dir_replaces_default_worktree_parent() {
        let base = abs("wt-base");
        let dir = jobs::on_this_thread(|b| project_worktree_dir(Path::new("repo"), Some(&base), b))
            .unwrap();
        assert!(dir.starts_with(&base), "{} not under {}", dir.display(), base.display());
        let leaf = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(leaf.starts_with("repo-"), "leaf {leaf:?} should keep <project>-<hash> layout");
    }

    #[test]
    fn no_base_dir_falls_back_to_home_default() {
        let dir =
            jobs::on_this_thread(|b| project_worktree_dir(Path::new("repo"), None, b)).unwrap();
        let expected = home::home_dir().unwrap().join(".alacritree").join("worktrees");
        assert!(dir.starts_with(&expected), "{} not under {}", dir.display(), expected.display());
    }

    #[test]
    fn spawn_delete_removes_a_live_worktree_off_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let repo = init_repo(&repo_dir);
        let wt_path = add_worktree(&repo, "feature");
        assert!(wt_path.is_dir());

        let req = RemoveCheckout {
            main: repo_dir,
            checkout: live_checkout(&wt_path, "feature"),
            force: false,
            delete_name: true,
        };
        let repaint = Recorder::default();
        let handle = spawn_delete(git(), req, Vec::<FakeHook>::new(), repaint.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            if let Some(result) = handle.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "the delete never landed");
            thread::yield_now();
        };

        assert!(result.is_ok(), "delete failed: {result:?}");
        assert_eq!(repaint.wakes(), 1, "the finished delete should wake the UI");
        assert!(!wt_path.exists(), "worktree directory should be gone");
        assert!(!worktree_exists(&repo, "feature"));
        assert!(!branch_exists(&repo, "feature"));
    }

    /// `create` must stop between steps when its handle is gone. Killing a
    /// registered child only covers the steps that have one; the local steps
    /// would otherwise run to completion for a worktree nobody is waiting for.
    #[test]
    fn create_stops_between_steps_once_cancelled() {
        let repo = tempfile::tempdir().expect("temp dir");
        let req = CreateRequest {
            project_root: repo.path().to_path_buf(),
            default_branch: Some("main".into()),
            branch: "topic".into(),
            base_dir: None,
            vcs: git(),
            temporary_origin: None,
        };
        let (tx, rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let job = jobs::pool().spawn(jobs::Priority::Interactive, move |blocking| {
            // Both halves of this handshake are load-bearing. Without the
            // started signal, a flag set while the task is still queued hits the
            // pre-start check, the task is skipped, `tx` drops unsent, and the
            // assertion below reports a disconnect. Without the gate, the task
            // can race past the first bail before the flag lands and fail on the
            // missing remote instead.
            let _ = started_tx.send(());
            let _ = gate_rx.recv();
            let _ = tx.send(create(&req, &[] as &[FakeHook], |_| {}, blocking));
        });
        started_rx.recv_timeout(Duration::from_secs(5)).expect("the job never started");
        drop(job);
        let _ = gate_tx.send(());
        let result = rx.recv_timeout(Duration::from_secs(10));
        match result {
            Ok(Err(WorktreeError::Cancelled)) => {},
            Ok(Err(e)) => panic!("create failed for the wrong reason: {e}"),
            Ok(Ok(path)) => panic!("create finished a worktree nobody was waiting for: {path:?}"),
            Err(e) => panic!("create never returned: {e}"),
        }
    }

    /// A caller that gave up before the hooks must not have them started only
    /// for the cancel to kill each one.
    #[test]
    fn create_runs_no_hook_once_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        let req = CreateRequest {
            project_root: project,
            default_branch: None,
            branch: "abandoned".into(),
            base_dir: Some(tmp.path().join("worktrees")),
            vcs: git(),
            temporary_origin: None,
        };
        let hook = FakeHook::reporting("Linked 1 fake scope");
        let job_hook = hook.clone();
        let (tx, rx) = mpsc::channel();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let job = jobs::pool().spawn(jobs::Priority::Interactive, move |blocking| {
            // Park on a step before the hooks until the handle is gone.
            let on_step = |step: &str| {
                if step.starts_with("Copying LLM configurations") {
                    let _ = reached_tx.send(());
                    let _ = gate_rx.recv();
                }
            };
            let _ = tx.send(create(&req, &[job_hook][..], on_step, blocking));
        });
        reached_rx.recv_timeout(Duration::from_secs(30)).expect("create never reached the hooks");
        drop(job);
        let _ = gate_tx.send(());
        let result = rx.recv_timeout(Duration::from_secs(10)).expect("create never returned");
        assert!(matches!(result, Err(WorktreeError::Cancelled)), "{result:?}");
        assert_eq!(hook.events(), []);
    }

    /// Everything that treats a checkout as temporary reads this path, so the
    /// create has to land exactly there, from whichever checkout it was asked.
    #[test]
    fn a_temporary_create_lands_under_the_checkout_it_was_made_from() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        let workspace = crate::test_util::workspace_under(&tmp.path().join("worktrees"));
        let linked = add_worktree(&project, "linked");
        let inside = linked.join("nested");
        std::fs::create_dir(&inside).unwrap();

        let make = |from: &Path, branch: &str| {
            jobs::on_this_thread(|b| {
                let req =
                    CreateRequest::temporary(from, branch.into(), &workspace, git(), b).unwrap();
                // Git and canonicalize spell a Windows path differently.
                assert_eq!(
                    req.project_root().canonicalize().unwrap(),
                    project.canonicalize().unwrap()
                );
                create(&req, &[] as &[FakeHook], |_| {}, b)
            })
            .expect("create succeeds")
        };

        let first = make(&inside, "scratch");
        assert_eq!(temporary::origin_name(&first).unwrap(), linked.file_name().unwrap());
        assert_eq!(first.file_name().unwrap(), "scratch");

        // Made from a temporary worktree, it files under that one's origin.
        let second = make(&first, "scratch-two");
        assert_eq!(temporary::origin_name(&second).unwrap(), linked.file_name().unwrap());
    }

    #[test]
    fn removing_a_temporary_worktree_takes_its_branch_and_refuses_unsaved_work() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        let workspace = crate::test_util::workspace_under(&tmp.path().join("worktrees"));
        let path = jobs::on_this_thread(|b| {
            let req =
                CreateRequest::temporary(&project, "scratch".into(), &workspace, git(), b).unwrap();
            create(&req, &[] as &[FakeHook], |_| {}, b)
        })
        .expect("create succeeds");
        std::fs::write(path.join("unsaved"), "x").unwrap();
        let hook = FakeHook::silent();
        let remove = |force: bool| {
            jobs::on_this_thread(|b| remove_temporary(&git(), &path, force, &[hook.clone()][..], b))
        };

        let refused = remove(false);
        assert!(
            matches!(&refused, Err(WorktreeError::Vcs(VcsError::Unsaved { .. }))),
            "{refused:?}"
        );
        assert!(path.is_dir());

        let main = remove(true).expect("forced removal succeeds");
        assert_eq!(main.canonicalize().unwrap(), project.canonicalize().unwrap());
        assert!(!path.exists());
        assert!(!branch_exists(&project, "scratch"));
        assert_eq!(hook.events().len(), 1, "the removal hooks run as for any removal");
    }

    /// The create leaves these behind, so a refusal over them would meet every
    /// agent on its first removal.
    #[test]
    fn what_the_create_added_does_not_count_as_unsaved_work() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        std::fs::write(project.join("CLAUDE.local.md"), "notes").unwrap();
        let workspace = crate::test_util::workspace_under(&tmp.path().join("worktrees"));
        let make = |branch: &str| {
            jobs::on_this_thread(|b| {
                let req = CreateRequest::temporary(&project, branch.into(), &workspace, git(), b)?;
                create(&req, &[] as &[FakeHook], |_| {}, b)
            })
            .expect("create succeeds")
        };
        let remove = |path: &Path| {
            jobs::on_this_thread(|b| remove_temporary(&git(), path, false, &[] as &[FakeHook], b))
        };

        let path = make("copied-only");
        assert!(path.join("CLAUDE.local.md").is_file(), "the create copied it");
        remove(&path).expect("nothing here is the agent's work");
        assert!(!path.exists());

        // Anything beside those files is still the agent's to decide about.
        let path = make("with-work");
        std::fs::write(path.join("unsaved"), "x").unwrap();
        let refused = remove(&path);
        assert!(
            matches!(&refused, Err(WorktreeError::Vcs(VcsError::Unsaved { .. }))),
            "{refused:?}"
        );
        assert!(path.is_dir());
    }

    /// An agent holding this tool must not be able to delete a worktree the
    /// user relies on, the main checkout above all.
    #[test]
    fn only_a_temporary_worktree_can_be_removed_this_way() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = init_repo(&tmp.path().join("repo"));
        let linked = add_worktree(&repo, "keep");
        for path in [&repo, &linked] {
            let result = jobs::on_this_thread(|b| {
                remove_temporary(&git(), path, true, &[] as &[FakeHook], b)
            });
            assert!(matches!(result, Err(WorktreeError::NotTemporary(_))), "{result:?}");
            assert!(path.is_dir());
        }
    }

    #[test]
    fn copying_configs_leaves_a_nested_checkout_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
        let nested = src.join(".claude/worktrees/native");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(nested.join(".git"), "gitdir: elsewhere").unwrap();

        assert_eq!(copy_llm_configs(&src, &dst), 1);

        assert!(dst.join(".claude/settings.json").is_file());
        assert!(!dst.join(".claude/worktrees/native").exists());
    }

    #[test]
    fn create_hands_the_new_checkout_to_every_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        let req = CreateRequest {
            project_root: project.clone(),
            default_branch: None,
            branch: "hooked".into(),
            base_dir: Some(tmp.path().join("worktrees")),
            vcs: git(),
            temporary_origin: None,
        };
        let hook = FakeHook::reporting("Linked 1 fake scope");
        let mut steps = Vec::new();
        let target = jobs::on_this_thread(|b| {
            create(&req, &[hook.clone()][..], |s| steps.push(s.to_string()), b)
        })
        .expect("create succeeds");
        assert_eq!(hook.events(), [Event::Created { main: project, checkout: target }]);
        assert!(steps.iter().any(|s| s == "Linked 1 fake scope"), "{steps:?}");
    }

    #[test]
    fn a_failing_hook_shows_in_the_steps_and_does_not_fail_the_create() {
        let tmp = tempfile::tempdir().unwrap();
        let project = clone_with_origin(tmp.path());
        let req = CreateRequest {
            project_root: project,
            default_branch: None,
            branch: "hook-fails".into(),
            base_dir: Some(tmp.path().join("worktrees")),
            vcs: git(),
            temporary_origin: None,
        };
        let mut steps = Vec::new();
        let result = jobs::on_this_thread(|b| {
            create(&req, &[FakeHook::failing()][..], |s| steps.push(s.to_string()), b)
        });
        assert!(result.is_ok(), "{result:?}");
        assert!(
            steps.iter().any(|s| s.starts_with("Hook failed: could not run fake")),
            "{steps:?}"
        );
    }

    /// The target path is picked only once the base resolved, so a create
    /// that fails early leaves no directory behind.
    #[test]
    fn a_failed_prepare_picks_no_target_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base_dir = tmp.path().join("worktrees");
        let fake = FakeVcs::new("/r").refusing_prepare();
        let req = CreateRequest {
            project_root: PathBuf::from("/r"),
            default_branch: None,
            branch: "topic".into(),
            base_dir: Some(base_dir.clone()),
            vcs: crate::vcs::Vcs::Fake(fake.clone()),
            temporary_origin: None,
        };
        let result = jobs::on_this_thread(|b| create(&req, &[] as &[FakeHook], |_| {}, b));
        assert!(matches!(result, Err(WorktreeError::Vcs(VcsError::NoRemote { .. }))), "{result:?}");
        assert!(!base_dir.exists(), "the target's parent was created before the base resolved");
        assert_eq!(fake.calls(), ["prepare /r"]);
    }

    #[test]
    fn hooks_run_after_the_backend_creates_the_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeVcs::new("/r");
        let hook = FakeHook::silent();
        let req = CreateRequest {
            project_root: PathBuf::from("/r"),
            default_branch: None,
            branch: "topic".into(),
            base_dir: Some(tmp.path().to_path_buf()),
            vcs: crate::vcs::Vcs::Fake(fake.clone()),
            temporary_origin: None,
        };
        let target = jobs::on_this_thread(|b| create(&req, &[hook.clone()][..], |_| {}, b))
            .expect("create succeeds");
        assert_eq!(fake.calls(), [
            "prepare /r".to_string(),
            format!("create {}", target.display())
        ]);
        assert_eq!(hook.events(), [Event::Created { main: PathBuf::from("/r"), checkout: target }]);
    }

    /// A hook may key its state by canonical path, and a removed directory
    /// can no longer be canonicalized, so the hook must get the path resolved
    /// first.
    #[cfg(unix)]
    #[test]
    fn removal_hands_hooks_the_path_resolved_before_git_deleted_it() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tmp.path().join("repo");
        let repo = init_repo(&repo_dir);
        let wt_path = add_worktree(&repo, "linked");
        let canonical = wt_path.canonicalize().unwrap();
        let link = tmp.path().join("via-link");
        std::os::unix::fs::symlink(&wt_path, &link).unwrap();
        let hook = FakeHook::silent();
        let req = RemoveCheckout {
            main: repo_dir.clone(),
            checkout: live_checkout(&link, "linked"),
            force: true,
            delete_name: true,
        };
        jobs::on_this_thread(|b| delete_worktree(&git(), &req, &[hook.clone()][..], b))
            .expect("delete succeeds");
        assert_eq!(hook.events(), [Event::Removed { main: repo_dir, checkout: canonical }]);
    }
}
