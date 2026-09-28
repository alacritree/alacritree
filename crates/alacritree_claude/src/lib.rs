//! Claude Code inside alacritree's terminals. A session waiting on input
//! rings the bell so the sidebar flags it, and Claude Code's own diff panel
//! stays shut because the git panel already shows the same changes.
//!
//! Neither touches a checkout. The bell rides on a `claude` launcher that
//! only alacritree's sessions have on `PATH`, so Claude Code started anywhere
//! else behaves as the user configured it. Claude Code has no per-process
//! switch for its diff panel, so that one is the global preference the panel
//! itself remembers, edited in place so the rest of the file stays as it was.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

/// `[integrations.claude]`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RawClaude {
    /// Run `claude` in alacritree's terminals with `preferredNotifChannel`
    /// set to `terminal_bell`, so Claude Code rings the bell when it waits on
    /// input. Passed on the command line by a launcher on the sessions'
    /// `PATH`, so no settings file changes. A shell startup file that puts
    /// another directory holding `claude` ahead of it on `PATH` bypasses it.
    terminal_bell: bool,
    /// Set `diffSidebarOpen` to `false` in Claude Code's global config when
    /// alacritree starts, so Claude Code's diff panel does not open on its
    /// own beside the git panel. Claude Code has no per-session switch for
    /// it, so this applies to Claude Code in every terminal. `/diff` still
    /// opens the panel.
    hide_diff_panel: bool,
}

impl Default for RawClaude {
    fn default() -> Self {
        Self { terminal_bell: true, hide_diff_panel: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClaudeConfig {
    pub terminal_bell: bool,
    pub hide_diff_panel: bool,
}

impl RawClaude {
    pub fn resolve(self) -> ClaudeConfig {
        ClaudeConfig { terminal_bell: self.terminal_bell, hide_diff_panel: self.hide_diff_panel }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("could not write the Claude Code launcher {}", path.display())]
    Launcher { path: PathBuf, source: std::io::Error },
    #[error("could not update Claude Code's config {}", path.display())]
    GlobalConfig { path: PathBuf, source: std::io::Error },
}

/// The settings the launcher hands `claude --settings`. Flag settings rank
/// above every settings file but policy, and are gone when the process is.
const LAUNCH_SETTINGS: &str = r#"{"preferredNotifChannel":"terminal_bell"}"#;

/// Write `dir/claude`, a script that runs the next `claude` on `PATH` with
/// [`LAUNCH_SETTINGS`]. The caller puts `dir` first on a session's `PATH`.
/// The script is written whole and renamed into place, since a shell in an
/// earlier session may exec it at any moment.
#[cfg(unix)]
pub fn write_launcher(dir: &Path) -> Result<PathBuf, ClaudeError> {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("claude");
    let write = || {
        std::fs::create_dir_all(dir)?;
        let staged = dir.join(format!(".claude.{}", std::process::id()));
        std::fs::write(&staged, launcher_script(dir))?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&staged, &path)
    };
    write().map_err(|source| ClaudeError::Launcher { path: path.clone(), source })?;
    Ok(path)
}

/// Skipping its own directory by name, rather than by resolving `$0`, keeps
/// a symlink or a relative `PATH` entry from making it exec itself.
#[cfg(unix)]
fn launcher_script(dir: &Path) -> String {
    let own = dir.to_string_lossy().replace('\'', r"'\''");
    format!(
        r#"#!/bin/sh
# Written by alacritree, whose terminals put this directory first on PATH.
# Runs the next `claude` on PATH with the terminal bell as its notification
# channel, so the sidebar sees it wait, without editing any settings file.
own='{own}'
set -f
IFS=:
for dir in $PATH; do
    if [ -z "$dir" ] || [ "$dir" = "$own" ]; then
        continue
    fi
    if [ -f "$dir/claude" ] && [ -x "$dir/claude" ]; then
        unset IFS
        exec "$dir/claude" --settings '{LAUNCH_SETTINGS}' "$@"
    fi
done
echo "claude: command not found" >&2
exit 127
"#
    )
}

/// The file Claude Code keeps its global preferences in, `diffSidebarOpen`
/// among them: `.config.json` under its config dir when that legacy file
/// exists, `.claude.json` under `$CLAUDE_CONFIG_DIR` or the home dir
/// otherwise. `config_dir` is the `CLAUDE_CONFIG_DIR` sessions get.
pub fn global_config_path(config_dir: Option<&OsStr>) -> Option<PathBuf> {
    let config_dir = config_dir.filter(|dir| !dir.is_empty()).map(PathBuf::from);
    let home = home::home_dir();
    let legacy = config_dir.clone().or_else(|| home.as_ref().map(|h| h.join(".claude")));
    if let Some(legacy) = legacy.map(|dir| dir.join(".config.json")).filter(|p| p.is_file()) {
        return Some(legacy);
    }
    Some(config_dir.or(home)?.join(".claude.json"))
}

const DIFF_PANEL_KEY: &str = "diffSidebarOpen";

/// Set `diffSidebarOpen` to `false`, reporting whether the file changed. A
/// missing file is left for Claude Code to create, and a file that is not a
/// JSON object is not ours to repair. Only the one value changes where a
/// text edit can do it, so the user's file keeps its layout and key order.
pub fn hide_diff_panel(path: &Path) -> Result<bool, ClaudeError> {
    let error = |source| ClaudeError::GlobalConfig { path: path.to_path_buf(), source };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(error(e)),
    };
    let Ok(config) = serde_json::from_str::<Map<String, Value>>(&text) else {
        return Ok(false);
    };
    if config.get(DIFF_PANEL_KEY) == Some(&Value::Bool(false)) {
        return Ok(false);
    }
    let edited = match edit_in_place(&text).filter(|edited| hides_only_the_panel(edited, &config)) {
        Some(edited) => edited,
        None => {
            let mut config = config;
            config.insert(DIFF_PANEL_KEY.into(), Value::Bool(false));
            serde_json::to_string_pretty(&config).map_err(|e| error(std::io::Error::other(e)))?
        },
    };
    replace(path, &edited).map_err(error)?;
    Ok(true)
}

/// Flip a `true`, or add the key after the opening brace. Anything else
/// (another value, the key nested somewhere) is caught by the check that
/// follows and falls back to a rewrite.
fn edit_in_place(text: &str) -> Option<String> {
    let quoted = format!("\"{DIFF_PANEL_KEY}\"");
    if let Some(at) = text.find(&quoted) {
        let after_key = at + quoted.len();
        let rest = text[after_key..].trim_start().strip_prefix(':')?.trim_start();
        let value = text.len() - rest.len();
        rest.starts_with("true").then(|| format!("{}false{}", &text[..value], &rest[4..]))
    } else {
        let open = text.find('{')? + 1;
        let empty = text[open..].trim_start().starts_with('}');
        let entry =
            if empty { format!("{quoted}: false") } else { format!("\n  {quoted}: false,") };
        Some(format!("{}{entry}{}", &text[..open], &text[open..]))
    }
}

fn hides_only_the_panel(edited: &str, before: &Map<String, Value>) -> bool {
    let Ok(mut after) = serde_json::from_str::<Map<String, Value>>(edited) else {
        return false;
    };
    let hidden = after.remove(DIFF_PANEL_KEY) == Some(Value::Bool(false));
    let mut before = before.clone();
    before.remove(DIFF_PANEL_KEY);
    hidden && after == before
}

/// Write beside the target and rename over it, so a Claude Code reading the
/// file never sees half of it. A symlinked config (a dotfiles repo) is
/// written through, keeping the link, and the file keeps its permissions.
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let permissions = std::fs::metadata(&target)?.permissions();
    let mut staged = target.clone().into_os_string();
    staged.push(format!(".alacritree-{}", std::process::id()));
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, text)?;
    std::fs::set_permissions(&staged, permissions)?;
    std::fs::rename(&staged, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hidden(before: &str) -> (bool, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, before).unwrap();
        let changed = hide_diff_panel(&path).unwrap();
        (changed, std::fs::read_to_string(&path).unwrap())
    }

    #[test]
    fn an_open_panel_is_flipped_in_place() {
        let before = "{\n  \"theme\": \"dark\",\n  \"diffSidebarOpen\": true,\n  \"zz\": 1\n}";
        assert_eq!(hidden(before), (true, before.replace("true", "false")));
    }

    #[test]
    fn a_missing_key_is_added_first() {
        let before = "{\n  \"zz\": 1,\n  \"aa\": 2\n}";
        let (changed, after) = hidden(before);
        assert!(changed);
        assert_eq!(after, "{\n  \"diffSidebarOpen\": false,\n  \"zz\": 1,\n  \"aa\": 2\n}");
    }

    #[test]
    fn an_empty_object_gets_the_key() {
        assert_eq!(hidden("{}"), (true, "{\"diffSidebarOpen\": false}".into()));
    }

    #[test]
    fn a_hidden_panel_is_left_alone() {
        let before = "{ \"diffSidebarOpen\": false }";
        assert_eq!(hidden(before), (false, before.into()));
    }

    #[test]
    fn a_nested_key_falls_back_to_a_rewrite() {
        let (changed, after) = hidden(r#"{ "projects": { "diffSidebarOpen": true } }"#);
        assert!(changed);
        let after: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(
            after,
            serde_json::json!({ "projects": { "diffSidebarOpen": true }, "diffSidebarOpen": false })
        );
    }

    #[test]
    fn a_file_that_is_not_an_object_is_left_alone() {
        assert_eq!(hidden("not json"), (false, "not json".into()));
    }

    #[test]
    fn a_missing_file_stays_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        assert!(!hide_diff_panel(&path).unwrap());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_keeps_its_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles.json");
        std::fs::write(&real, "{}").unwrap();
        let link = dir.path().join(".claude.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(hide_diff_panel(&link).unwrap());
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{\"diffSidebarOpen\": false}");
    }

    #[test]
    fn claude_config_dir_moves_the_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            global_config_path(Some(dir.path().as_os_str())),
            Some(dir.path().join(".claude.json"))
        );
        std::fs::write(dir.path().join(".config.json"), "{}").unwrap();
        assert_eq!(
            global_config_path(Some(dir.path().as_os_str())),
            Some(dir.path().join(".config.json"))
        );
    }

    /// Runs the launcher against a fake `claude` further down `PATH`, with
    /// the launcher's own directory listed twice to prove it skips itself.
    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Running the launcher is the test.
    fn the_launcher_runs_the_next_claude_with_the_bell() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("launcher");
        let launcher = write_launcher(&own).unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("claude"), "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(real.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let path = std::env::join_paths([&own, &own, &real]).unwrap();

        let out = std::process::Command::new(&launcher)
            .args(["-p", "two words"])
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("--settings\n{LAUNCH_SETTINGS}\n-p\ntwo words\n")
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::disallowed_methods)] // Running the launcher is the test.
    fn the_launcher_alone_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = write_launcher(dir.path()).unwrap();
        let out = std::process::Command::new(&launcher).env("PATH", dir.path()).output().unwrap();
        assert_eq!(out.status.code(), Some(127));
    }

    #[test]
    fn a_table_can_turn_each_off() {
        let raw: RawClaude = toml::from_str("terminal_bell = false").unwrap();
        assert_eq!(raw.resolve(), ClaudeConfig { terminal_bell: false, hide_diff_panel: true });
        let raw: RawClaude = toml::from_str("hide_diff_panel = false").unwrap();
        assert_eq!(raw.resolve(), ClaudeConfig { terminal_bell: true, hide_diff_panel: false });
    }
}
