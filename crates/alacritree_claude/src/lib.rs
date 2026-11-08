//! Claude Code as the source of the sidebar's sub-agent rows.
//! [`subagents`] reads which sub-agents each Claude Code process has running.

pub mod subagents;

use serde::Deserialize;

/// `[integrations.claude]`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct RawClaude {
    /// List the sub-agents each Claude Code session has running, workflow
    /// agents included, in the sidebar under that session's row, or under its
    /// workspace's row when the session is the only one there. They are read
    /// from the transcripts Claude Code keeps in `CLAUDE_CONFIG_DIR` or
    /// `~/.claude`, so a session running inside WSL lists none.
    subagents: bool,
}

impl Default for RawClaude {
    fn default() -> Self {
        Self { subagents: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClaudeConfig {
    pub subagents: bool,
}

impl RawClaude {
    pub fn resolve(self) -> ClaudeConfig {
        ClaudeConfig { subagents: self.subagents }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_is_on_unless_the_table_turns_it_off() {
        assert!(RawClaude::default().resolve().subagents);
        let raw: RawClaude = toml::from_str("subagents = false").unwrap();
        assert!(!raw.resolve().subagents);
    }
}
