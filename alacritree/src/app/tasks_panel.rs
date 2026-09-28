//! The workspace's tasks docked at the bottom of a sidebar, read-only, so an
//! agent's progress shows beside the terminal. `[ui.tasks] sidebar` picks the
//! sidebar. The section draws the workspace's tasks tab when one is open, so
//! the two never list twice, and keeps a listing of its own otherwise.
//! Either lists only while drawn. `ToggleTasksSidebar` hides the section, and
//! a right-click on its heading opens the tasks tab.

use alacritree_tasks::Status;
use alacritree_tasks::tree::{self, Row, Section};
use egui::{Sense, StrokeKind, Vec2, vec2};

use crate::config::TasksSidebar;
use crate::tasks::view::{self as tasks_view, TasksView};

use super::*;

/// A double-click on the divider fits the section to its tasks, up to this
/// share of the sidebar's height.
const FIT_SHARE: f32 = 0.5;

/// `[ui.tasks.sidebar_colors]` with every color resolved against the
/// sidebar's.
#[derive(Clone, Copy, Debug)]
pub(super) struct Colors {
    pub(super) section: Color32,
    pub(super) count: Color32,
    pub(super) chevron: Color32,
    pub(super) bar: Color32,
    pub(super) bar_background: Color32,
    pub(super) pending: Color32,
    pub(super) started: Color32,
    pub(super) completed: Color32,
    pub(super) pending_box: Color32,
    pub(super) started_box: Color32,
    pub(super) completed_box: Color32,
    pub(super) tick: Color32,
}

/// The listing the sidebar keeps for a workspace with no tasks tab open.
pub(super) struct TasksPanel {
    pub(super) workspace: WorkspaceKey,
    pub(super) view: TasksView,
}

impl AlacritreeApp {
    /// Whether the sidebar on `side` docks the tasks. When it does, the
    /// sidebar gets a listing for the workspace on screen unless a tasks tab
    /// there already has one.
    pub(super) fn docks_tasks(&mut self, side: TasksSidebar) -> bool {
        if !self.show_tasks_sidebar
            || self.config.ui.tasks.sidebar != side
            || !self.config.integrations.tasks_enabled()
        {
            return false;
        }
        let ws = &self.current_workspace;
        let kept = self.tasks_panel.as_ref().is_some_and(|p| p.workspace == *ws);
        if !kept && self.tasks_session_index(ws).is_none() {
            let view = self.new_tasks_view(ws);
            self.tasks_panel = Some(TasksPanel { workspace: ws.clone(), view });
        }
        true
    }
}

impl Action for action::ToggleTasksSidebar {
    fn run(&self, app: &mut AlacritreeApp, _: &Context, _: ActionOrigin) {
        app.show_tasks_sidebar = !app.show_tasks_sidebar;
        app.persist_sidebars();
    }
}

/// The view the sidebar draws for `ws`. Takes the fields rather than the app
/// so the sidebar can keep reading the rest of it while drawing.
pub(super) fn sidebar_view<'a>(
    sessions: &'a mut SessionList,
    panel: &'a mut Option<TasksPanel>,
    ws: &WorkspaceKey,
) -> Option<&'a mut TasksView> {
    let tab = sessions
        .iter_mut()
        .find(|s| s.working_directory == *ws && matches!(s.kind, SessionKind::Tasks))
        .and_then(|s| s.tasks.as_mut());
    tab.or_else(|| panel.as_mut().filter(|p| p.workspace == *ws).map(|p| &mut p.view))
}

/// The egui id of the section docked in `side`, which keeps its height.
pub(super) fn panel_id(side: TasksSidebar) -> egui::Id {
    egui::Id::new(("sidebar_tasks", <&'static str>::from(side)))
}

/// Lists when due and, while the workspace has tasks, docks them at the
/// bottom of `ui` in a section the user can resize. Double-clicking the
/// divider above it fits the section to its tasks. Runs before whatever
/// fills the rest of `ui`. Returns whether a right-click on the heading asked
/// for the tasks tab.
pub(super) fn show(
    ui: &mut egui::Ui,
    view: &mut TasksView,
    theme: &Theme,
    side: TasksSidebar,
) -> bool {
    view.tick();
    ui.ctx().request_repaint_after(tasks_view::RELOAD_EVERY);
    let sections = view.sidebar_sections();
    if sections.is_empty() {
        return false;
    }
    let id = panel_id(side);
    let (fit_id, content_id) = (id.with("fit"), id.with("content"));
    let sidebar = ui.available_height();
    let least = 2.0 * ui.spacing().interact_size.y;
    let most = (sidebar * 0.8).max(least);
    let mut panel = egui::TopBottomPanel::bottom(id)
        .resizable(true)
        .frame(Frame::default())
        .default_height(sidebar * 0.4)
        .height_range(least..=most);
    // The content's height is known only once it is drawn, so a fit waits
    // for the frame after the double-click. One frame at an exact height is
    // enough, since the panel keeps the height it last had.
    if ui.ctx().data_mut(|d| d.remove_temp::<()>(fit_id)).is_some() {
        let content = ui.ctx().data(|d| d.get_temp::<f32>(content_id)).unwrap_or(most);
        panel = panel.exact_height(content.clamp(least, (sidebar * FIT_SHARE).max(least)));
    }
    let mut open_tab = false;
    let shown = panel.show_inside(ui, |ui| {
        let top = ui.cursor().top();
        ui.add_space(6.0 * theme.ui_scale);
        row_with_trailing(
            ui,
            |ui| open_tab = heading(ui, theme),
            |ui| filter_button(ui, view, theme),
        );
        if let Some(e) = view.load_error() {
            ui.add(egui::Label::new(RichText::new(e).color(theme.error).small()).wrap());
        }
        let above = ui.cursor().top() - top;
        let area = ScrollArea::vertical().auto_shrink([false, false]).min_scrolled_height(0.0);
        let rows = area.show(ui, |ui| {
            for section in &sections {
                paint_section(ui, view, section, theme);
            }
        });
        above + rows.content_size.y
    });
    ui.ctx().data_mut(|d| d.insert_temp(content_id, shown.inner));
    // Sensing clicks on the strip egui drags to resize: egui picks a click
    // target and a drag target apart, so the drag still resizes.
    let (rect, grab) = (shown.response.rect, ui.style().interaction.resize_grab_radius_side);
    let divider =
        egui::Rect::from_x_y_ranges(rect.x_range(), rect.top() - grab..=rect.top() + grab);
    if ui.interact(divider, id.with("divider"), Sense::click()).double_clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(fit_id, ()));
        ui.ctx().request_repaint();
    }
    open_tab
}

/// The section's title. Returns whether it was right-clicked.
fn heading(ui: &mut egui::Ui, theme: &Theme) -> bool {
    let text = RichText::new("Tasks").color(theme.text).strong();
    let label = ui.add(egui::Label::new(text).sense(Sense::click()));
    icon_tooltip(label, "Right-click to open the tasks tab", theme.icon_tooltips)
        .secondary_clicked()
}

/// Hides or shows the completed tasks, in the sidebar and every tasks tab.
/// Drawn like the git panel's review buttons, brighter while it filters.
fn filter_button(ui: &mut egui::Ui, view: &mut TasksView, theme: &Theme) {
    let (label, color) = match view.hides_completed() {
        true => ("show completed", theme.text),
        false => ("hide completed", theme.text_muted),
    };
    let s = theme.ui_scale;
    let text = RichText::new(label).color(color).small();
    let button = framed_button(ui, theme, text, vec2(4.0 * s, 1.0 * s));
    let hint = "Filters the tasks tab as well";
    if icon_tooltip(button, hint, theme.icon_tooltips).clicked() {
        view.toggle_completed();
    }
}

/// A heading that folds the section, the same fold the tab shows, then a
/// bar of how much of it is done and the rows the filter leaves.
pub(super) fn paint_section(
    ui: &mut egui::Ui,
    view: &mut TasksView,
    section: &Section,
    theme: &Theme,
) {
    let (s, c) = (theme.ui_scale, theme.tasks_sidebar);
    let open = !view.is_collapsed(&section.node);
    let done = section.rows.iter().filter(|r| r.status == Status::Completed).count();
    let total = section.rows.len();
    ui.add_space(4.0 * s);
    let rect = row_with_trailing(
        ui,
        |ui| {
            let size = vec2(12.0 * s, ui.spacing().interact_size.y);
            let (chevron, _) = ui.allocate_exact_size(size, Sense::hover());
            let stroke = Stroke::new(1.5 * s, c.chevron);
            tasks_view::paint_chevron(ui, chevron.center(), open, stroke);
            let name = RichText::new(view.short_name(&section.node)).color(c.section).small();
            ui.add(egui::Label::new(name.strong()).truncate());
        },
        |ui| {
            ui.label(RichText::new(format!("{done}/{total}")).color(c.count).small());
        },
    );
    let header = ui.interact(rect, ui.id().with(("tasks-section", &section.node)), Sense::click());
    if header.on_hover_text(&section.node).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    {
        view.toggle_collapsed(&section.node);
    }
    paint_bar(ui, done as f32 / total as f32, &c, 3.0 * s);
    if !open {
        return;
    }
    let rows = match view.hides_completed() {
        true => tree::without_completed(section.rows.clone()),
        false => section.rows.clone(),
    };
    rows.iter().for_each(|row| paint_row(ui, row, theme));
}

fn paint_bar(ui: &mut egui::Ui, done: f32, c: &Colors, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let radius = height / 2.0;
    ui.painter().rect_filled(rect, radius, c.bar_background);
    let filled = egui::Rect::from_min_size(rect.min, vec2(rect.width() * done, height));
    ui.painter().rect_filled(filled, radius, c.bar);
}

pub(super) fn paint_row(ui: &mut egui::Ui, row: &Row, theme: &Theme) {
    let c = &theme.tasks_sidebar;
    let color = match (row.status, row.started) {
        (Status::Completed, _) => c.completed,
        (Status::Pending, true) => c.started,
        (Status::Pending, false) => c.pending,
    };
    ui.horizontal_top(|ui| {
        ui.add_space(row.depth as f32 * 12.0 * theme.ui_scale);
        paint_box(ui, row, c);
        ui.add(egui::Label::new(RichText::new(&row.text).color(color).small()).wrap());
    });
}

/// A box, ticked once the task is done and filled while it is under way.
fn paint_box(ui: &mut egui::Ui, row: &Row, c: &Colors) {
    let side = ui.text_style_height(&egui::TextStyle::Small);
    let (slot, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let r = egui::Rect::from_center_size(slot.center(), Vec2::splat(side * 0.7));
    let painter = ui.painter();
    match (row.status, row.started) {
        (Status::Completed, _) => {
            painter.rect_filled(r, 2.0, c.completed_box);
            let (w, h) = (r.width(), r.height());
            let tick = vec![
                r.left_center() + vec2(0.2 * w, 0.0),
                r.center_bottom() - vec2(0.1 * w, 0.25 * h),
                r.right_top() + vec2(-0.2 * w, 0.25 * h),
            ];
            painter.add(egui::Shape::line(tick, Stroke::new(1.5_f32, c.tick)));
        },
        (Status::Pending, true) => {
            painter.rect_stroke(r, 2.0, Stroke::new(1.0_f32, c.started_box), StrokeKind::Inside);
            painter.rect_filled(r.shrink(0.25 * r.width()), 1.0, c.started_box);
        },
        (Status::Pending, false) => {
            painter.rect_stroke(r, 2.0, Stroke::new(1.0_f32, c.pending_box), StrokeKind::Inside);
        },
    }
}

#[cfg(test)]
mod tests {
    use egui::containers::panel::PanelState;
    use egui::{Event, Modifiers, PointerButton, Pos2, RawInput};

    use super::*;

    const SIDEBAR: f32 = 600.0;

    /// The section alone in a sidebar-tall pane, drawn with real egui input
    /// over a store that holds `count` tasks.
    struct Harness {
        ctx: Context,
        view: TasksView,
        theme: Theme,
        time: f64,
        painted: Vec<(String, egui::Rect)>,
    }

    impl Harness {
        fn listing(count: usize) -> Self {
            Self::of((0..count).map(|i| task(&format!("t{i}"))).collect())
        }

        fn of(tasks: Vec<alacritree_tasks::Task>) -> Self {
            let view = TasksView::new(
                crate::tasks::backend::Backend::Fake(
                    alacritree_tasks::fake::FakeBackend::with_tasks(tasks),
                ),
                crate::tasks::view::Scope::for_workspace(None, None),
                None,
                Vec::new(),
                crate::tasks::view::Prefs::default(),
                None,
            );
            let theme = Theme::from_config(&Config::default());
            let mut h =
                Self { ctx: Context::default(), view, theme, time: 0.0, painted: Vec::new() };
            let deadline = Instant::now() + Duration::from_secs(10);
            while h.height().is_none() {
                assert!(Instant::now() < deadline, "the listing never landed");
                h.frame(Vec::new());
                std::thread::yield_now();
            }
            h
        }

        /// The texts the frame painted.
        fn frame(&mut self, events: Vec<Event>) -> Vec<String> {
            self.time += 0.05;
            let input = RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(300.0, SIDEBAR))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (view, theme) = (&mut self.view, &self.theme);
            let output = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(Frame::default()).show(ctx, |ui| {
                    show(ui, view, theme, TasksSidebar::Left);
                });
            });
            self.painted = crate::app::tests::painted_text_rects(&output.shapes);
            self.painted.iter().map(|(text, _)| text.clone()).collect()
        }

        /// Clicks `text` where the last frame painted it.
        fn click(&mut self, text: &str) -> Vec<String> {
            let drawn = self.painted.iter().find(|(t, _)| t == text);
            let pos = drawn.unwrap_or_else(|| panic!("{text:?} was not drawn")).1.center();
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(pos)]);
            self.frame(vec![button(true)]);
            self.frame(vec![button(false)]);
            self.frame(Vec::new())
        }

        fn panel(&self) -> Option<egui::Rect> {
            PanelState::load(&self.ctx, panel_id(TasksSidebar::Left)).map(|s| s.rect)
        }

        fn height(&self) -> Option<f32> {
            self.panel().map(|r| r.height())
        }

        fn content(&self) -> f32 {
            let id = panel_id(TasksSidebar::Left).with("content");
            self.ctx.data(|d| d.get_temp::<f32>(id)).expect("drawn")
        }

        fn double_click_divider(&mut self) {
            let pos = self.panel().expect("drawn").center_top();
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(pos)]);
            for pressed in [true, false, true, false] {
                self.frame(vec![button(pressed)]);
            }
            self.frame(Vec::new());
            self.frame(Vec::new());
        }
    }

    fn task(id: &str) -> alacritree_tasks::Task {
        alacritree_tasks::fake::task(id, alacritree_tasks::scope::GLOBAL)
    }

    #[test]
    fn the_filter_hides_completed_tasks_and_shows_them_again() {
        let done = alacritree_tasks::Task { status: Status::Completed, ..task("write it") };
        let mut h = Harness::of(vec![done, task("test it")]);
        let has = |texts: &[String], t: &str| texts.iter().any(|x| x == t);

        let hidden = h.click("hide completed");
        assert!(!has(&hidden, "write it"), "{hidden:?}");
        assert!(has(&hidden, "test it"), "{hidden:?}");
        assert!(has(&hidden, "1/2"), "the count dropped the hidden task: {hidden:?}");
        let changes = h.view.take_pref_changes();
        assert_eq!(changes, vec![tasks_view::PrefChange::HideCompleted(true)]);

        let shown = h.click("show completed");
        assert!(has(&shown, "write it"), "{shown:?}");
    }

    #[test]
    fn a_double_click_on_the_divider_fits_a_short_list() {
        let mut h = Harness::listing(2);
        let before = h.height().unwrap();
        h.double_click_divider();
        let after = h.height().unwrap();
        assert!(after < before, "{after} did not shrink from {before}");
        assert!((after - h.content()).abs() < 1.0, "{after} does not fit {}", h.content());
    }

    #[test]
    fn a_double_click_on_the_divider_stops_a_long_list_at_half_the_sidebar() {
        let mut h = Harness::listing(60);
        h.double_click_divider();
        assert!(h.content() > SIDEBAR * FIT_SHARE);
        assert!((h.height().unwrap() - SIDEBAR * FIT_SHARE).abs() < 1.0);
    }
}
