//! Interactive inventory of Alacritree's production UI painters.
//!
//! Stories use fixed in-memory display data: opening the catalog never starts
//! a PTY, probes a repository, or reads persisted application state.  The
//! controls and rows themselves are the same functions the main window calls.

mod dialogs;
mod fixtures;
mod stories;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alacritree_tasks::Status;
use alacritree_tasks::tree::{Progress, Row};
use alacritree_vcs::{ChangeKind, DiffStat, FileChange};
use eframe::egui;
use egui::RichText;

use super::sidebar::{PaintedIcons, RowName, SessionRowData, home_row, session_row};
use super::widgets::agent_hint;
use super::*;

const PAGES: [Page; 15] = [
    Page::Foundations,
    Page::Icons,
    Page::Indicators,
    Page::Sidebar,
    Page::Worktrees,
    Page::Multiplexers,
    Page::Navigation,
    Page::Tabs,
    Page::Git,
    Page::Palette,
    Page::Tasks,
    Page::Scratchpad,
    Page::Dialogs,
    Page::Terminal,
    Page::Activity,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Foundations,
    Icons,
    Indicators,
    Sidebar,
    Git,
    Palette,
    Tasks,
    Activity,
    Worktrees,
    Multiplexers,
    Navigation,
    Tabs,
    Scratchpad,
    Dialogs,
    Terminal,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Foundations => "Foundations",
            Self::Indicators => "Status indicators",
            Self::Sidebar => "Project sidebar",
            Self::Git => "Git sidebar",
            Self::Palette => "Command palette",
            Self::Tasks => "Tasks",
            Self::Activity => "Activity row",
            Self::Icons => "Icons and badges",
            Self::Worktrees => "Worktrees and PRs",
            Self::Multiplexers => "Multiplexer panes",
            Self::Navigation => "Search and navigation",
            Self::Tabs => "Session tabs",
            Self::Scratchpad => "Scratchpad",
            Self::Dialogs => "Dialogs",
            Self::Terminal => "Terminal typography",
        }
    }

    fn search_text(self) -> &'static str {
        match self {
            Self::Foundations => "foundations colors typography buttons paths loader",
            Self::Indicators => {
                "status indicators agent idle working blocked done attention unknown"
            },
            Self::Sidebar => "project sidebar home sessions rows selected hover cursor shell agent",
            Self::Git => "git sidebar files changes added modified deleted renamed conflicted diff",
            Self::Palette => "command palette actions sessions selected narrow wide keys",
            Self::Tasks => "tasks pending started completed nested checkbox",
            Self::Activity => "activity status progress running failed success",
            Self::Icons => "icons badges buttons git home project upstream pull request",
            Self::Worktrees => "worktrees pr upstream branch missing deleting creating profiles",
            Self::Multiplexers => "multiplexer herdr zellij attach detach shared pane",
            Self::Navigation => "navigation search filters focus outline scrollbars drag",
            Self::Tabs => "session tabs strip profiles attention active",
            Self::Scratchpad => "scratchpad notes markdown editor empty save error",
            Self::Dialogs => {
                "dialogs modal create delete prune rename close detach quit error base branch"
            },
            Self::Terminal => "terminal typography ansi colors bold italic symbols unicode emoji",
        }
    }
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Alacritree UI catalog")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Alacritree UI catalog",
        options,
        Box::new(|cc| Ok(Box::new(Catalog::new(&cc.egui_ctx)))),
    )
}

struct Catalog {
    page: Page,
    query: String,
    canvas_width: f32,
    font_size: f32,
    show_bounds: bool,
    config: Config,
    theme: Theme,
    icons: PaintedIcons,
    unscaled_style: Arc<egui::Style>,
    samples: fixtures::Samples,
}

impl Catalog {
    fn new(ctx: &egui::Context) -> Self {
        let config = Config::default();
        let theme = Theme::from_config(&config);
        let unscaled_style = ctx.style();
        AlacritreeApp::configure_context(ctx, &config, &theme);
        let multiplexers = Multiplexers::new(&config.integrations);
        let icons = PaintedIcons::new(&config, &multiplexers);
        let samples = fixtures::Samples::new(&config);
        Self {
            page: Page::Foundations,
            query: String::new(),
            canvas_width: 560.0,
            font_size: config.font.size,
            show_bounds: false,
            config,
            theme,
            icons,
            unscaled_style,
            samples,
        }
    }

    fn rebuild_style(&mut self, ctx: &egui::Context) {
        if (self.config.font.size - self.font_size).abs() < f32::EPSILON {
            return;
        }
        self.config.font.size = self.font_size;
        self.theme = Theme::from_config(&self.config);
        // Context setup scales the existing spacing, so every slider change
        // must start from the same unscaled style.
        ctx.set_style(self.unscaled_style.clone());
        AlacritreeApp::configure_context(ctx, &self.config, &self.theme);
    }

    fn navigation(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("catalog_navigation").resizable(false).exact_width(220.0).show(
            ctx,
            |ui| {
                ui.add_space(8.0);
                ui.heading("Alacritree UI");
                ui.label(RichText::new("Interactive component inventory").small().weak());
                ui.add_space(10.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Filter components…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                let needle = self.query.to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for page in PAGES {
                        if !needle.is_empty() && !page.search_text().contains(&needle) {
                            continue;
                        }
                        if ui.selectable_label(self.page == page, page.title()).clicked() {
                            self.page = page;
                        }
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(RichText::new("Uses production painters").small().weak());
                });
            },
        );
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Canvas width");
            ui.add(egui::Slider::new(&mut self.canvas_width, 280.0..=820.0).suffix(" px"));
            ui.separator();
            ui.label("Font");
            ui.add(egui::Slider::new(&mut self.font_size, 8.0..=24.0).suffix(" pt"));
            ui.separator();
            ui.checkbox(&mut self.show_bounds, "Show bounds");
        });
    }

    fn story(&self, ui: &mut egui::Ui, title: &str, note: &str, add: impl FnOnce(&mut egui::Ui)) {
        self.canvas().story(ui, title, note, add);
    }

    fn canvas(&self) -> Canvas {
        Canvas { theme: self.theme, canvas_width: self.canvas_width, show_bounds: self.show_bounds }
    }
}

#[derive(Clone, Copy)]
struct Canvas {
    theme: Theme,
    canvas_width: f32,
    show_bounds: bool,
}

impl Canvas {
    fn story(&self, ui: &mut egui::Ui, title: &str, note: &str, add: impl FnOnce(&mut egui::Ui)) {
        ui.add_space(14.0);
        ui.label(RichText::new(title).strong());
        if !note.is_empty() {
            ui.label(RichText::new(note).small().weak());
        }
        ui.add_space(5.0);
        let frame = egui::Frame::new()
            .fill(self.theme.sidebar_bg)
            .stroke(egui::Stroke::new(
                1.0_f32,
                if self.show_bounds { self.theme.accent } else { self.theme.sidebar_border },
            ))
            .inner_margin(12.0)
            .corner_radius(4.0);
        ui.push_id(title, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(self.canvas_width.min(ui.available_width()));
                add(ui);
            })
        });
    }
}

impl Catalog {
    fn foundations(&self, ui: &mut egui::Ui) {
        self.story(ui, "Typography", "Heading, normal, small, muted, and monospace text", |ui| {
            ui.heading("Workspace heading");
            ui.label("Terminal and sidebar body text");
            ui.label(RichText::new("Secondary row label").small().color(self.theme.text_dim));
            ui.label(RichText::new("Muted metadata").small().color(self.theme.text_muted));
            ui.label(RichText::new("cargo check --workspace").monospace().color(self.theme.accent));
        });
        self.story(
            ui,
            "Resolved palette",
            "Colors derived from the active Alacritree config",
            |ui| {
                let colors = [
                    ("background", self.theme.terminal_bg),
                    ("sidebar", self.theme.sidebar_bg),
                    ("text", self.theme.text),
                    ("dim", self.theme.text_dim),
                    ("accent", self.theme.accent),
                    ("attention", self.theme.attention),
                    ("error", self.theme.error),
                    ("success", self.theme.ok),
                ];
                let cell_size = egui::vec2(
                    80.0 * self.theme.ui_scale,
                    34.0 * self.theme.ui_scale
                        + ui.spacing().item_spacing.y
                        + ui.text_style_height(&egui::TextStyle::Small),
                );
                ui.horizontal_wrapped(|ui| {
                    for (name, color) in colors {
                        ui.allocate_ui_with_layout(
                            cell_size,
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(62.0, 34.0) * self.theme.ui_scale,
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, 4.0, color);
                                ui.label(RichText::new(name).small());
                            },
                        );
                    }
                });
            },
        );
        self.story(
            ui,
            "Controls",
            "Native egui controls beside Alacritree's framed and icon buttons",
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    let _ = framed_button(
                        ui,
                        &self.theme,
                        RichText::new("review").small(),
                        egui::vec2(5.0, 2.0),
                    );
                    let _ = styled_icon_button(
                        ui,
                        &self.icons.refresh,
                        DEFAULT_REFRESH_ICON,
                        self.theme.text_muted,
                        &self.theme,
                    );
                    let _ = styled_icon_button(
                        ui,
                        &self.icons.search,
                        DEFAULT_SEARCH_ICON,
                        self.theme.text_muted,
                        &self.theme,
                    );
                    let mut enabled = true;
                    ui.checkbox(&mut enabled, "enabled");
                    ui.add_enabled(false, egui::Button::new("disabled"));
                });
            },
        );
        self.story(ui, "Path styles", "The same path layout used by sidebar and Git rows", |ui| {
            for style in [PathStyle::Full, PathStyle::Fish, PathStyle::Zed] {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{style:?}")).small().color(self.theme.text_muted),
                    );
                    let text = path_text(
                        ui,
                        "/home/dev/projects/alacritree/src/app/catalog.rs",
                        self.theme.text,
                        &self.theme,
                        style,
                        egui::FontFamily::Proportional,
                        Some("/home/dev"),
                    );
                    ui.add(egui::Label::new(text).truncate());
                });
            }
        });
    }

    fn indicators(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Agent lifecycle",
            "Hover each mark to inspect the intended meaning",
            |ui| {
                // A centered vertical UI takes the remaining row width unless
                // bounded. Give each mark a cell the wrapping row can place.
                let cell_size = egui::vec2(
                    80.0 * self.theme.ui_scale,
                    row_status_icon_size(&self.theme).y
                        + ui.spacing().item_spacing.y
                        + ui.text_style_height(&egui::TextStyle::Small),
                );
                ui.horizontal_wrapped(|ui| {
                    for state in [
                        ShownState::Unknown,
                        ShownState::Idle,
                        ShownState::Working,
                        ShownState::Pinged,
                        ShownState::Done,
                        ShownState::Blocked,
                    ] {
                        ui.allocate_ui_with_layout(
                            cell_size,
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                let (rect, response) = ui.allocate_exact_size(
                                    row_status_icon_size(&self.theme),
                                    egui::Sense::hover(),
                                );
                                paint_status_mark(ui, state, &self.icons, rect, &self.theme);
                                response.on_hover_text(agent_hint(state, Some("Codex")));
                                ui.label(RichText::new(format!("{state:?}")).small());
                            },
                        );
                    }
                });
            },
        );
        self.story(ui, "Loaders and attention", "Animated work and latched attention", |ui| {
            ui.horizontal_wrapped(|ui| {
                braille_loader(ui, 14.0 * self.theme.ui_scale, self.theme.accent);
                ui.label("Working");
                attention_mark(ui, &self.icons, &self.theme).on_hover_text(ATTENTION_HINT);
                ui.label("Needs attention");
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        self.project_headers(ui);
        self.story(ui, "Home row states", "Inactive, active, and keyboard-cursor variants", |ui| {
            home_row(
                ui,
                false,
                false,
                false,
                RowStatus::live(SessionActivity::Shell),
                &self.icons,
                &self.theme,
            );
            home_row(
                ui,
                true,
                false,
                false,
                RowStatus::live(SessionActivity::agent(Some("Codex"), LiveState::Working)),
                &self.icons,
                &self.theme,
            );
            home_row(
                ui,
                false,
                true,
                false,
                RowStatus::live(SessionActivity::Shell),
                &self.icons,
                &self.theme,
            );
        });
        self.story(
            ui,
            "Session row states",
            "Shell, selected session, agent states, attention, and long names",
            |ui| {
                let rows = [
                    session(1, "zsh", SessionActivity::Shell, false, false, false),
                    session(2, "server logs", SessionActivity::Shell, true, true, false),
                    session(
                        3,
                        "Codex implement UI catalog",
                        SessionActivity::agent(Some("Codex"), LiveState::Working),
                        false,
                        false,
                        false,
                    ),
                    session(
                        4,
                        "Claude waiting on a decision",
                        SessionActivity::agent(Some("Claude"), LiveState::Blocked),
                        false,
                        false,
                        false,
                    ),
                    session(
                        5,
                        "very-long-session-name-that-demonstrates-truncation-in-a-narrow-sidebar",
                        SessionActivity::agent(Some("Codex"), LiveState::Idle),
                        false,
                        false,
                        true,
                    ),
                ];
                for (index, row) in rows.iter().enumerate() {
                    session_row(ui, row, index == 3, false, false, &self.icons, &self.theme);
                }
            },
        );
    }

    fn git(&mut self, ui: &mut egui::Ui) {
        self.git_sections(ui);
        self.story(
            ui,
            "Working-tree changes",
            "Every file-change state, including an active row",
            |ui| {
                for (index, kind) in [
                    ChangeKind::Added,
                    ChangeKind::Modified,
                    ChangeKind::Deleted,
                    ChangeKind::Renamed,
                    ChangeKind::Untracked,
                    ChangeKind::Conflicted,
                ]
                .into_iter()
                .enumerate()
                {
                    let change = FileChange {
                        path: format!("src/components/{kind:?}.rs").to_lowercase(),
                        kind,
                    };
                    git_panel::file_row(ui, &change, &self.theme, index == 1);
                }
            },
        );
        self.story(
            ui,
            "Branch diff",
            "Addition-only, deletion-only, mixed, and long paths",
            |ui| {
                for (index, stat) in [
                    DiffStat { path: "src/new.rs".into(), additions: 42, deletions: 0 },
                    DiffStat { path: "src/removed.rs".into(), additions: 0, deletions: 18 },
                    DiffStat {
                        path: "src/app/a-very-long-component-name-that-needs-to-elide.rs".into(),
                        additions: 12,
                        deletions: 7,
                    },
                ]
                .iter()
                .enumerate()
                {
                    git_panel::branch_diff_row(ui, stat, &self.theme, index == 2);
                }
            },
        );
    }

    fn palette_rows(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Palette rows",
            "Action, workspace, and session rows at the selected canvas width",
            |ui| {
                let width = ui.available_width();
                let cols = palette::PaletteColumns::new(self.theme.ui_scale, width);
                let items = [
                    PaletteItem::workspace(None, "Home".into(), "switch workspace".into()),
                    PaletteItem::create_worktree(
                        PathBuf::from("/repos/alacritree"),
                        "New worktree".into(),
                        "alacritree".into(),
                    ),
                    PaletteItem::session(
                        7,
                        "Implement component catalog".into(),
                        "Codex · feature/ui-catalog".into(),
                        "native · working".into(),
                        "Open session".into(),
                        Some("codex"),
                        None,
                        None,
                    ),
                ];
                for (index, item) in items.iter().enumerate() {
                    let mark =
                        (index == 2).then(|| (ShownState::Working, "Codex is working".into()));
                    palette::paint_palette_row(
                        ui,
                        &self.theme,
                        &self.icons,
                        &cols,
                        item,
                        mark.as_ref(),
                        index,
                        index == 1,
                    );
                }
            },
        );
    }

    fn task_rows(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Task row states",
            "Pending, started, completed, nested, and wrapping",
            |ui| {
                let rows = [
                    task_row("pending", 0, "Design the catalog navigation", Status::Pending, false),
                    task_row(
                        "started",
                        0,
                        "Build the interactive component canvas",
                        Status::Pending,
                        true,
                    ),
                    task_row(
                        "nested",
                        1,
                        "Exercise narrow layouts and wrapped task descriptions",
                        Status::Pending,
                        false,
                    ),
                    task_row(
                        "done",
                        0,
                        "Inventory the production painters",
                        Status::Completed,
                        true,
                    ),
                ];
                for row in &rows {
                    tasks_panel::paint_row(ui, row, &self.theme);
                }
            },
        );
    }

    fn activity(&self, ui: &mut egui::Ui) {
        self.story(
            ui,
            "Activity states",
            "The fixed-height row shown at the foot of the projects sidebar",
            |ui| {
                ui.set_min_height(150.0);
                let states = [
                    crate::activity::StatusLine {
                        running: true,
                        text: "Scanning projects 2/5".into(),
                        failed: false,
                        tooltip: vec!["Scanning projects 2/5".into()],
                        repaint_after: Some(Duration::from_secs(1)),
                    },
                    crate::activity::StatusLine {
                        running: false,
                        text: "PRs refreshed · just now".into(),
                        failed: false,
                        tooltip: Vec::new(),
                        repaint_after: None,
                    },
                    crate::activity::StatusLine {
                        running: false,
                        text: "PR refresh failed".into(),
                        failed: true,
                        tooltip: vec!["alacritree: authentication failed".into()],
                        repaint_after: None,
                    },
                ];
                for (index, line) in states.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.allocate_ui(egui::vec2(ui.available_width(), 42.0), |ui| {
                            activity_row::show(ui, line, &self.theme)
                        });
                    });
                }
            },
        );
    }
}

impl Catalog {
    fn show(&mut self, ctx: &egui::Context) {
        self.rebuild_style(ctx);
        self.navigation(ctx);
        egui::TopBottomPanel::top("catalog_toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(self.page.title());
                ui.label(
                    RichText::new(
                        "Resize the canvas and interact with controls to inspect real application \
                         behavior.",
                    )
                    .weak(),
                );
                match self.page {
                    Page::Foundations => self.foundations(ui),
                    Page::Icons => self.icons_page(ui),
                    Page::Indicators => {
                        self.indicators(ui);
                        self.indicator_variants(ui);
                    },
                    Page::Sidebar => self.sidebar(ui),
                    Page::Git => self.git(ui),
                    Page::Palette => self.palette(ui),
                    Page::Tasks => self.tasks(ui),
                    Page::Activity => self.activity(ui),
                    Page::Worktrees => self.worktrees(ui),
                    Page::Multiplexers => self.multiplexers(ui),
                    Page::Navigation => self.navigation_samples(ui),
                    Page::Tabs => self.tabs(ui),
                    Page::Scratchpad => self.scratchpads(ui),
                    Page::Dialogs => self.dialogs(ui),
                    Page::Terminal => self.terminal_typography(ui),
                }
                ui.add_space(30.0);
            });
        });
        self.show_sample_dialog(ctx);
    }
}

impl eframe::App for Catalog {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.show(ctx);
    }
}

fn session(
    id: SessionId,
    name: &str,
    activity: SessionActivity,
    is_active: bool,
    is_displayed: bool,
    needs_attention: bool,
) -> SessionRowData {
    SessionRowData {
        id,
        name: RowName::plain(name.into()),
        needs_attention,
        done: false,
        activity,
        is_active,
        is_displayed,
        managed: None,
    }
}

fn task_row(id: &str, depth: usize, text: &str, status: Status, started: bool) -> Row {
    Row { id: id.into(), depth, text: text.into(), status, started, subtasks: Progress::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(ctx: &egui::Context, catalog: &Catalog) -> egui::FullOutput {
        // The palette wraps into several rows at the largest font size.
        let height = if catalog.page == Page::Foundations { 2000.0 } else { 760.0 };
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1120.0, height),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| match catalog.page {
                    Page::Foundations => catalog.foundations(ui),
                    Page::Indicators => catalog.indicators(ui),
                    Page::Activity => catalog.activity(ui),
                    _ => unreachable!(),
                });
            },
        )
    }

    #[test]
    fn font_slider_round_trip_restores_spacing() {
        let ctx = egui::Context::default();
        let mut catalog = Catalog::new(&ctx);
        let initial = ctx.style();
        let original_size = catalog.font_size;
        for size in [24.0, 16.0, 8.0, 20.0, original_size] {
            catalog.font_size = size;
            catalog.rebuild_style(&ctx);
        }
        assert_eq!(ctx.style().spacing, initial.spacing);
        assert_eq!(ctx.style().text_styles, initial.text_styles);
    }

    #[test]
    fn indicator_labels_stay_readable_at_every_canvas_size() {
        let ctx = egui::Context::default();
        let mut catalog = Catalog::new(&ctx);
        catalog.page = Page::Indicators;
        for size in [8.0, 11.25, 24.0] {
            catalog.font_size = size;
            catalog.rebuild_style(&ctx);
            for width in [280.0, 560.0, 820.0] {
                catalog.canvas_width = width;
                let output = render(&ctx, &catalog);
                let texts = super::super::tests::painted_text_rects(&output.shapes);
                let mut previous = Vec::<egui::Rect>::new();
                for name in ["Unknown", "Idle", "Working", "Pinged", "Done", "Blocked"] {
                    let rect = texts.iter().find(|(text, _)| text == name).expect(name).1;
                    assert!(
                        rect.height() < 2.0 * catalog.theme.font_normal,
                        "{name} wrapped within its cell at font {size}, canvas {width}: {rect:?}"
                    );
                    assert!(rect.left() >= 0.0 && rect.right() <= width + 20.0);
                    assert!(previous.iter().all(|other| !rect.intersects(*other)));
                    previous.push(rect);
                }
            }
        }
    }

    #[test]
    fn palette_labels_stay_in_their_cells() {
        let ctx = egui::Context::default();
        let mut catalog = Catalog::new(&ctx);
        for size in [8.0, 11.25, 24.0] {
            catalog.font_size = size;
            catalog.rebuild_style(&ctx);
            for width in [280.0, 560.0, 820.0] {
                catalog.canvas_width = width;
                let output = render(&ctx, &catalog);
                let texts = super::super::tests::painted_text_rects(&output.shapes);
                let mut previous = Vec::<egui::Rect>::new();
                for name in [
                    "background",
                    "sidebar",
                    "text",
                    "dim",
                    "accent",
                    "attention",
                    "error",
                    "success",
                ] {
                    let rect = texts.iter().find(|(text, _)| text == name).expect(name).1;
                    assert!(rect.height() < 2.0 * catalog.theme.font_normal, "{name} wrapped");
                    assert!(rect.left() >= 0.0 && rect.right() <= width + 20.0);
                    assert!(previous.iter().all(|other| !rect.intersects(*other)));
                    previous.push(rect);
                }
            }
        }
    }

    #[test]
    fn activity_previews_have_no_id_collisions() {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.warn_on_id_clash = true);
        let mut catalog = Catalog::new(&ctx);
        catalog.page = Page::Activity;
        for _ in 0..2 {
            let output = render(&ctx, &catalog);
            let texts = super::super::tests::painted_text_rects(&output.shapes);
            for expected in
                ["Scanning projects 2/5", "PRs refreshed · just now", "PR refresh failed"]
            {
                assert!(texts.iter().any(|(text, _)| text == expected), "{expected} missing");
            }
            assert!(
                texts.iter().all(|(text, _)| !text.contains("use of") || !text.contains(" ID ")),
                "egui reported an ID collision: {texts:?}"
            );
        }
    }

    #[test]
    fn every_page_renders_without_id_collisions() {
        let ctx = egui::Context::default();
        ctx.options_mut(|options| options.warn_on_id_clash = true);
        let mut catalog = Catalog::new(&ctx);
        for size in [8.0, 11.25, 24.0] {
            catalog.font_size = size;
            for width in [760.0, 1120.0] {
                catalog.canvas_width = if width == 760.0 { 280.0 } else { 820.0 };
                for page in PAGES {
                    catalog.page = page;
                    let output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 760.0),
                            )),
                            ..Default::default()
                        },
                        |ctx| catalog.show(ctx),
                    );
                    let texts = super::super::tests::painted_text_rects(&output.shapes);
                    assert!(
                        texts.iter().any(|(text, _)| text == page.title()),
                        "{page:?} is missing"
                    );
                    assert!(
                        texts
                            .iter()
                            .all(|(text, _)| !text.contains("use of") || !text.contains(" ID ")),
                        "ID collision on {page:?}, font {size}, width {width}: {texts:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_dialog_renders_and_dismisses_without_app_actions() {
        let ctx = egui::Context::default();
        let mut catalog = Catalog::new(&ctx);
        catalog.page = Page::Dialogs;
        for kind in dialogs::Kind::ALL {
            catalog.samples.dialog = Some(kind);
            for _ in 0..2 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| catalog.show(ctx));
            }
            let output = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| catalog.show(ctx),
            );
            let texts = super::super::tests::painted_text_rects(&output.shapes);
            assert!(
                texts.iter().all(|(text, _)| !text.contains("use of") || !text.contains(" ID "))
            );
            assert!(catalog.samples.dialog.is_none(), "{kind:?} did not close");
            assert!(catalog.samples.last_action.is_empty());
        }
    }

    #[test]
    fn scratchpad_keeps_typing_focus_and_releases_it_to_catalog_search() {
        let ctx = egui::Context::default();
        let mut catalog = Catalog::new(&ctx);
        catalog.page = Page::Scratchpad;
        let frame = |catalog: &mut Catalog, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1120.0, 1000.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| catalog.show(ctx),
            )
        };
        let click = |catalog: &mut Catalog, pos| {
            for pressed in [true, false] {
                frame(
                    catalog,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        };
        frame(&mut catalog, Vec::new());
        let output = frame(&mut catalog, Vec::new());
        let texts = super::super::tests::painted_text_rects(&output.shapes);
        let editor = texts.iter().find(|(text, _)| text.starts_with("Start typing.")).unwrap().1;
        let search = texts.iter().find(|(text, _)| text == "Filter components…").unwrap().1;
        let other_notes = catalog.samples.scratchpads[1].text().to_owned();
        click(&mut catalog, editor.center());
        frame(&mut catalog, Vec::new());
        frame(&mut catalog, vec![egui::Event::Text("Local note.".into())]);
        assert_eq!(catalog.samples.scratchpads[0].text(), "Local note.");
        assert_eq!(catalog.samples.scratchpads[1].text(), other_notes);

        click(&mut catalog, search.center());
        frame(&mut catalog, vec![egui::Event::Text("tasks".into())]);
        assert_eq!(catalog.query, "tasks");
        assert_eq!(catalog.samples.scratchpads[0].text(), "Local note.");
    }
}
