//! Display data and local interaction state for catalog stories. Paths are
//! labels only; no fixture discovers a project or starts an integration.

use super::*;
use crate::tasks::view::{Scope, preview::Preview};
use alacritree_multiplexer::MultiplexerKind;
use alacritree_tasks::Task;

pub(super) struct Samples {
    pub project: Project,
    pub reorder: bool,
    pub last_action: String,
    pub palette: CommandPalette,
    pub palette_items: Vec<PaletteItem>,
    pub palette_selected: Option<usize>,
    pub shortcuts: crate::shortcut::Shortcuts,
    pub tasks: Preview,
    pub scratchpads: [scratchpad::Editor; 3],
    pub active_scratchpad: Option<usize>,
    pub tab_names: Vec<String>,
    pub active_tab: SessionId,
    pub profiles: Vec<String>,
    pub filter_query: String,
    pub filter_toggles: [bool; 3],
    pub search_all: bool,
    pub dialog: Option<dialogs::Kind>,
    pub dialog_text: String,
    pub dialog_force: bool,
    pub dialog_branch: usize,
}

impl Samples {
    pub(super) fn new(config: &Config) -> Self {
        let shortcuts = crate::shortcut::Shortcuts::new(&config.bindings);
        let mut palette_items = command_palette::action_items(&shortcuts, true);
        palette_items.extend([
            PaletteItem::workspace(None, "Home".into(), "switch workspace".into()),
            PaletteItem::workspace(
                Some("/repos/alacritree".into()),
                "alacritree / main".into(),
                "switch workspace".into(),
            ),
            PaletteItem::create_worktree(
                "/repos/alacritree".into(),
                "New worktree".into(),
                "alacritree".into(),
            ),
            PaletteItem::profile(
                "Development shell".into(),
                "zsh -l".into(),
                "Ctrl+Shift+1".into(),
                "SpawnProfile1",
            ),
            PaletteItem::session(
                7,
                "Review navigation changes".into(),
                "alacritree / feature/catalog".into(),
                "Codex · working".into(),
                "Open session".into(),
                Some("codex"),
                None,
                None,
            ),
        ]);
        Self {
            project: Project::placeholder("/repos/alacritree".into()),
            reorder: false,
            last_action: String::new(),
            palette: CommandPalette::new(),
            palette_items,
            palette_selected: None,
            shortcuts,
            tasks: tasks(),
            scratchpads: [
                scratchpad::Editor::preview("", None),
                scratchpad::Editor::preview(
                    "# Workspace notes\n\n- Inspect the worktree states\n- Compare narrow and wide layouts\n\n## Follow-up\n\nKeep the terminal at the center.\n",
                    None,
                ),
                scratchpad::Editor::preview(
                    "These notes remain available while a save fails.",
                    Some("Autosave failed: permission denied"),
                ),
            ],
            active_scratchpad: None,
            tab_names: vec![
                "zsh".into(),
                "Codex · review".into(),
                "Scratchpad".into(),
                "Tasks".into(),
            ],
            active_tab: 0,
            profiles: vec!["Development shell".into(), "PowerShell".into(), "Ubuntu (WSL)".into()],
            filter_query: "feature".into(),
            filter_toggles: [true, false, false],
            search_all: false,
            dialog: None,
            dialog_text: "feature/catalog".into(),
            dialog_force: false,
            dialog_branch: 0,
        }
    }
}

pub(super) fn tasks() -> Preview {
    let descriptions = [
        ("global", "Plan the next review", false, None),
        ("alacritree", "Improve the component catalog", false, None),
        ("alacritree.catalog", "Preview every workspace state", false, None),
        (
            "alacritree.catalog",
            "Add narrow-layout examples and check that long descriptions wrap beside their checkbox",
            false,
            Some("fixture-2"),
        ),
        ("alacritree.catalog", "Fix the font slider", true, Some("fixture-2")),
        ("alacritree.catalog.codex-demo", "Review keyboard focus and dialogs", false, None),
        ("alacritree.catalog.claude-demo", "Verify the completed changes", true, None),
    ];
    let tasks = descriptions
        .into_iter()
        .enumerate()
        .map(|(index, (project, text, done, parent))| Task {
            id: format!("fixture-{index}"),
            description: text.into(),
            status: if done { Status::Completed } else { Status::Pending },
            started: index == 2,
            parent: parent.map(String::from),
            order: Some(index as i64 * 100),
            project: Some(project.into()),
            entry: None,
            modified: None,
        })
        .collect();
    Preview::new(
        tasks,
        Scope {
            side: Side::Native,
            repo: Some("alacritree".into()),
            workspace: Some("alacritree.catalog".into()),
        },
    )
}

pub(super) fn pane(
    index: usize,
    multiplexer: MultiplexerKind,
    status: Option<PaneStatus>,
    shared: bool,
) -> sidebar::PaneRowData {
    let title = if status.is_some() { "Review the sidebar" } else { "Development shell" };
    sidebar::PaneRowData {
        key: PaneKey { multiplexer, side: Side::Native, terminal_id: format!("catalog-{index}") },
        pane_id: format!("pane-{index}"),
        name: RowName { text: title.into(), context: status.map(|_| "Codex".into()) },
        managed: Managed {
            multiplexer,
            detach: Some("Ctrl+b, d".into()),
            shared_view: shared,
            kind: status.map(|_| "codex".into()),
            title: Some(title.into()),
            status,
        },
    }
}
