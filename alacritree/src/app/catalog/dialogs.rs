//! Modal patterns with fixed content. Frames, action buttons and dirty-state
//! warnings use the app's painters; confirmations only dismiss the sample.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Create,
    Rename,
    Delete,
    Prune,
    Close,
    Detach,
    Quit,
    Error,
    Base,
    Progress,
    Success,
}

impl Kind {
    pub(super) const ALL: [Self; 11] = [
        Self::Create,
        Self::Rename,
        Self::Delete,
        Self::Prune,
        Self::Close,
        Self::Detach,
        Self::Quit,
        Self::Error,
        Self::Base,
        Self::Progress,
        Self::Success,
    ];

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Create => "Create worktree",
            Self::Rename => "Rename project",
            Self::Delete => "Delete dirty worktree",
            Self::Prune => "Prune missing worktree",
            Self::Close => "Close busy session",
            Self::Detach => "Detach all panes",
            Self::Quit => "Quit confirmation",
            Self::Error => "Error dialog",
            Self::Base => "Base branch picker",
            Self::Progress => "Worktree creation progress",
            Self::Success => "Worktree created",
        }
    }

    fn confirm(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Rename => "Rename",
            Self::Delete => "Delete",
            Self::Prune => "Prune",
            Self::Close => "Close",
            Self::Detach => "Detach",
            Self::Quit => "Quit",
            Self::Error => "OK",
            Self::Base => "Select",
            Self::Progress => "Keep working",
            Self::Success => "Open",
        }
    }
}

impl Catalog {
    pub(super) fn dialogs(&mut self, ui: &mut egui::Ui) {
        self.canvas().story(
            ui,
            "Modal gallery",
            "Open a sample dialog. Confirmations affect only this preview.",
            |ui| {
                for kind in Kind::ALL {
                    if ui.button(kind.title()).clicked() {
                        self.samples.dialog = Some(kind);
                        self.samples.dialog_text = match kind {
                            Kind::Rename => "Alacritree".into(),
                            Kind::Base => String::new(),
                            _ => "feature/catalog".into(),
                        };
                        self.samples.dialog_force = false;
                    }
                }
            },
        );
        self.story(
            ui,
            "Delete safety states",
            "Checking, unavailable, dirty, and force-removal warnings",
            |ui| {
                let dirty = Dirty { staged: 2, modified: 3, untracked: 1 };
                for warning in [
                    modals::dirty_warning(None, false, true),
                    modals::dirty_warning(None, false, false),
                    modals::dirty_warning(Some(&dirty), false, false),
                    modals::dirty_warning(Some(&dirty), true, false),
                ]
                .into_iter()
                .flatten()
                {
                    ui.label(RichText::new(warning).small().color(self.theme.error));
                    ui.add_space(4.0);
                }
            },
        );
    }

    pub(super) fn show_sample_dialog(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.samples.dialog else { return };
        let theme = self.theme;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("catalog_sample_dialog"))
            .frame(modal_frame(&theme))
            .show(ctx, |ui| {
                // A sample remains usable at the catalog's largest font size
                // in the smallest window.
                ui.set_width((380.0 * theme.ui_scale).min(ctx.screen_rect().width() - 80.0));
                ui.spacing_mut().item_spacing.y = 6.0 * theme.ui_scale;
                egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.7).show(
                    ui,
                    |ui| {
                        self.sample_dialog_body(ui, kind, &theme);
                    },
                );
                ui.add_space(6.0 * theme.ui_scale);
                ui.horizontal_wrapped(|ui| {
                    let danger = matches!(kind, Kind::Delete | Kind::Close | Kind::Quit);
                    let enabled = !(kind == Kind::Create
                        && (self.samples.dialog_force
                            || self.samples.dialog_text.trim().is_empty()));
                    if ui
                        .add_enabled_ui(enabled, |ui| {
                            modals::modal_button(
                                ui,
                                &theme,
                                kind.confirm(),
                                if danger { theme.error } else { theme.accent },
                            )
                        })
                        .inner
                        .clicked()
                    {
                        self.samples.last_action = format!("Preview: {}", kind.confirm());
                        close = true;
                    }
                    if modals::modal_button(ui, &theme, "Cancel", theme.text_dim).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.samples.dialog = None;
        }
    }

    fn sample_dialog_body(&mut self, ui: &mut egui::Ui, kind: Kind, theme: &Theme) {
        ui.label(RichText::new(kind.title()).strong().color(if kind == Kind::Error {
            theme.error
        } else {
            theme.text
        }));
        match kind {
            Kind::Create | Kind::Rename => {
                let hint = if kind == Kind::Create {
                    "Branch from main in alacritree"
                } else {
                    "Sidebar name only. The directory is untouched."
                };
                ui.label(RichText::new(hint).small().color(theme.text_muted));
                ui.add(
                    egui::TextEdit::singleline(&mut self.samples.dialog_text)
                        .desired_width(f32::INFINITY),
                );
                if kind == Kind::Create {
                    ui.checkbox(&mut self.samples.dialog_force, "Show validation error");
                    if self.samples.dialog_force {
                        ui.label(
                            RichText::new("A branch with this name already exists.")
                                .color(theme.error)
                                .small(),
                        );
                    }
                }
            },
            Kind::Delete => {
                ui.label("Removes the worktree directory and deletes branch `feature/catalog`.");
                ui.checkbox(&mut self.samples.dialog_force, "Force removal");
                let dirty = Dirty { staged: 2, modified: 3, untracked: 1 };
                if let Some(warning) =
                    modals::dirty_warning(Some(&dirty), self.samples.dialog_force, false)
                {
                    ui.label(RichText::new(warning).small().color(theme.error));
                }
            },
            Kind::Prune => {
                ui.label(
                    "The worktree directory is already gone; this removes git's leftover metadata.",
                );
                ui.checkbox(&mut self.samples.dialog_force, "Also delete branch `feature/catalog`");
            },
            Kind::Close => {
                ui.label("A process is still running in `cargo test`. Closing this session terminates it.");
            },
            Kind::Detach => {
                ui.label("Detach 3 sessions? Their panes keep running under the multiplexer.");
            },
            Kind::Quit => {
                ui.label("Quit Alacritree? All terminal sessions will be closed.");
            },
            Kind::Error => {
                ui.label(
                    RichText::new("Could not start the selected shell: executable not found.")
                        .color(theme.error),
                );
                ui.label(
                    RichText::new("Check the program in the selected shell profile.")
                        .small()
                        .color(theme.text_muted),
                );
            },
            Kind::Base => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.samples.dialog_text)
                        .hint_text("Filter branches")
                        .desired_width(f32::INFINITY),
                );
                for (index, branch) in
                    ["Auto (main)", "main", "develop", "release/v1", "origin/feature/catalog"]
                        .into_iter()
                        .enumerate()
                {
                    if branch.contains(&self.samples.dialog_text)
                        && ui
                            .selectable_label(self.samples.dialog_branch == index, branch)
                            .clicked()
                    {
                        self.samples.dialog_branch = index;
                    }
                }
            },
            Kind::Progress | Kind::Success => {
                for (index, text) in [
                    "Verify origin",
                    "Fetch main",
                    "Create feature/catalog",
                    "Copy assistant configuration",
                    "Run checkout hooks",
                ]
                .into_iter()
                .enumerate()
                {
                    ui.horizontal_wrapped(|ui| {
                        let current = kind == Kind::Progress && index == 4;
                        if current {
                            braille_loader(ui, 12.0 * theme.ui_scale, theme.accent);
                        } else {
                            ui.label(RichText::new("•").color(theme.ok));
                        }
                        ui.label(RichText::new(text).small().color(if current {
                            theme.text
                        } else {
                            theme.text_dim
                        }));
                    });
                }
                if kind == Kind::Success {
                    ui.label(RichText::new("Created feature/catalog").color(theme.ok));
                }
            },
        }
    }
}
