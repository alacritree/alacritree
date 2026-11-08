//! The programs a session's probe recognizes by name, shared so every
//! backend reads a foreground program the same way.

/// Foreground process names recognized as agents. `comm` is kernel-truncated
/// (15 bytes on Linux, 16 on macOS, which `cursor-agent` would otherwise
/// miss) and Windows names carry an `.exe` suffix, so matching uses
/// `starts_with`.
pub const AGENT_PROCESS_NAMES: &[&str] =
    &["claude", "codex", "gemini", "aider", "cursor-agent", "continue"];

/// TUIs that manage their own splits and cooperate with FocusLeft/
/// FocusRight (vim, nvim, tmux, zellij, herdr): the key is forwarded while
/// one runs, and the TUI calls `alacritree action FocusLeft` or `FocusRight`
/// over IPC once it has no split left in that direction. Matches Linux
/// `comm` values (`tmux: client`) and Windows image names (`nvim.exe`)
/// alike. gvim stays out because it owns its own window. The WSL helper's
/// `PROBE` script carries the same list.
pub fn is_nav_tui_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["nvim", "vim", "tmux", "zellij", "herdr"].iter().any(|tui| n.starts_with(tui))
}
