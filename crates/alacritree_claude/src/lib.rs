//! Claude Code as a checkout hook. A new worktree gets Claude Code's
//! notification channel set to the terminal bell, so a session waiting on
//! input rings and the sidebar flags it without the user configuring each
//! worktree by hand.

use std::path::Path;

use alacritree_checkout_hooks::{CheckoutEvent, CheckoutHook, HookError, Outcome};
use alacritree_common::jobs::Blocking;
use serde::Deserialize;
use serde_json::{Map, Value};

/// `[integrations.claude]`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RawClaude {
    /// Set `preferredNotifChannel` to `terminal_bell` in each new worktree's
    /// `.claude/settings.local.json`, so Claude Code rings the bell when it
    /// waits on input. Other keys in the file are kept.
    terminal_bell: bool,
}

impl Default for RawClaude {
    fn default() -> Self {
        Self { terminal_bell: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClaudeConfig {
    pub terminal_bell: bool,
}

impl RawClaude {
    pub fn resolve(self) -> ClaudeConfig {
        ClaudeConfig { terminal_bell: self.terminal_bell }
    }
}

impl ClaudeConfig {
    pub fn hook(&self) -> Option<ClaudeBellHook> {
        self.terminal_bell.then_some(ClaudeBellHook)
    }
}

/// Runs only on create. Opening a worktree made elsewhere leaves its Claude
/// settings as the user wrote them.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeBellHook;

impl CheckoutHook for ClaudeBellHook {
    fn on_created(&self, event: &CheckoutEvent<'_>, _: &Blocking) -> Outcome {
        let path = event.checkout.join(".claude").join("settings.local.json");
        enable_terminal_bell(&path).map_err(|source| HookError::Write {
            hook: "Claude Code".into(),
            path,
            source,
        })?;
        Ok(Some("Enabled Claude Code terminal bell".into()))
    }

    fn on_opened(&self, _: &CheckoutEvent<'_>, _: &Blocking) -> Outcome {
        Ok(None)
    }

    fn on_removed(&self, _: &CheckoutEvent<'_>, _: &Blocking) -> Outcome {
        Ok(None)
    }
}

/// A file that is not a JSON object is replaced. Worktree creation copies it
/// from the main checkout's `.claude/`, which keeps the original.
fn enable_terminal_bell(path: &Path) -> std::io::Result<()> {
    let mut settings = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Map<String, Value>>(&text).unwrap_or_default(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e),
    };
    settings.insert("preferredNotifChannel".into(), "terminal_bell".into());
    let pretty = serde_json::to_string_pretty(&settings).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, pretty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritree_common::jobs;

    fn created(checkout: &Path) -> Outcome {
        let event = CheckoutEvent { main: checkout, checkout };
        jobs::on_this_thread(|b| ClaudeBellHook.on_created(&event, b))
    }

    fn settings(checkout: &Path) -> Value {
        let text = std::fs::read_to_string(checkout.join(".claude/settings.local.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn a_new_worktree_gets_the_bell() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            created(dir.path()).unwrap().as_deref(),
            Some("Enabled Claude Code terminal bell")
        );
        assert_eq!(
            settings(dir.path()),
            serde_json::json!({ "preferredNotifChannel": "terminal_bell" })
        );
    }

    #[test]
    fn other_settings_survive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.local.json"),
            r#"{ "model": "opus", "preferredNotifChannel": "iterm2" }"#,
        )
        .unwrap();
        created(dir.path()).unwrap();
        assert_eq!(
            settings(dir.path()),
            serde_json::json!({ "model": "opus", "preferredNotifChannel": "terminal_bell" })
        );
    }

    #[test]
    fn a_table_can_turn_the_bell_off() {
        assert!(RawClaude::default().resolve().hook().is_some());
        let raw: RawClaude = toml::from_str("terminal_bell = false").unwrap();
        assert!(raw.resolve().hook().is_none());
    }
}
