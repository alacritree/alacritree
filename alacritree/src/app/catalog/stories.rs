//! Catalog pages composed from the app's row, editor, and control painters.
//! Only `Samples` changes when a preview control is activated.

use super::*;
use crate::config::DEFAULT_VCS_GIT_ICON;
use alacritree_multiplexer::MultiplexerKind;
use alacritree_vcs::Head;

impl Catalog {
    pub(super) fn icons_page(&self, ui: &mut egui::Ui) {
        self.story(ui, "Navigation and actions", "Hover a glyph to inspect its role", |ui| {
            let icons = &self.icons;
            let entries = [
                ("Home", &icons.home, DEFAULT_HOME_ICON),
                ("Main checkout", &icons.worktree_main, DEFAULT_WORKTREE_MAIN_ICON),
                ("Worktree", &icons.worktree, DEFAULT_WORKTREE_ICON),
                ("Session", &icons.session, DEFAULT_SESSION_ICON),
                ("Search", &icons.search, DEFAULT_SEARCH_ICON),
                ("Git", &icons.vcs_git, DEFAULT_VCS_GIT_ICON),
                ("Expanded", &icons.project_expanded, DEFAULT_PROJECT_EXPANDED_ICON),
                ("Collapsed", &icons.project_collapsed, DEFAULT_PROJECT_COLLAPSED_ICON),
                ("Add project", &icons.add_project, DEFAULT_ADD_ICON),
                ("New worktree", &icons.new_worktree, DEFAULT_ADD_ICON),
                ("New session", &icons.new_session, DEFAULT_ADD_ICON),
                ("Remove project", &icons.remove_project, DEFAULT_CLOSE_ICON),
                ("Delete worktree", &icons.delete_worktree, DEFAULT_CLOSE_ICON),
                ("Close session", &icons.close_session, DEFAULT_CLOSE_ICON),
                ("Refresh", &icons.refresh, DEFAULT_REFRESH_ICON),
                ("Reorder", &icons.reorder, DEFAULT_REORDER_ICON),
            ];
            for (name, icon, glyph) in entries {
                ui.horizontal(|ui| {
                    styled_icon_button(ui, icon, glyph, self.theme.text_dim, &self.theme)
                        .on_hover_text(name);
                    ui.label(name);
                });
            }
        });
        self.story(ui, "PR and upstream badges", "Colors and glyphs used on checkout rows", |ui| {
            let i = &self.icons;
            let t = &self.theme;
            for (name, style, glyph, color) in [
                ("Open PR", &i.pr_open, DEFAULT_PR_OPEN_ICON, t.pr_open),
                ("Draft PR", &i.pr_draft, DEFAULT_PR_DRAFT_ICON, t.pr_draft),
                ("Merged PR", &i.pr_merged, DEFAULT_PR_MERGED_ICON, t.pr_merged),
                ("Closed PR", &i.pr_closed, DEFAULT_PR_CLOSED_ICON, t.pr_closed),
                ("Up to date", &i.upstream_level, DEFAULT_UPSTREAM_LEVEL_ICON, t.upstream_level),
                (
                    "Ahead / behind",
                    &i.upstream_diverged,
                    DEFAULT_UPSTREAM_DIVERGED_ICON,
                    t.upstream_diverged,
                ),
                ("Upstream removed", &i.upstream_gone, DEFAULT_UPSTREAM_GONE_ICON, t.upstream_gone),
                (
                    "No upstream",
                    &i.upstream_untracked,
                    DEFAULT_UPSTREAM_UNTRACKED_ICON,
                    t.upstream_untracked,
                ),
            ] {
                ui.horizontal(|ui| {
                    let (glyph, font, color) = resolve_icon(style, glyph, color, 10.0, 10.0, t);
                    ui.label(RichText::new(glyph).font(font).color(color));
                    ui.label(name);
                });
            }
        });
    }

    pub(super) fn indicator_variants(&self, ui: &mut egui::Ui) {
        self.story(ui, "Symbol indicators", "The alternate agent-status glyph set", |ui| {
            let theme = Theme { status_indicators: StatusIndicators::Symbols, ..self.theme };
            for state in [
                ShownState::Unknown,
                ShownState::Idle,
                ShownState::Working,
                ShownState::Pinged,
                ShownState::Done,
                ShownState::Blocked,
            ] {
                ui.horizontal(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(row_status_icon_size(&theme), egui::Sense::hover());
                    paint_status_mark(ui, state, &self.icons, rect, &theme);
                    response.on_hover_text(agent_hint(state, Some("Codex")));
                    ui.label(format!("{state:?}"));
                });
            }
        });
    }

    pub(super) fn project_headers(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        canvas.story(
            ui,
            "Project headers",
            "Expand, collapse, hover controls, and reveal the reorder grip",
            |ui| {
                ui.checkbox(&mut self.samples.reorder, "Show reorder grip");
                let project = &mut self.samples.project;
                let mut expand = false;
                row_with_trailing(
                    ui,
                    |ui| {
                        expand = sidebar::project_title(
                            ui,
                            project,
                            "alacritree",
                            self.samples.reorder,
                            true,
                            &self.icons,
                            &self.theme,
                        )
                        .0;
                    },
                    |ui| {
                        for (icon, glyph, hint) in [
                            (&self.icons.remove_project, DEFAULT_CLOSE_ICON, "remove from sidebar"),
                            (&self.icons.refresh, DEFAULT_REFRESH_ICON, "refresh worktrees"),
                            (&self.icons.new_worktree, DEFAULT_ADD_ICON, "create new worktree"),
                        ] {
                            if styled_icon_button(
                                ui,
                                icon,
                                glyph,
                                self.theme.text_muted,
                                &self.theme,
                            )
                            .on_hover_text(hint)
                            .clicked()
                            {
                                self.samples.last_action = format!("Preview: {hint}");
                            }
                        }
                        if !project.expanded {
                            attention_mark(ui, &self.icons, &self.theme)
                                .on_hover_text(ATTENTION_HINT);
                        }
                    },
                );
                if expand {
                    project.expanded = !project.expanded;
                }
                if project.expanded {
                    let checkout = &project.checkouts[0];
                    sidebar::worktree_row(
                        ui,
                        &sidebar::WorktreeRowView {
                            wt: checkout,
                            missing: None,
                            display_name: "main",
                            pr: None,
                            is_active: true,
                            is_cursor: false,
                            scroll_into_view: false,
                            status: RowStatus::live(SessionActivity::Shell),
                            deleting: false,
                            profiles: &[],
                            icons: &self.icons,
                            theme: &self.theme,
                        },
                    );
                }
                if !self.samples.last_action.is_empty() {
                    ui.label(RichText::new(&self.samples.last_action).weak().small());
                }
            },
        );
    }

    pub(super) fn worktrees(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Checkout lifecycle",
            "Main, active, keyboard focus, missing, deleting, and creating",
            |ui| {
                for (index, name) in [
                    "main",
                    "feature/active",
                    "feature/keyboard-focus",
                    "feature/missing",
                    "feature/deleting",
                ]
                .iter()
                .enumerate()
                {
                    let checkout = Checkout {
                        name: (*name).into(),
                        path: PathBuf::from("/repos").join(name),
                        head: Head { name: Some((*name).into()), ..Default::default() },
                        is_main: index == 0,
                        gone: index == 3,
                        upstream: None,
                    };
                    sidebar::worktree_row(
                        ui,
                        &sidebar::WorktreeRowView {
                            wt: &checkout,
                            missing: None,
                            display_name: name,
                            pr: None,
                            is_active: index == 1,
                            is_cursor: index == 2,
                            scroll_into_view: false,
                            status: RowStatus::live(SessionActivity::Shell),
                            deleting: index == 4,
                            profiles: &[
                                ("Development shell".into(), "zsh -l".into()),
                                ("Ubuntu (WSL)".into(), "wsl.exe -d Ubuntu".into()),
                            ],
                            icons: &self.icons,
                            theme: &self.theme,
                        },
                    );
                }
                sidebar::creating_row(ui, "feature/creating", &self.icons, &self.theme);
            },
        );
        self.story(
            ui,
            "Pull requests and tracking",
            "Hover the PR and upstream badges; right-click a row for its menu",
            |ui| {
                for (index, (state, upstream)) in [
                    (
                        PrState::Open,
                        UpstreamState::Level { upstream: "origin/feature/catalog".into() },
                    ),
                    (
                        PrState::Draft,
                        UpstreamState::Diverged {
                            upstream: "origin/feature/catalog".into(),
                            ahead: 3,
                            behind: 2,
                        },
                    ),
                    (
                        PrState::Merged,
                        UpstreamState::Gone { upstream: "origin/feature/catalog".into() },
                    ),
                    (PrState::Closed, UpstreamState::Untracked),
                ]
                .into_iter()
                .enumerate()
                {
                    let name = format!("feature/{state:?}").to_lowercase();
                    let checkout = Checkout {
                        name: name.clone(),
                        path: PathBuf::from("/repos").join(&name),
                        head: Head::default(),
                        is_main: false,
                        gone: false,
                        upstream: Some(upstream),
                    };
                    let pr = PrInfo {
                        number: 42 + index as u64,
                        base_branch: "main".into(),
                        url: String::new(),
                        state,
                    };
                    sidebar::worktree_row(
                        ui,
                        &sidebar::WorktreeRowView {
                            wt: &checkout,
                            missing: None,
                            display_name: &name,
                            pr: Some(&pr),
                            is_active: false,
                            is_cursor: false,
                            scroll_into_view: false,
                            status: RowStatus::live(SessionActivity::Shell),
                            deleting: false,
                            profiles: &[],
                            icons: &self.icons,
                            theme: &self.theme,
                        },
                    );
                }
            },
        );
    }

    pub(super) fn multiplexers(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Detached panes",
            "herdr agent states, plain shells, and Zellij shared views",
            |ui| {
                for (index, state) in [
                    PaneStatus::Idle,
                    PaneStatus::Working,
                    PaneStatus::Blocked,
                    PaneStatus::Done,
                    PaneStatus::Unknown,
                ]
                .into_iter()
                .enumerate()
                {
                    let row = fixtures::pane(index, MultiplexerKind::Herdr, Some(state), false);
                    sidebar::pane_row(ui, &row, index == 2, false, &self.icons, &self.theme);
                }
                for (index, kind) in
                    [MultiplexerKind::Herdr, MultiplexerKind::Zellij].into_iter().enumerate()
                {
                    sidebar::pane_row(
                        ui,
                        &fixtures::pane(index + 10, kind, None, true),
                        false,
                        false,
                        &self.icons,
                        &self.theme,
                    );
                }
            },
        );
        self.story(
            ui,
            "Attached sessions",
            "Hover the mark and close control to inspect attachment and detach hints",
            |ui| {
                for (index, (kind, shared)) in [
                    (MultiplexerKind::Herdr, false),
                    (MultiplexerKind::Herdr, true),
                    (MultiplexerKind::Zellij, true),
                ]
                .into_iter()
                .enumerate()
                {
                    let pane = fixtures::pane(
                        index,
                        kind,
                        (kind == MultiplexerKind::Herdr).then_some(PaneStatus::Working),
                        shared,
                    );
                    let mut row = session(
                        index as u64,
                        "Review the sidebar",
                        SessionActivity::agent(Some("Codex"), LiveState::Working),
                        index == 0,
                        index == 0,
                        false,
                    );
                    row.managed = Some(pane.managed);
                    session_row(ui, &row, false, false, false, &self.icons, &self.theme);
                }
            },
        );
    }

    pub(super) fn navigation_samples(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        canvas.story(
            ui,
            "Search and filter chips",
            "Change the query and filter combination",
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.samples.filter_query)
                        .desired_width(ui.available_width())
                        .hint_text("Search preview"),
                );
                ui.horizontal_wrapped(|ui| {
                    for (enabled, label) in self.samples.filter_toggles.iter_mut().zip([
                        "Sessions",
                        "Attention",
                        "Open PRs",
                    ]) {
                        ui.checkbox(enabled, label);
                    }
                    ui.checkbox(&mut self.samples.search_all, "Search all rows");
                });
                let mut filter = PanelFilter::new(&['s', 'a', 'o']);
                filter.on_text("/");
                filter.on_text(&self.samples.filter_query);
                for (enabled, key) in self.samples.filter_toggles.iter().zip(['s', 'a', 'o']) {
                    if *enabled {
                        filter.toggle(key);
                    }
                }
                let scope =
                    if self.samples.search_all { SearchScope::All } else { SearchScope::Filtered };
                ui.horizontal_wrapped(|ui| {
                    panel_header_filter_ui(
                        ui,
                        "Projects",
                        &filter,
                        filter.active_toggles(),
                        &self.icons.search,
                        &self.theme,
                        filter.toggles_apply(scope),
                    )
                });
                ui.separator();
                let mut matches = 0;
                for (name, flags) in [
                    ("feature/catalog", [true, true, true]),
                    ("feature/search", [false, false, true]),
                    ("main", [true, false, false]),
                    ("release", [false, false, false]),
                ] {
                    let passes_toggles = !filter.toggles_apply(scope)
                        || self
                            .samples
                            .filter_toggles
                            .iter()
                            .zip(flags)
                            .all(|(enabled, present)| !enabled || present);
                    if passes_toggles && filter.matches(name) {
                        ui.label(name);
                        matches += 1;
                    }
                }
                if matches == 0 {
                    ui.label(RichText::new("no matches").color(self.theme.text_muted));
                }
            },
        );
        canvas.story(
            ui,
            "Focus and cursor",
            "Panel focus around a row with the keyboard cursor",
            |ui| {
                let response = egui::Frame::new()
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        home_row(
                            ui,
                            true,
                            true,
                            false,
                            RowStatus::live(SessionActivity::Shell),
                            &self.icons,
                            &self.theme,
                        );
                    })
                    .response;
                paint_focus_outline(ui.ctx(), response.rect, &self.theme);
            },
        );
        for (name, style) in [
            ("Floating scrollbar", ScrollbarStyle::Floating),
            ("Solid scrollbar", ScrollbarStyle::Solid),
        ] {
            canvas.story(ui, name, "Scroll through the rows; hover the right edge", |ui| {
                apply_scrollbar_style(ui, style);
                egui::ScrollArea::vertical().max_height(130.0).show(ui, |ui| {
                    for index in 0..18 {
                        ui.label(format!("Workspace {index:02} — feature/catalog"));
                    }
                });
            });
        }
    }

    pub(super) fn tabs(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        canvas.story(
            ui,
            "Session strip",
            "Click a segment; the last segment adds a sample tab. Right-click it for profiles.",
            |ui| {
                ui.add_space(8.0);
                let tabs: Vec<_> = self
                    .samples
                    .tab_names
                    .iter()
                    .enumerate()
                    .map(|(index, title)| tab_strip::Tab {
                        id: index as u64,
                        title,
                        active: self.samples.active_tab == index as u64,
                        attention: index == 1,
                    })
                    .collect();
                let requests = tab_strip::show(ui, &tabs, &self.samples.profiles, &self.theme);
                if let Some(id) = requests.activate {
                    self.samples.active_tab = id;
                }
                if requests.spawn_default || requests.spawn_profile.is_some() {
                    let title = requests.spawn_profile.unwrap_or_else(|| "New shell".into());
                    self.samples.active_tab = self.samples.tab_names.len() as u64;
                    self.samples.tab_names.push(title);
                }
                ui.add_space(12.0);
                ui.label(format!(
                    "Active: {}",
                    self.samples.tab_names[self.samples.active_tab as usize]
                ));
                ui.label(
                    RichText::new("The second tab is requesting attention.")
                        .small()
                        .color(self.theme.attention),
                );
            },
        );
    }

    pub(super) fn git_sections(&mut self, ui: &mut egui::Ui) {
        self.canvas().story(
            ui,
            "Git sections and review controls",
            "Click the base branch or a review button",
            |ui| {
                git_panel::path_header_label(
                    ui,
                    "/home/dev/projects/alacritree",
                    self.theme.text_muted,
                    &self.theme,
                    self.theme.path_style.git_header,
                    Some("/home/dev"),
                );
                if git_panel::branch_header(ui, "feature/catalog", Some("main"), &self.theme) {
                    self.samples.dialog = Some(dialogs::Kind::Base);
                    self.samples.dialog_text.clear();
                }
                let mut gap = 10.0;
                for (index, (title, kind)) in [
                    ("Staged", ChangeKind::Added),
                    ("Unstaged", ChangeKind::Modified),
                    ("Changes vs main", ChangeKind::Deleted),
                ]
                .into_iter()
                .enumerate()
                {
                    let count = SectionCount { visible: 1, total: if index == 1 { 3 } else { 1 } };
                    let active = self.samples.last_action == format!("Review: {title}");
                    if git_panel::section(
                        ui,
                        &self.theme,
                        title,
                        &count,
                        index == 1,
                        &mut gap,
                        Some(git_panel::ReviewButton { label: "review", active }),
                        |ui| {
                            git_panel::file_row(
                                ui,
                                &FileChange { path: "src/app/catalog.rs".into(), kind },
                                &self.theme,
                                active,
                            );
                        },
                    ) {
                        self.samples.last_action =
                            if active { String::new() } else { format!("Review: {title}") };
                    }
                }
            },
        );
    }

    pub(super) fn palette(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        canvas.story(
            ui,
            "Searchable command palette",
            "Type to rank real actions and sample workspaces; click a row to select it",
            |ui| {
                let samples = &mut self.samples;
                let changed = ui
                    .add(
                        egui::TextEdit::singleline(samples.palette.query_mut())
                            .desired_width(ui.available_width())
                            .hint_text("search actions, sessions, workspaces"),
                    )
                    .changed();
                let ranked = samples.palette.rank(&samples.palette_items);
                samples.palette.reseed(changed, ranked.len());
                if changed {
                    samples.palette_selected = None;
                }
                let columns =
                    palette::PaletteColumns::new(self.theme.ui_scale, ui.available_width());
                palette::paint_palette_header(ui, &self.theme, &columns);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    if ranked.is_empty() {
                        ui.label(RichText::new("no matches").weak());
                    }
                    for (section, rows) in command_palette::group(&samples.palette_items, &ranked) {
                        palette::paint_palette_section(ui, &self.theme, &columns, section.title());
                        for index in rows {
                            let item = &samples.palette_items[index];
                            let selected =
                                samples.palette_selected.or_else(|| ranked.first().copied())
                                    == Some(index);
                            if palette::paint_palette_row(
                                ui,
                                &self.theme,
                                &self.icons,
                                &columns,
                                item,
                                None,
                                index,
                                selected,
                            )
                            .clicked()
                            {
                                samples.palette_selected = Some(index);
                                samples.last_action = format!("Selected: {}", item.primary);
                            }
                        }
                    }
                });
                if !samples.last_action.is_empty() {
                    ui.label(RichText::new(&samples.last_action).small().weak());
                }
            },
        );
        self.palette_rows(ui);
    }

    pub(super) fn tasks(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        canvas.story(
            ui,
            "Task editor",
            "Edit, complete, fold, reorder, or add tasks. Changes stay in this preview.",
            |ui| {
                if ui.button("Reset sample tasks").clicked() {
                    self.samples.tasks = fixtures::tasks();
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 400.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        self.samples.tasks.show(ui, &self.samples.shortcuts, self.theme.tasks);
                    },
                );
            },
        );
        canvas.story(
            ui,
            "Docked task progress",
            "Shares the editor's tasks, section folds, and completion state",
            |ui| {
                let view = self.samples.tasks.view();
                for section in view.sidebar_sections() {
                    tasks_panel::paint_section(ui, view, &section, &self.theme);
                }
            },
        );
        self.task_rows(ui);
    }

    pub(super) fn scratchpads(&mut self, ui: &mut egui::Ui) {
        let canvas = self.canvas();
        if ui.input(|input| input.pointer.any_pressed()) {
            self.samples.active_scratchpad = None;
        }
        for (index, title) in
            ["Empty scratchpad", "Workspace notes", "Autosave error"].into_iter().enumerate()
        {
            canvas.story(ui, title, "Editable sample; changes stay in this catalog", |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 200.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        // Several editors are visible together, so only an explicit
                        // click takes focus instead of each sample requesting it.
                        if ui.rect_contains_pointer(ui.max_rect())
                            && ui.input(|i| i.pointer.any_pressed())
                        {
                            self.samples.active_scratchpad = Some(index);
                        }
                        let focus = self.samples.active_scratchpad == Some(index);
                        scratchpad::show_editor(
                            ui,
                            10_000 + index as u64,
                            &mut self.samples.scratchpads[index],
                            focus,
                            self.theme.ui_scale,
                            self.theme.scratchpad_font,
                            self.theme.editor_text,
                            self.theme.editor_hint,
                            self.theme.error,
                        );
                    },
                );
            });
        }
    }

    pub(super) fn terminal_typography(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "ANSI palette",
            "Normal and bright colors from the terminal palette",
            |ui| {
                for colors in [&self.config.palette.normal, &self.config.palette.bright] {
                    ui.horizontal_wrapped(|ui| {
                        for (index, color) in colors.iter().enumerate() {
                            let color = rgb_to_color32(*color);
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(40.0, 26.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 0.0, color);
                            response.on_hover_text(format!(
                                "{index}: #{:02x}{:02x}{:02x}",
                                color.r(),
                                color.g(),
                                color.b()
                            ));
                        }
                    });
                }
            },
        );
        self.story(
            ui,
            "Font faces and Unicode",
            "Font samples; the live PTY grid is not hosted in the catalog",
            |ui| {
                let size = self.config.font.logical_size();
                for (name, family) in [
                    ("Regular", egui::FontFamily::Monospace),
                    ("Bold", egui::FontFamily::Name(crate::fonts::BOLD_FAMILY.into())),
                    ("Italic", egui::FontFamily::Name(crate::fonts::ITALIC_FAMILY.into())),
                    (
                        "Bold italic",
                        egui::FontFamily::Name(crate::fonts::BOLD_ITALIC_FAMILY.into()),
                    ),
                ] {
                    ui.label(
                        RichText::new(format!("{name}: Aa Bb 0123456789"))
                            .font(egui::FontId::new(size, family)),
                    );
                }
                for text in [
                    "┌──────────────┐\n│  Box drawing │\n└──────────────┘",
                    "█ ▓ ▒ ░ ⠋ ⠙ ⠹ ⠸ ⠼ ⠴",
                    "Powerline: \u{e0b0} \u{e0b2}  Symbols: ✓ ⇅ ⌫ ⌂",
                    "Unicode: café · 日本語 · Ελληνικά",
                ] {
                    ui.label(RichText::new(text).monospace().size(size));
                }
            },
        );
    }
}
