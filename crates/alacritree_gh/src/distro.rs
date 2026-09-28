//! The scripts the gh forge runs inside a WSL distro, and the parsers for
//! what they print.
//!
//! Nothing on the Windows side reads a repository that lives inside a
//! distro, so each script carries its own git reads. They are plain `sh` so
//! a test can run the exact text under any unix shell, without a distro.

/// Picks the remote `git push` sends branch `$b` to, from the checkout in the
/// working directory, and leaves its name in `$r`. It walks the keys in git's
/// order and skips a missing, empty or `.` value, as the native reader in
/// `alacritree_git` does, so both sides agree on whose PR a branch can have.
pub(crate) const PUSH_REMOTE: &str = r#"r=origin
for k in "branch.$b.pushRemote" remote.pushDefault "branch.$b.remote"; do
  v=$(git config --get "$k") || continue
  case "$v" in ''|.) continue ;; esac
  r=$v
  break
done"#;

/// The per-branch lookup: the push remote's URL on the first line, blank when
/// it has none, then `gh pr list`'s JSON. `$1` is the checkout, `$2` the
/// distro's `gh`, `$3` the branch, `$4` the page limit and `$5` the fields.
pub(crate) fn per_branch_script() -> String {
    format!(
        r#"cd "$1" || exit 1
b=$3
{PUSH_REMOTE}
printf '%s\n' "$(git config --get "remote.$r.url" 2>/dev/null)"
exec "$2" pr list --head "$3" --state all --limit "$4" --json "$5""#
    )
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    mod sh {
        use std::path::{Path, PathBuf};
        use std::process::Command;

        use crate::distro::*;

        /// Runs `git` in `dir`, failing the test when it does.
        #[allow(clippy::disallowed_methods)] // A test's own fixture, off the UI thread.
        fn git(dir: &Path, args: &[&str]) {
            let ok = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
            assert!(ok.success(), "git {args:?}");
        }

        /// A repository at `dir` with nothing configured.
        fn repo(dir: &Path) -> PathBuf {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            dir.to_path_buf()
        }

        /// Runs `script` under `sh` with `args` as `$1...`, returning stdout.
        #[allow(clippy::disallowed_methods)] // A test's own fixture, off the UI thread.
        fn sh(script: &str, args: &[&str]) -> String {
            let out = Command::new("sh").arg("-c").arg(script).arg("sh").args(args).output();
            String::from_utf8(out.unwrap().stdout).unwrap()
        }

        /// The remote [`PUSH_REMOTE`] picks for `branch` in `repo`.
        fn push_remote(repo: &Path, branch: &str) -> String {
            let script = format!("cd \"$1\" && b=$2 && {PUSH_REMOTE}\nprintf %s \"$r\"");
            sh(&script, &[repo.to_str().unwrap(), branch])
        }

        /// `.` names the local repository, which pushes nowhere, so git moves
        /// on to `remote.pushDefault` rather than settling on `origin`.
        #[test]
        fn push_remote_skips_a_dot_and_takes_push_default() {
            let dir = tempfile::tempdir().unwrap();
            let repo = repo(dir.path());
            git(&repo, &["config", "branch.main.pushRemote", "."]);
            git(&repo, &["config", "remote.pushDefault", "fork"]);
            assert_eq!(push_remote(&repo, "main"), "fork");
        }

        #[test]
        fn push_remote_prefers_the_branch_push_remote() {
            let dir = tempfile::tempdir().unwrap();
            let repo = repo(dir.path());
            git(&repo, &["config", "branch.main.pushRemote", "fork"]);
            git(&repo, &["config", "remote.pushDefault", "other"]);
            assert_eq!(push_remote(&repo, "main"), "fork");
        }

        #[test]
        fn push_remote_defaults_to_origin() {
            let dir = tempfile::tempdir().unwrap();
            let repo = repo(dir.path());
            assert_eq!(push_remote(&repo, "main"), "origin");
        }
    }
}
