//! Readies Claude Code for alacritree's terminals once, at startup. See
//! `alacritree_claude` for what each half does and why it touches no
//! checkout.
//!
//! The launcher is written whatever the config says, so turning
//! `terminal_bell` on in a reloaded config reaches the next session without a
//! restart. Only [`session_path`] reads the setting.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::Config;

/// Set once the launcher is on disk, so a failed write leaves `PATH` alone.
static LAUNCHER_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_up(config: &Config) {
    #[cfg(unix)]
    {
        let dir = crate::ipc::protocol::socket_dir().join("bin");
        match alacritree_claude::write_launcher(&dir) {
            Ok(_) => {
                let _ = LAUNCHER_DIR.set(dir);
            },
            Err(e) => log::warn!("{}", chain(&e)),
        }
    }
    if config.integrations.claude.hide_diff_panel {
        // `[env]` is what a session's `claude` sees, so it names the config
        // that session reads.
        let config_dir = config
            .env
            .get("CLAUDE_CONFIG_DIR")
            .map(OsString::from)
            .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR"));
        // Off the startup path: the file is Claude Code's history too, and
        // can run to megabytes.
        std::thread::spawn(move || {
            let Some(path) = alacritree_claude::global_config_path(config_dir.as_deref()) else {
                return;
            };
            match alacritree_claude::hide_diff_panel(&path) {
                Ok(true) => log::info!("Hid Claude Code's diff panel in {}", path.display()),
                Ok(false) => {},
                Err(e) => log::warn!("{}", chain(&e)),
            }
        });
    }
}

/// `PATH` for a session with the launcher first, or `None` to leave it as
/// `path` (the session's inherited or `[env]` value) says.
pub(crate) fn session_path(config: &Config, path: Option<&OsStr>) -> Option<String> {
    let dir = LAUNCHER_DIR.get().filter(|_| config.integrations.claude.terminal_bell)?;
    prepend(dir, path)
}

fn prepend(dir: &Path, path: Option<&OsStr>) -> Option<String> {
    let rest = path.map(std::env::split_paths).into_iter().flatten().filter(|p| p != dir);
    std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest)).ok()?.into_string().ok()
}

fn chain(e: &alacritree_claude::ClaudeError) -> String {
    match std::error::Error::source(e) {
        Some(source) => format!("{e}: {source}"),
        None => e.to_string(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_goes_first_once() {
        let path = OsStr::new("/usr/bin:/run/a/bin:/bin");
        assert_eq!(
            prepend(Path::new("/run/a/bin"), Some(path)).unwrap(),
            "/run/a/bin:/usr/bin:/bin"
        );
        assert_eq!(prepend(Path::new("/run/a/bin"), None).unwrap(), "/run/a/bin");
    }
}
