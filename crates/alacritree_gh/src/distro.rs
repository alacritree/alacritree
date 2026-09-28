//! The scripts the gh forge runs inside a WSL distro, and the parsers for
//! what they print.
//!
//! Nothing on the Windows side reads a repository that lives inside a
//! distro, so each script carries its own git reads. They are plain `sh` so
//! a test can run the exact text under any unix shell, without a distro.

use alacritree_common::jobs::Blocking;
use alacritree_common::wsl;
use alacritree_vcs::Remotes;

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
/// A repository with no remote answers an empty list without running `gh`.
pub(crate) fn per_branch_script() -> String {
    format!(
        r#"cd "$1" || exit 1
if git rev-parse --git-dir >/dev/null 2>&1 && [ -z "$(git remote)" ]; then
  printf '\n[]'
  exit 0
fi
b=$3
{PUSH_REMOTE}
printf '%s\n' "$(git config --get "remote.$r.url" 2>/dev/null)"
exec "$2" pr list --head "$3" --state all --limit "$4" --json "$5""#
    )
}

/// Aliases per WSL request. `run_batch` has no stdin, so the GraphQL body
/// rides in argv, and the one-shot `wsl.exe` caps a command line at 32,767
/// characters. Quoted for Windows, 50 aliases come to about 12k characters,
/// which leaves room for long branch names where 100 would not.
pub(crate) const CHUNK: usize = 50;

/// One record per `path branch` pair in `$@`: a status, then origin's URL,
/// then the push remote's, tab separated. Status `0` is a checkout git could
/// not read, `1` one with remotes, and `2` one that lists none at all.
pub(crate) fn remotes_script() -> String {
    format!(
        r#"while [ "$#" -ge 2 ]; do
p=$1
b=$2
shift 2
if cd "$p" 2>/dev/null && git rev-parse --git-dir >/dev/null 2>&1; then
  if [ -z "$(git remote)" ]; then
    printf '2\t\t\n'
  else
    o=$(git config --get remote.origin.url)
{PUSH_REMOTE}
    u=$(git config --get "remote.$r.url")
    printf '1\t%s\t%s\n' "$o" "$u"
  fi
else
  printf '0\t\t\n'
fi
done"#
    )
}

/// The records [`remotes_script`] printed for `n` checkouts, in order. A
/// checkout git could not read is `None`, which leaves it on the per-branch
/// path. A record set that does not line up with the checkouts is no answer at
/// all, since records pair with checkouts by position.
pub(crate) fn parse_remotes(stdout: &[u8], n: usize) -> Option<Vec<Option<Remotes>>> {
    let records: Vec<&str> = std::str::from_utf8(stdout).ok()?.lines().collect();
    if records.len() != n {
        return None;
    }
    let url = |field: &str| (!field.is_empty()).then(|| field.to_string());
    records
        .into_iter()
        .map(|record| {
            let mut fields = record.split('\t');
            let (status, origin, push) = (fields.next()?, fields.next()?, fields.next()?);
            match status {
                "0" => Some(None),
                "1" => Some(Some(Remotes {
                    origin_url: url(origin),
                    push_url: url(push),
                    no_remotes: false,
                })),
                "2" => Some(Some(Remotes { no_remotes: true, ..Default::default() })),
                _ => None,
            }
        })
        .collect()
}

/// Reads every checkout's remotes in one call into `distro`. `heads` pairs
/// each checkout's Linux path with its branch.
pub(crate) fn remotes(
    distro: &str,
    heads: &[(&str, &str)],
    blocking: &Blocking,
) -> Option<Vec<Option<Remotes>>> {
    let args: Vec<&str> = heads.iter().flat_map(|&(path, branch)| [path, branch]).collect();
    let stdout = wsl::run_batch(distro, &remotes_script(), &args, blocking).ok()?;
    parse_remotes(&stdout, heads.len())
}

/// `$1` is the distro's `gh` and `$2` the JSON body, piped in because
/// `gh api graphql --input -` reads it from stdin.
pub(crate) const REQUEST_SCRIPT: &str = r#"printf '%s' "$2" | "$1" api graphql --input -"#;

/// Runs one GraphQL body through the distro's `gh`, returning its stdout.
///
/// `run_batch` hands back stdout whatever `gh` exited with, and `gh api
/// graphql` exits 1 on a GraphQL error while still printing the answer. So a
/// response carrying `errors` beside aliases that did answer is kept here,
/// where the native path's exit check sweeps it per-branch. Keeping the
/// aliases that answered is the rule `graphql::parse` already follows.
pub(crate) fn request(distro: &str, gh: &str, body: &str, blocking: &Blocking) -> Option<Vec<u8>> {
    wsl::run_batch(distro, REQUEST_SCRIPT, &[gh, body], blocking).ok()
}

/// `$1` is the checkout and `$2` the distro's `gh`.
const RESOLVE_SCRIPT: &str = r#"cd "$1" && exec "$2" repo view --json nameWithOwner"#;

/// The repository the distro's `gh` acts on from `linux_path`.
pub(crate) fn resolve(
    distro: &str,
    gh: &str,
    linux_path: &str,
    blocking: &Blocking,
) -> Option<(String, String)> {
    let stdout = wsl::run_batch(distro, RESOLVE_SCRIPT, &[linux_path, gh], blocking).ok()?;
    crate::parse_name_with_owner(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(origin: &str, push: &str) -> Option<Remotes> {
        Some(Remotes {
            origin_url: Some(origin.into()),
            push_url: Some(push.into()),
            no_remotes: false,
        })
    }

    #[test]
    fn each_record_answers_its_own_checkout() {
        let stdout = b"1\thttps://github.com/o/r.git\tgh:me/r.git\n2\t\t\n0\t\t\n";
        let no_remotes = Some(Remotes { no_remotes: true, ..Default::default() });
        assert_eq!(
            parse_remotes(stdout, 3),
            Some(vec![record("https://github.com/o/r.git", "gh:me/r.git"), no_remotes, None])
        );
    }

    /// An empty field is a remote git could not name, which the native reader
    /// reports as `None` too.
    #[test]
    fn an_empty_field_reads_as_no_url() {
        let parsed = parse_remotes(b"1\t\tgh:me/r.git\n", 1).expect("one record");
        assert_eq!(parsed, [Some(Remotes {
            origin_url: None,
            push_url: Some("gh:me/r.git".into()),
            no_remotes: false,
        })]);
    }

    #[test]
    fn the_last_record_needs_no_trailing_newline() {
        assert_eq!(parse_remotes(b"2\t\t\n0\t\t", 2).map(|r| r.len()), Some(2));
    }

    /// Records are matched to checkouts by position, so a missing one would
    /// hand every later checkout its neighbour's remotes.
    #[test]
    fn a_short_record_set_is_no_answer() {
        assert_eq!(parse_remotes(b"2\t\t\n", 2), None);
    }

    #[test]
    fn an_unknown_status_is_no_answer() {
        assert_eq!(parse_remotes(b"7\t\t\n", 1), None);
    }

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

        /// The script runs from the distro with every checkout in one call, so
        /// each one has to answer in its own record, in order. The space in
        /// the first path is one `$p` that must stay one argument.
        #[test]
        fn the_remotes_script_reports_each_checkout() {
            let dir = tempfile::tempdir().unwrap();
            let with = repo(&dir.path().join("with remotes"));
            git(&with, &["remote", "add", "origin", "https://github.com/up/r.git"]);
            git(&with, &["remote", "add", "fork", "https://github.com/me/r.git"]);
            git(&with, &["config", "remote.pushDefault", "fork"]);
            let without = repo(&dir.path().join("without"));
            let plain = dir.path().join("plain");
            std::fs::create_dir(&plain).unwrap();

            let stdout = sh(&remotes_script(), &[
                with.to_str().unwrap(),
                "main",
                without.to_str().unwrap(),
                "main",
                plain.to_str().unwrap(),
                "main",
            ]);

            assert_eq!(
                stdout,
                "1\thttps://github.com/up/r.git\thttps://github.com/me/r.git\n2\t\t\n0\t\t\n"
            );
        }

        /// With no push settings git pushes to `origin`, and the native reader
        /// reports origin's URL as the push URL. The script has to agree.
        #[test]
        fn a_checkout_without_push_settings_pushes_to_origin() {
            let dir = tempfile::tempdir().unwrap();
            let repo = repo(dir.path());
            git(&repo, &["remote", "add", "origin", "gh:me/r.git"]);
            let stdout = sh(&remotes_script(), &[repo.to_str().unwrap(), "main"]);
            assert_eq!(stdout, "1\tgh:me/r.git\tgh:me/r.git\n");
        }

        /// The per-branch path runs only when the remotes read failed, and a
        /// repository with no remote there must read as "no PR" rather than
        /// handing `gh`'s error to the JSON parser.
        #[test]
        fn the_per_branch_script_answers_no_pr_without_remotes() {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let repo = repo(&dir.path().join("repo"));
            let ran = dir.path().join("ran");
            let stub = dir.path().join("gh");
            std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            let stdout = sh(&per_branch_script(), &[
                repo.to_str().unwrap(),
                stub.to_str().unwrap(),
                "main",
                "100",
                "number",
            ]);

            assert_eq!(stdout, "\n[]");
            assert!(!ran.exists(), "gh ran for a repository with no remote");
        }

        /// A folder git cannot read is no evidence of a missing remote, so it
        /// keeps its lookup and whatever failure `gh` reports.
        #[test]
        fn the_per_branch_script_still_asks_outside_a_repository() {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let plain = dir.path().join("plain");
            std::fs::create_dir(&plain).unwrap();
            let ran = dir.path().join("ran");
            let stub = dir.path().join("gh");
            std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

            sh(&per_branch_script(), &[
                plain.to_str().unwrap(),
                stub.to_str().unwrap(),
                "main",
                "100",
                "number",
            ]);

            assert!(ran.exists(), "gh never ran for an unreadable checkout");
        }

        /// The body rides in argv, since `run_batch` has no stdin, and branch
        /// names put quotes and backslashes in it. `gh` has to read it back
        /// byte for byte.
        #[test]
        fn the_request_script_pipes_the_body_verbatim() {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let got = dir.path().join("got");
            let stub = dir.path().join("gh");
            std::fs::write(&stub, format!("#!/bin/sh\ncat > '{}'\n", got.display())).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            let body = r#"{"query":"b0: pullRequests(headRefName: \"a\\\"b\\c %s\")"}"#;

            sh(REQUEST_SCRIPT, &[stub.to_str().unwrap(), body]);

            assert_eq!(std::fs::read_to_string(&got).unwrap(), body);
        }
    }
}
