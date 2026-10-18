//! The tasks tab: the backend's lists for one workspace, drawn as checklist
//! rows. Each change is a batch of edits on the pool, typed text shows at
//! once, and a reload replaces it with what the store holds. Agents write
//! the same store, so the tab re-lists while visible instead of trusting its
//! own copy. The last listing is kept on disk, so the tab opens on it and the
//! first listing only corrects it.

pub(crate) mod preview;

use alacritree_vcs::Checkout;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{
    Align, Color32, DragAndDrop, Frame, Id, Key, Layout, Margin, Modal, Modifiers, Response,
    RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui, Vec2, WidgetText, vec2,
};

use alacritree_common::jobs::{self, Job, Priority};
use alacritree_common::side::Side;
use alacritree_common::wsl;
use alacritree_tasks::scope::{GLOBAL, Place, node};
use alacritree_tasks::tree::{self, Landing, Row, Section, SectionKind};
use alacritree_tasks::{Edit, Filter, NodeMatch, Status, Task, TaskBackend, TaskError};

use crate::bindings::{NamedAction, action};
use crate::projects::Project;
use crate::shortcut::Shortcuts;
use crate::state::PersistedState;
use crate::tasks::backend::{self, Backend};
use crate::tasks::facts;
use crate::vcs::Vcs;

pub(crate) const RELOAD_EVERY: Duration = Duration::from_secs(1);
const INDENT: f32 = 16.0;
const CHEVRON: f32 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scope {
    pub side: Side,
    pub repo: Option<String>,
    pub workspace: Option<String>,
}

impl Scope {
    /// The names git would give, worked out from what discovery recorded, so
    /// the tab opens without waiting on git. `adopt` replaces them once git
    /// has answered.
    pub(crate) fn for_workspace(project: Option<&Project>, worktree: Option<&Checkout>) -> Self {
        let side = match project.map(|p| wsl::classify(&p.root)) {
            Some(wsl::Location::Wsl { distro, .. }) => Side::Wsl(distro),
            _ => Side::Native,
        };
        let mut scope = Self { side, repo: None, workspace: None };
        if let Some(project) = project {
            scope.adopt(&facts::place_of(project, worktree));
        }
        scope
    }

    /// Takes the names `task scope` gives the worktree, which follow git's
    /// own view of a detached or shared checkout where the sidebar's labels
    /// do not.
    pub(crate) fn adopt(&mut self, place: &Place) {
        match place {
            Place::Global => {},
            Place::Project { .. } => {
                self.repo = Some(node(place, None));
                self.workspace = None;
            },
            Place::Workspace { repo, .. } => {
                self.repo = Some(node(&Place::Project { repo: repo.clone() }, None));
                self.workspace = Some(node(place, None));
            },
        }
    }

    /// The global list, the repository's own node, and this workspace's
    /// subtree. A workspace git has not named yet keeps the repository
    /// subtree, so the tab still has something to list.
    pub(crate) fn filter(&self) -> Filter {
        let mut nodes = vec![NodeMatch::Exact(GLOBAL.to_string())];
        match (&self.repo, &self.workspace) {
            (Some(repo), Some(workspace)) => {
                nodes.push(NodeMatch::Exact(repo.clone()));
                nodes.push(NodeMatch::Subtree(workspace.clone()));
            },
            (Some(repo), None) => nodes.push(NodeMatch::Subtree(repo.clone())),
            (None, _) => {},
        }
        Filter { nodes }
    }

    /// `node` without the workspace or repository it sits under, so a
    /// session reads as its own name in a narrow panel.
    fn short_name<'a>(&self, node: &'a str) -> &'a str {
        [&self.workspace, &self.repo]
            .into_iter()
            .flatten()
            .find_map(|parent| node.strip_prefix(parent.as_str())?.strip_prefix('.'))
            .unwrap_or(node)
    }

    /// The listing's file under the cache directory. The file belongs to the
    /// workspace, so two worktrees of one repository do not overwrite each
    /// other. Two scopes that differ only in characters a file name cannot
    /// hold share one file, which the first listing corrects.
    fn cache_file(&self, dir: &Path) -> PathBuf {
        let side = match &self.side {
            Side::Native => "native".to_string(),
            Side::Wsl(distro) => format!("wsl-{distro}"),
        };
        let place = self.workspace.as_deref().or(self.repo.as_deref()).unwrap_or(GLOBAL);
        let name: String = format!("{side}-{place}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
            .collect();
        dir.join(format!("{name}.json"))
    }
}

/// What a task action does to the row it runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowAction {
    Indent,
    Dedent,
    MoveUp,
    MoveDown,
    Delete,
}

impl RowAction {
    const MENU: [Self; 5] =
        [Self::Indent, Self::Dedent, Self::MoveUp, Self::MoveDown, Self::Delete];

    fn label(self) -> &'static str {
        match self {
            Self::Indent => "Indent",
            Self::Dedent => "Dedent",
            Self::MoveUp => "Move up",
            Self::MoveDown => "Move down",
            Self::Delete => "Delete",
        }
    }

    /// The action a binding names, whose keys the menu shows.
    fn named(self) -> NamedAction {
        match self {
            Self::Indent => NamedAction::IndentTask(action::IndentTask),
            Self::Dedent => NamedAction::DedentTask(action::DedentTask),
            Self::MoveUp => NamedAction::MoveTaskUp(action::MoveTaskUp),
            Self::MoveDown => NamedAction::MoveTaskDown(action::MoveTaskDown),
            Self::Delete => NamedAction::DeleteTask(action::DeleteTask),
        }
    }
}

/// What the tab keeps in `state.toml`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Prefs {
    /// Sections drawn folded, by node.
    pub collapsed: HashSet<String>,
    pub hide_completed: bool,
}

impl Prefs {
    pub(crate) fn from_state(state: &PersistedState) -> Self {
        Self {
            collapsed: state.collapsed_task_sections.iter().cloned().collect(),
            hide_completed: state.hide_completed_tasks,
        }
    }
}

/// One change to [`Prefs`], for the app to persist and hand the other tabs.
/// A change rather than the whole set, since other windows write the same
/// file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrefChange {
    Collapsed { node: String, collapsed: bool },
    HideCompleted(bool),
}

impl PrefChange {
    pub(crate) fn persist(&self, state: &mut PersistedState) {
        match self {
            Self::Collapsed { node, collapsed } => {
                state.collapsed_task_sections.retain(|n| n != node);
                if *collapsed {
                    state.collapsed_task_sections.push(node.clone());
                }
            },
            Self::HideCompleted(hide) => state.hide_completed_tasks = *hide,
        }
    }
}

const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(60);

/// Asks version control for a worktree's names until it answers. The tab
/// gets a worktree only when its project has a backend, so an answer of
/// `Global` means the backend could not be asked, as when `wsl.exe` fails,
/// and not that the worktree is in no repository.
struct Resolver {
    dir: PathBuf,
    backends: Vec<Vcs>,
    /// `None` between a failed lookup and its retry.
    lookup: Option<Job<Place>>,
    retry_at: Instant,
    backoff: Duration,
}

impl Resolver {
    fn new(dir: PathBuf, backends: Vec<Vcs>) -> Self {
        let mut resolver =
            Self { dir, backends, lookup: None, retry_at: Instant::now(), backoff: FIRST_RETRY };
        resolver.lookup = Some(resolver.spawn());
        resolver
    }

    fn spawn(&self) -> Job<Place> {
        let (dir, backends) = (self.dir.clone(), self.backends.clone());
        jobs::pool().spawn(Priority::Interactive, move |b| facts::place_for(&dir, &backends, b).1)
    }

    fn poll(&mut self, now: Instant) -> Option<Place> {
        let Some(job) = &self.lookup else {
            if now >= self.retry_at {
                self.lookup = Some(self.spawn());
            }
            return None;
        };
        match job.poll() {
            Some(Place::Global) => {},
            Some(place) => {
                self.lookup = None;
                return Some(place);
            },
            None if job.failed() => {},
            None => return None,
        }
        log::warn!(
            "tasks: no repository found for {}, asking again in {:?}",
            self.dir.display(),
            self.backoff
        );
        self.lookup = None;
        self.retry_at = now + self.backoff;
        self.backoff = (self.backoff * 2).min(LAST_RETRY);
        None
    }
}

/// Edits the tab asks for, with the row a failure is shown on. A frame
/// queues them and the next one spawns them, so what a frame decided can be
/// read before the pool runs it.
type QueuedWrite = (Option<String>, Vec<Edit>);

/// One section and the tasks it was built from. An added row has no id yet.
/// Drawing the section reads these tasks instead of copying the listing.
#[derive(Clone)]
pub(crate) struct Built {
    pub(crate) section: Section,
    tasks: Vec<Task>,
}

/// A write in flight, with the row its failure is shown on.
type PendingWrite = (Option<String>, Job<Result<(), TaskError>>);

/// A listing in flight: the write count and the scope it started under, and
/// the job. A result whose scope is no longer current is discarded, because
/// the cache write uses the scope the tab holds when the listing lands.
struct PendingReload {
    epoch: u64,
    scope: Scope,
    job: Job<Result<Vec<Task>, TaskError>>,
}

/// A row being typed that the store has not got yet.
#[derive(Clone)]
struct NewRow {
    node: String,
    after: Option<String>,
    depth: usize,
    text: String,
    focus: bool,
}

/// A delete that would take subtasks with it, waiting for a yes.
struct ConfirmDelete {
    ids: Vec<String>,
    prompt: String,
}

/// Where a drawn row's text starts and ends, for the arrow that steps into it.
struct RowSpot {
    id: String,
    first_line_chars: usize,
    last_line_start: usize,
    last_line_chars: usize,
}

/// Where the cursor sat in the focused row last frame. Up on the first line
/// or Down on the last one steps to the next row instead of moving within it.
struct Caret {
    id: String,
    first_line: bool,
    last_line: bool,
    column: usize,
}

/// The payload a row's grip carries while it is dragged.
struct DraggedTask {
    id: String,
    node: String,
}

pub(crate) struct TasksView {
    backend: Backend,
    scope: Scope,
    /// The worktree's names as `task scope` reads them from git.
    resolving: Option<Resolver>,
    tasks: Vec<Task>,
    /// The shown tasks split into sections, built when a listing lands or a
    /// pending edit changes what the rows show. A redraw reads it.
    built: OnceCell<Vec<Built>>,
    /// Builds since the view was created. Tests read it to tell a reuse from
    /// a rebuild that happens to return the same sections.
    #[cfg(test)]
    builds: std::cell::Cell<u32>,
    /// Whether a listing has landed, so an empty tab can say it is waiting.
    loaded: bool,
    cache_dir: Option<PathBuf>,
    cache_write: Option<Job<()>>,
    load_error: Option<String>,
    /// A failed add has no row to show on; it stays until the next write.
    write_error: Option<String>,
    reload: Option<PendingReload>,
    listed: Option<String>,
    /// A stamp read in flight. The frame only polls it.
    stamp_job: Option<Job<(Option<String>, Option<PathBuf>)>>,
    /// The replica directory last located for this side. A later tick stats it.
    replica_dir: Option<(Side, PathBuf)>,
    last_reload: Option<Instant>,
    /// Set when a write finishes, so the export after it is not skipped
    /// while an older one is still running.
    stale: bool,
    writes_finished: u64,
    outbox: Vec<QueuedWrite>,
    writes: Vec<PendingWrite>,
    row_errors: HashMap<String, String>,
    /// Typed text a reload has not confirmed yet, by id.
    drafts: HashMap<String, String>,
    /// What the rows show until a listing started after every write has
    /// landed: toggled statuses, deleted rows, moved rows and added rows.
    statuses: HashMap<String, Status>,
    deleted: HashSet<String>,
    /// A delete landed while a frame held section indexes. The sections
    /// stay as drawn until the next frame starts.
    sections_stale: bool,
    /// Set while this view's draw loop holds section indexes.
    sections_held: std::rc::Rc<std::cell::Cell<bool>>,
    moves: Vec<Edit>,
    added: Vec<NewRow>,
    new_row: Option<NewRow>,
    /// Tasks whose sub-tasks are hidden, by id.
    collapsed_tasks: HashSet<String>,
    /// The row a task action runs on: the one being edited, or the last one
    /// that was, so the palette can still reach it.
    current: Option<String>,
    /// Rows picked with Ctrl or Shift held, which a delete takes together.
    selected: HashSet<String>,
    /// The row a Shift+click selects from.
    anchor: Option<String>,
    confirm: Option<ConfirmDelete>,
    /// The rows drawn last frame, top to bottom across every section.
    spots: Vec<RowSpot>,
    drawing: Vec<RowSpot>,
    caret: Option<Caret>,
    prefs: Prefs,
    pref_changes: Vec<PrefChange>,
}

impl TasksView {
    /// Shows `scope` at once, from the last listing under `cache_dir` when
    /// there is one, and switches to the names git gives `worktree` once
    /// they are read.
    pub(crate) fn new(
        backend: Backend,
        scope: Scope,
        worktree: Option<PathBuf>,
        backends: Vec<Vcs>,
        prefs: Prefs,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        let resolving = worktree.map(|dir| Resolver::new(dir, backends));
        let tasks = cache_dir.as_deref().and_then(|dir| read_cache(&scope.cache_file(dir)));
        Self {
            backend,
            scope,
            resolving,
            tasks: tasks.unwrap_or_default(),
            built: OnceCell::new(),
            #[cfg(test)]
            builds: std::cell::Cell::new(0),
            loaded: false,
            cache_dir,
            cache_write: None,
            load_error: None,
            write_error: None,
            reload: None,
            listed: None,
            stamp_job: None,
            replica_dir: None,
            last_reload: None,
            stale: false,
            writes_finished: 0,
            outbox: Vec::new(),
            writes: Vec::new(),
            row_errors: HashMap::new(),
            drafts: HashMap::new(),
            statuses: HashMap::new(),
            deleted: HashSet::new(),
            sections_stale: false,
            sections_held: std::rc::Rc::new(std::cell::Cell::new(false)),
            moves: Vec::new(),
            added: Vec::new(),
            new_row: None,
            collapsed_tasks: HashSet::new(),
            current: None,
            selected: HashSet::new(),
            anchor: None,
            confirm: None,
            spots: Vec::new(),
            drawing: Vec::new(),
            caret: None,
            prefs,
            pref_changes: Vec::new(),
        }
    }

    /// Drops the built sections, so the next read builds them from the
    /// listing and the pending edits as they stand.
    fn rebuild(&mut self) {
        self.built.take();
    }

    /// Shows `id` as `status` until a listing started after the write lands.
    fn show_status(&mut self, id: String, status: Status) {
        self.statuses.insert(id, status);
        self.rebuild();
    }

    fn show_deleted(&mut self, ids: impl IntoIterator<Item = String>) {
        self.deleted.extend(ids);
        self.mark_sections_stale();
    }

    /// A delete during a frame marks the sections stale. The rebuild waits
    /// for the next frame, which no longer holds the old indexes.
    fn mark_sections_stale(&mut self) {
        if self.sections_held.get() {
            self.sections_stale = true;
        } else {
            self.rebuild();
        }
    }

    /// Holds this view's section indexes until the guard drops, including
    /// when a row panics. The guard does not borrow the view.
    pub(crate) fn hold_sections(&self) -> SectionHold {
        self.sections_held.set(true);
        SectionHold(std::rc::Rc::clone(&self.sections_held))
    }

    /// Rebuilds sections a delete marked stale, before this frame reads
    /// their indexes.
    pub(crate) fn refresh_sections(&mut self) {
        if !self.sections_stale {
            return;
        }
        self.sections_stale = false;
        self.rebuild();
    }

    fn show_moves(&mut self, edits: impl IntoIterator<Item = Edit>) {
        self.moves.extend(edits);
        self.rebuild();
    }

    fn show_added(&mut self, rows: impl IntoIterator<Item = NewRow>) {
        self.added.extend(rows);
        self.rebuild();
    }

    /// The store's tasks with the unconfirmed toggles, deletes and moves
    /// laid over them.
    fn shown_tasks(&self) -> Vec<Task> {
        let mut tasks: Vec<Task> =
            self.tasks.iter().filter(|t| !self.deleted.contains(&t.id)).cloned().collect();
        for task in &mut tasks {
            if let Some(status) = self.statuses.get(&task.id) {
                task.status = *status;
            }
        }
        for edit in &self.moves {
            tree::replay(&mut tasks, edit);
        }
        tasks
    }

    /// The shown tasks as sections, built once per listing and pending edit.
    pub(crate) fn sections(&self) -> &[Built] {
        self.built.get_or_init(|| {
            #[cfg(test)]
            {
                self.builds.set(self.builds.get() + 1);
            }
            self.build_sections()
        })
    }

    /// How many times the sections have been built. A redraw that reuses them
    /// leaves this unchanged.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> u32 {
        self.builds.get()
    }

    fn build_sections(&self) -> Vec<Built> {
        let tasks = self.shown_tasks();
        let split =
            tree::sections_with(tasks, self.scope.repo.as_deref(), self.scope.workspace.as_deref());
        let mut built = Vec::new();
        for (mut section, here) in split {
            for add in self.added.iter().filter(|a| a.node == section.node) {
                let rows = &section.rows;
                let anchor =
                    add.after.as_ref().and_then(|id| rows.iter().position(|r| &r.id == id));
                let mut at = anchor.map_or(rows.len(), |i| i + 1 + tree::descendants(rows, i));
                // Rows added earlier after the same anchor stay above this one.
                while rows.get(at).is_some_and(|r| r.id.is_empty()) {
                    at += 1;
                }
                section.rows.insert(at, Row {
                    id: String::new(),
                    depth: add.depth,
                    text: add.text.clone(),
                    status: Status::Pending,
                    started: false,
                    subtasks: tree::Progress::default(),
                });
            }
            built.push(Built { section, tasks: here });
        }
        built
    }

    pub(crate) fn section(&self, index: usize) -> &Section {
        &self.sections()[index].section
    }

    /// The indexes of [`Self::sections`] the sidebar draws: those holding
    /// tasks, without the global list once the workspace is in a repository,
    /// since that list says nothing about the checkout.
    pub(crate) fn sidebar_indexes(&self) -> impl Iterator<Item = usize> + '_ {
        let in_repo = self.scope.repo.is_some();
        self.sections().iter().enumerate().filter_map(move |(i, built)| {
            let section = &built.section;
            (!section.rows.is_empty() && !(in_repo && section.kind == SectionKind::Global))
                .then_some(i)
        })
    }

    pub(crate) fn short_name<'a>(&self, node: &'a str) -> &'a str {
        self.scope.short_name(node)
    }

    pub(crate) fn is_collapsed(&self, node: &str) -> bool {
        self.prefs.collapsed.contains(node)
    }

    pub(crate) fn hides_completed(&self) -> bool {
        self.prefs.hide_completed
    }

    pub(crate) fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub(crate) fn plain_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for built in self.sections() {
            lines.push(format!("## {}", built.section.node));
            for row in &built.section.rows {
                let mark = if row.status == Status::Completed { "x" } else { " " };
                lines.push(format!("{}- [{mark}] {}", "  ".repeat(row.depth), row.text));
            }
        }
        lines
    }

    /// Runs `action` on the current row, or deletes the selected rows when
    /// there are any. Waits out an open delete prompt, so a key pressed under
    /// it does not act on the row behind it.
    pub(crate) fn act(&mut self, action: RowAction) {
        if self.confirm.is_some() {
            return;
        }
        if action == RowAction::Delete && !self.selected.is_empty() {
            self.request_delete(self.selection());
        } else if let Some(id) = self.current.clone() {
            self.act_on(&id, action);
        }
    }

    fn act_on(&mut self, id: &str, action: RowAction) {
        let shown = self.shown_tasks();
        let Some(task) = shown.iter().find(|t| t.id == id) else { return };
        let refs: Vec<&Task> = shown.iter().filter(|t| t.node() == task.node()).collect();
        let edits = match action {
            RowAction::Indent => tree::indent(&refs, id),
            RowAction::Dedent => tree::dedent(&refs, id),
            RowAction::MoveUp => tree::move_up(&refs, id),
            RowAction::MoveDown => tree::move_down(&refs, id),
            RowAction::Delete => return self.request_delete(vec![id.to_string()]),
        };
        // A row moved under a collapsed task would vanish mid-edit.
        for edit in &edits {
            if let Edit::Move { parent: Some(parent), .. } = edit {
                self.collapsed_tasks.remove(parent);
            }
        }
        self.rearrange(id, edits);
    }

    /// The selected rows still shown, in store order.
    fn selection(&self) -> Vec<String> {
        let shown = self.shown_tasks().into_iter();
        shown.filter(|t| self.selected.contains(&t.id)).map(|t| t.id).collect()
    }

    /// Deletes `ids` at once, or asks first when they would take subtasks
    /// that are not among them.
    fn request_delete(&mut self, ids: Vec<String>) {
        let shown = self.shown_tasks();
        let refs: Vec<&Task> = shown.iter().collect();
        let ids: Vec<String> =
            ids.into_iter().filter(|id| refs.iter().any(|t| &t.id == id)).collect();
        let subtasks = doomed(&refs, &ids).len() - ids.len();
        if subtasks == 0 {
            return self.delete(&ids);
        }
        let noun = if subtasks == 1 { "subtask" } else { "subtasks" };
        let prompt = match ids.as_slice() {
            [id] => {
                let text = refs.iter().find(|t| &t.id == id).map_or("", |t| &t.description);
                format!("Delete \"{text}\" and its {subtasks} {noun}?")
            },
            _ => format!("Delete {} tasks and their {subtasks} {noun}?", ids.len()),
        };
        self.confirm = Some(ConfirmDelete { ids, prompt });
    }

    /// Deletes `ids` and everything nested below them in one write.
    fn delete(&mut self, ids: &[String]) {
        let Some(first) = ids.first() else { return };
        let shown = self.shown_tasks();
        let refs: Vec<&Task> = shown.iter().collect();
        let gone = doomed(&refs, ids);
        for id in &gone {
            self.drafts.remove(id);
            self.selected.remove(id);
        }
        self.show_deleted(gone.iter().cloned());
        if self.current.as_ref().is_some_and(|c| gone.contains(c)) {
            self.current = None;
        }
        self.write(Some(first.clone()), gone.into_iter().map(Edit::Delete).collect());
    }

    /// Ctrl+click adds `id` to the selection or takes it out. Shift+click
    /// selects every drawn row between it and the anchor, or the row being
    /// edited when nothing was picked yet.
    fn pick(&mut self, id: &str, range: bool) {
        let at = |id: &str| self.spots.iter().position(|s| s.id == id);
        let from = self.anchor.as_deref().or(self.current.as_deref()).and_then(at);
        match (range, from, at(id)) {
            (true, Some(from), Some(to)) => {
                let span = &self.spots[from.min(to)..=from.max(to)];
                self.selected = span.iter().map(|s| s.id.clone()).collect();
                self.anchor = Some(self.spots[from].id.clone());
            },
            _ => {
                if !self.selected.remove(id) {
                    self.selected.insert(id.to_string());
                }
                self.anchor = Some(id.to_string());
            },
        }
        self.current = Some(id.to_string());
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// Writes edits that place `id`, and shows them before the store has.
    fn rearrange(&mut self, id: &str, edits: Vec<Edit>) {
        let placing =
            edits.iter().filter(|e| matches!(e, Edit::Move { .. } | Edit::Reorder { .. }));
        self.show_moves(placing.cloned());
        self.write(Some(id.to_string()), edits);
    }

    pub(crate) fn toggle_collapsed(&mut self, node: &str) {
        let collapsed = !self.prefs.collapsed.contains(node);
        let change = PrefChange::Collapsed { node: node.to_string(), collapsed };
        self.apply_pref(&change);
        self.pref_changes.push(change);
    }

    pub(crate) fn toggle_completed(&mut self) {
        let change = PrefChange::HideCompleted(!self.prefs.hide_completed);
        self.apply_pref(&change);
        self.pref_changes.push(change);
    }

    /// Takes a change another tab made, or this one.
    pub(crate) fn apply_pref(&mut self, change: &PrefChange) {
        match change {
            PrefChange::Collapsed { node, collapsed: true } => {
                self.prefs.collapsed.insert(node.clone());
            },
            PrefChange::Collapsed { node, collapsed: false } => {
                self.prefs.collapsed.remove(node);
            },
            PrefChange::HideCompleted(hide) => self.prefs.hide_completed = *hide,
        }
    }

    /// The changes made since the last call, for the app to persist.
    pub(crate) fn take_pref_changes(&mut self) -> Vec<PrefChange> {
        std::mem::take(&mut self.pref_changes)
    }

    fn write(&mut self, row: Option<String>, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        match &row {
            Some(id) => self.row_errors.remove(id),
            None => self.write_error.take(),
        };
        self.outbox.push((row, edits));
    }

    fn write_one(&mut self, row: &str, edit: Edit) {
        self.write(Some(row.to_string()), vec![edit]);
    }

    fn finish_write(&mut self, row: Option<String>, result: Result<(), TaskError>) {
        self.writes_finished += 1;
        self.stale = true;
        let Err(e) = result else { return };
        match row {
            Some(id) => {
                self.drafts.remove(&id);
                self.row_errors.insert(id, e.to_string());
            },
            None => self.write_error = Some(e.to_string()),
        }
    }

    /// What a listing started now is tagged with.
    fn reload_epoch(&self) -> u64 {
        self.writes_finished
    }

    /// A failed or panicked listing did not export, so the next due tick lists again.
    fn invalidate_listing(&mut self) {
        self.listed = None;
        self.stale = true;
    }

    fn finish_reload(&mut self, epoch: u64, scope: &Scope, result: Result<Vec<Task>, TaskError>) {
        if scope != &self.scope {
            return;
        }
        match result {
            Ok(tasks) => {
                self.load_error = None;
                self.loaded = true;
                // A draft stays until the store holds its text or the task
                // is gone.
                self.drafts
                    .retain(|id, text| tasks.iter().any(|t| &t.id == id && &t.description != text));
                let settled = epoch == self.writes_finished
                    && self.writes.is_empty()
                    && self.outbox.is_empty();
                if settled {
                    self.statuses.clear();
                    self.deleted.clear();
                    self.moves.clear();
                    self.added.clear();
                }
                // A settled listing drops the edits the sections were built
                // with. An unsettled one keeps them, and the listing under
                // them changed either way.
                self.rebuild();
                if tasks != self.tasks {
                    self.save_cache(&tasks);
                }
                self.tasks = tasks;
            },
            Err(e) => {
                self.load_error = Some(e.to_string());
                self.invalidate_listing();
            },
        }
    }

    fn save_cache(&mut self, tasks: &[Task]) {
        let Some(dir) = &self.cache_dir else { return };
        let (path, tasks) = (self.scope.cache_file(dir), tasks.to_vec());
        let job = jobs::pool().spawn(Priority::Background, move |_| write_cache(&path, &tasks));
        self.cache_write = Some(job);
    }

    /// Spawns queued writes, drains finished jobs, and reloads after any
    /// write or once a second.
    pub(crate) fn tick(&mut self) {
        if let Some(place) = self.resolving.as_mut().and_then(|r| r.poll(Instant::now())) {
            self.resolving = None;
            self.scope.adopt(&place);
            self.rebuild();
            self.stale = true;
        }
        for (row, edits) in std::mem::take(&mut self.outbox) {
            let (store, side) = (self.backend.clone(), self.scope.side.clone());
            let job = jobs::pool().spawn(Priority::Interactive, move |b| {
                backend::apply_all(&store, &side, &edits, b)
            });
            self.writes.push((row, job));
        }
        let due = self.last_reload.is_none_or(|t| t.elapsed() >= RELOAD_EVERY);
        if let Some(pending) = self.reload.take() {
            match pending.job.poll() {
                Some(result) => self.finish_reload(pending.epoch, &pending.scope, result),
                None if pending.job.failed() => self.invalidate_listing(),
                None => self.reload = Some(pending),
            }
        }
        let mut finished = Vec::new();
        self.writes.retain(|(row, job)| match job.poll() {
            Some(result) => {
                finished.push((row.clone(), result));
                false
            },
            None => !job.failed(),
        });
        for (row, result) in finished {
            self.finish_write(row, result);
        }
        if self.reload.is_none() && (self.stale || due) {
            self.reload_if_changed();
        }
    }

    /// Lists when the replica changed, when our own write finished, or when
    /// the stamp cannot be read. An unchanged stamp runs no export. The stamp
    /// read, including a locate after a missing file, runs on a pool job.
    fn reload_if_changed(&mut self) {
        if self.stamp_job.is_none() {
            let (store, side) = (self.backend.clone(), self.scope.side.clone());
            let known = self
                .replica_dir
                .as_ref()
                .filter(|(cached, _)| cached == &side)
                .map(|(_, dir)| dir.clone());
            self.stamp_job = Some(jobs::pool().spawn(Priority::Background, move |b| {
                backend::replica_stamp(&store, &side, known.as_deref(), b)
            }));
        }
        let Some(job) = self.stamp_job.as_ref() else { return };
        let read = job.poll();
        if read.is_none() && !job.failed() {
            return;
        }
        self.stamp_job = None;
        let Some((stamp, dir)) = read else { return };
        let side = self.scope.side.clone();
        self.replica_dir = dir.map(|dir| (side.clone(), dir));
        let changed = self.stale || stamp.is_none() || stamp != self.listed;
        self.last_reload = Some(Instant::now());
        if !changed {
            return;
        }
        self.stale = false;
        self.listed = stamp;
        let (store, scope) = (self.backend.clone(), self.scope.clone());
        let listed = scope.clone();
        let job = jobs::pool().spawn(Priority::Background, move |b| {
            store.list(&listed.side, &listed.filter(), b)
        });
        self.reload = Some(PendingReload { epoch: self.reload_epoch(), scope, job });
    }
}

/// Clears one view's section hold when its draw loop ends, including on panic.
pub(crate) struct SectionHold(std::rc::Rc<std::cell::Cell<bool>>);

impl Drop for SectionHold {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

#[cfg(test)]
thread_local! {
    static DRAWN_ROW: std::cell::RefCell<Option<std::sync::Arc<dyn Fn(&TasksView, usize, &Row) + Send + Sync>>> =
        const { std::cell::RefCell::new(None) };
    static STALE_INDEX: std::cell::RefCell<Option<(usize, String)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn stale_section_index() -> Option<(usize, String)> {
    STALE_INDEX.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
fn check_drawn_row(view: &TasksView, index: usize, rows: &[Row]) {
    let Some(check) = DRAWN_ROW.with(|slot| slot.borrow().clone()) else { return };
    for row in rows {
        check(view, index, row);
    }
}

#[cfg(test)]
fn set_drawn_row_check(check: impl Fn(&TasksView, usize, &Row) + Send + Sync + 'static) {
    DRAWN_ROW.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::new(check)));
}

#[cfg(test)]
fn clear_drawn_row_check() {
    DRAWN_ROW.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
impl TasksView {
    /// A tab showing `tasks` with `current` as the row actions run on, and no
    /// backend or cache behind it.
    pub(crate) fn for_test(tasks: Vec<Task>, current: &str) -> Self {
        let backend = Backend::from_config(&Default::default());
        let scope = Scope::for_workspace(None, None);
        let mut view = Self::new(backend, scope, None, Vec::new(), Prefs::default(), None);
        view.tasks = tasks;
        view.current = Some(current.to_string());
        view
    }
}

/// `ids` and every task nested below them, each once, a task's subtasks
/// before it so a failure part way never leaves them orphaned at the top
/// level.
fn doomed(tasks: &[&Task], ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        for gone in tree::descendant_ids(tasks, id).into_iter().chain([id.clone()]) {
            if seen.insert(gone.clone()) {
                out.push(gone);
            }
        }
    }
    out
}

fn read_cache(path: &Path) -> Option<Vec<Task>> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Through a file beside it and a rename, so another window opening the tab
/// never reads half a listing.
fn write_cache(path: &Path, tasks: &[Task]) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let partial = path.with_extension(format!("json.{}", std::process::id()));
        std::fs::write(&partial, serde_json::to_vec(tasks)?)?;
        std::fs::rename(&partial, path)
    })();
    if let Err(e) = result {
        log::debug!("cannot cache the task listing at {}: {e}", path.display());
    }
}

pub(crate) fn show(
    ui: &mut Ui,
    view: &mut TasksView,
    allow_focus: bool,
    shortcuts: &Shortcuts,
    style: Style,
) -> Response {
    view.tick();
    let response = draw(ui, view, allow_focus, shortcuts, style);
    if view.outbox.is_empty() {
        ui.ctx().request_repaint_after(RELOAD_EVERY);
    } else {
        ui.ctx().request_repaint();
    }
    response
}

fn draw(
    ui: &mut Ui,
    view: &mut TasksView,
    allow_focus: bool,
    shortcuts: &Shortcuts,
    style: Style,
) -> Response {
    view.refresh_sections();
    // Registered before the rows: egui gives a click to the last widget
    // registered under the pointer, so every row widget wins over this.
    let background = ui.interact(ui.max_rect(), ui.id().with("tasks-tab"), Sense::click());
    if background.clicked() {
        view.current = None;
        view.clear_selection();
    }
    let paint = Paint { allow_focus, shortcuts, c: style };
    Frame::default().inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
        use_terminal_font(ui.style_mut(), style.font);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for e in view.load_error.iter().chain(&view.write_error) {
                ui.label(RichText::new(e).color(style.error));
            }
            let label = if view.prefs.hide_completed { "show completed" } else { "hide completed" };
            if button(ui, label, style.add_button).clicked() {
                view.toggle_completed();
            }
            if !view.loaded && view.tasks.is_empty() && view.load_error.is_none() {
                ui.label(RichText::new("loading tasks").color(style.dim));
            }
            // Taken once: a drop rebuilds the sections, and the rows being
            // drawn have to outlive that.
            let drawn = view.sections().to_vec();
            let _hold = view.hold_sections();
            for (i, built) in drawn.iter().enumerate() {
                show_section(ui, view, i, built, &paint);
                #[cfg(test)]
                check_drawn_row(view, i, &built.section.rows);
            }
        });
    });
    view.spots = std::mem::take(&mut view.drawing);
    show_confirm(ui.ctx(), view);
    background
}

/// Every text style in the pane switches to the terminal font. Body and
/// Heading take `body` pixels, the rest keep the UI's smaller ratio to it.
fn use_terminal_font(style: &mut egui::Style, body: f32) {
    use crate::config::FontConfig;
    use egui::{FontId, TextStyle};
    let small = body * FontConfig::UI_NORMAL_RATIO / FontConfig::UI_HEADING_RATIO;
    for (text_style, size) in [
        (TextStyle::Heading, body),
        (TextStyle::Body, body),
        (TextStyle::Small, small),
        (TextStyle::Button, small),
        (TextStyle::Monospace, small),
    ] {
        style.text_styles.insert(text_style, FontId::monospace(size));
    }
}

/// `[ui.tasks]` with every color resolved against the palette.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Style {
    pub text: Color32,
    pub dim: Color32,
    pub error: Color32,
    pub chevron: Stroke,
    pub chevron_hover: Color32,
    pub section_chevron: Stroke,
    pub hidden_count: Color32,
    pub active_marker: Stroke,
    pub active_background: Color32,
    pub add_button: ButtonStyle,
    /// Behind a selected row.
    pub selection: Color32,
    pub selected_text: Color32,
    /// Logical-pixel size for task text, in the terminal font. Smaller text
    /// such as buttons keeps the UI's ratio to it.
    pub font: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ButtonStyle {
    pub text: Color32,
    pub hover_text: Color32,
    pub fill: Color32,
    pub hover_fill: Color32,
    pub pressed_fill: Color32,
}

struct Paint<'a> {
    allow_focus: bool,
    shortcuts: &'a Shortcuts,
    c: Style,
}

fn show_section(ui: &mut Ui, view: &mut TasksView, index: usize, built: &Built, paint: &Paint<'_>) {
    let c = paint.c;
    let node = built.section.node.clone();
    let (done, total) = {
        let rows = &built.section.rows;
        (rows.iter().filter(|r| r.status == Status::Completed).count(), rows.len())
    };
    let open = !view.prefs.collapsed.contains(&node);
    let header = format!("{node}  {done}/{total}");
    if heading(ui, open, &header, c).clicked() {
        view.toggle_collapsed(&node);
    }
    if !open {
        return;
    }
    let open_rows;
    let rows: Vec<&Row> = match view.prefs.hide_completed {
        true => {
            open_rows = tree::open_rows(&built.section.rows);
            open_rows
        },
        false => built.section.rows.iter().collect(),
    };
    let collapsed = view.collapsed_tasks.clone();
    let drawn = tree::fold(&rows, &collapsed);
    for (row, below) in drawn {
        let id = row.id.clone();
        show_row(ui, view, index, row, &built.tasks, below, paint);
        let follows = |n: &NewRow| n.node == node && n.after.as_deref() == Some(&id);
        if view.new_row.as_ref().is_some_and(follows) {
            show_new_row(ui, view, index, c);
        }
    }
    let adding_first = |n: &NewRow| n.node == node && n.after.is_none();
    if view.new_row.as_ref().is_some_and(adding_first) {
        show_new_row(ui, view, index, c);
    } else if button(ui, "+ add a task", c.add_button).clicked() {
        view.clear_selection();
        // Drawn below the last root row from the next frame on.
        let last_root = rows.iter().rev().find(|r| r.depth == 0).map(|r| r.id.clone());
        view.new_row =
            Some(NewRow { node, after: last_root, depth: 0, text: String::new(), focus: true });
    }
}

fn show_row(
    ui: &mut Ui,
    view: &mut TasksView,
    index: usize,
    row: &Row,
    tasks: &[Task],
    below: usize,
    paint: &Paint<'_>,
) {
    let c = paint.c;
    if row.id.is_empty() {
        ui.horizontal_top(|ui| {
            ui.add_space(row.depth as f32 * INDENT);
            // Drawn so the row does not shift once the store gives it an id,
            // though nothing can be dragged before then.
            grip(ui, c, Sense::hover());
            toggle(ui, None, c.chevron, c.chevron_hover);
            ui.add_enabled(false, egui::Checkbox::without_text(&mut false));
            ui.label(RichText::new(&row.text).color(c.dim));
        });
        return;
    }
    let node = view.section(index).node.clone();
    let widget = Id::new(("task-row", &row.id));
    let mut buffer = view.drafts.get(&row.id).cloned().unwrap_or_else(|| row.text.clone());
    // Read before the text field: it would take Enter as a line break, and
    // Backspace on a row that is already empty deletes the row instead.
    let (enter, erase) = match ui.memory(|m| m.has_focus(widget)) {
        true => ui.input_mut(|i| {
            let enter = i.consume_key(Modifiers::NONE, Key::Enter);
            (enter, buffer.is_empty() && i.consume_key(Modifiers::NONE, Key::Backspace))
        }),
        false => (false, false),
    };
    step_at_edge(ui, view, &row.id);
    // Reserved before the row's widgets so the fills paint under them once
    // the row's full rect is known.
    let background = ui.painter().add(Shape::Noop);
    let highlight = ui.painter().add(Shape::Noop);
    let collapsed = view.collapsed_tasks.contains(&row.id);
    let selected = view.selected.contains(&row.id);
    let line = ui.horizontal_top(|ui| {
        ui.add_space(row.depth as f32 * INDENT);
        grip(ui, c, Sense::drag())
            .dnd_set_drag_payload(DraggedTask { id: row.id.clone(), node: node.clone() });
        if toggle(ui, (below > 0).then_some(!collapsed), c.chevron, c.chevron_hover)
            && !view.collapsed_tasks.remove(&row.id)
        {
            view.collapsed_tasks.insert(row.id.clone());
        }
        let mut checked = row.status == Status::Completed;
        if ui.checkbox(&mut checked, "").changed() {
            let id = row.id.clone();
            let status = if checked { Status::Completed } else { Status::Pending };
            view.show_status(id.clone(), status);
            view.write_one(&row.id, if checked { Edit::Done(id) } else { Edit::Undone(id) });
        }
        if row.started {
            let (rect, _) = ui.allocate_exact_size(glyph_size(ui), Sense::hover());
            paint_chevron(ui, rect.center(), false, c.active_marker);
        }
        if collapsed && below > 0 {
            let color = if selected { c.selected_text } else { c.hidden_count };
            ui.label(RichText::new(format!("+{below}")).color(color));
        }
        // Locking focus keeps Tab for indenting instead of moving to the
        // next widget.
        let field = TextEdit::multiline(&mut buffer)
            .id(widget)
            .desired_rows(1)
            .frame(false)
            .lock_focus(true)
            .text_color(match (selected, checked) {
                (true, _) => c.selected_text,
                (false, true) => c.dim,
                (false, false) => c.text,
            })
            .desired_width(f32::INFINITY);
        let show = |ui: &mut Ui| ui.add_enabled_ui(paint.allow_focus, |ui| field.show(ui)).inner;
        let subtasks = row.subtasks;
        if subtasks.total == 0 {
            return show(ui);
        }
        let counter = format!("({}/{})", subtasks.done, subtasks.total);
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.label(RichText::new(counter).color(if selected { c.selected_text } else { c.dim }));
            show(ui)
        })
        .inner
    });
    let output = line.inner;
    record_spot(view, &row.id, &output);
    let edit = output.response;
    if edit.changed() {
        view.drafts.insert(row.id.clone(), buffer.clone());
    }
    edit.context_menu(|ui| row_menu(ui, view, &row.id, paint.shortcuts));
    if edit.has_focus() {
        view.current = Some(row.id.clone());
    }
    // A right-click focuses the row too, and its menu acts on the selection.
    let right_click = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
    if edit.gained_focus() && !right_click {
        view.clear_selection();
    }
    if erase {
        view.act_on(&row.id, RowAction::Delete);
    }
    if enter {
        // The new row takes focus next frame, and this one commits its text
        // as it loses it.
        view.new_row = Some(NewRow {
            node,
            after: Some(row.id.clone()),
            depth: row.depth,
            text: String::new(),
            focus: true,
        });
    }
    if edit.lost_focus() {
        let text = one_line(&buffer);
        if text != row.text && !text.is_empty() {
            view.write_one(&row.id, Edit::Describe { id: row.id.clone(), description: text });
        } else {
            view.drafts.remove(&row.id);
        }
    }
    let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), line.response.rect.y_range());
    if row.started {
        let radius = ui.visuals().widgets.inactive.corner_radius;
        ui.painter().set(background, Shape::rect_filled(rect, radius, c.active_background));
    }
    // Registered after the row's widgets, so it takes the click from them
    // while a selecting modifier is held and leaves it to them otherwise.
    if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
        let pick = ui.interact(rect, widget.with("pick"), Sense::click_and_drag());
        if pick.clicked() {
            view.pick(&row.id, ui.input(|i| i.modifiers.shift));
            if let Some(focused) = ui.memory(|m| m.focused()) {
                ui.memory_mut(|m| m.surrender_focus(focused));
            }
        }
    }
    if view.selected.contains(&row.id) {
        ui.painter().set(highlight, Shape::rect_filled(rect, 2.0, c.selection));
    } else if view.current.as_deref() == Some(&row.id) && !edit.has_focus() {
        let fill = ui.visuals().faint_bg_color;
        ui.painter().set(highlight, Shape::rect_filled(rect, 2.0, fill));
    }
    let (id, depth) = (row.id.clone(), row.depth);
    accept_drop(ui, view, index, row, tasks, rect, c);
    // Below the row, since the text beside it takes the full width.
    if let Some(e) = view.row_errors.get(&id) {
        ui.horizontal(|ui| {
            ui.add_space((depth + 1) as f32 * INDENT);
            ui.add(egui::Label::new(RichText::new(e).color(c.error)).wrap());
        });
    }
}

/// A section heading as one click target: the chevron, the gap after it and
/// the label, all lit together under the pointer.
fn heading(ui: &mut Ui, open: bool, label: &str, c: Style) -> Response {
    let galley = WidgetText::from(RichText::new(label).color(c.text).strong()).into_galley(
        ui,
        None,
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let square = ui.spacing().interact_size.y;
    let (gap, pad) = (ui.spacing().item_spacing.x, ui.spacing().button_padding.x);
    let size = vec2(square + gap + galley.size().x + pad, square.max(galley.size().y));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    if hovered {
        let fill = ui.visuals().widgets.hovered;
        ui.painter().rect_filled(rect, fill.corner_radius, fill.weak_bg_fill);
    }
    let chevron = egui::pos2(rect.left() + square / 2.0, rect.center().y);
    let color = if hovered { c.chevron_hover } else { c.section_chevron.color };
    paint_chevron(ui, chevron, open, Stroke { color, ..c.section_chevron });
    let text = egui::pos2(rect.left() + square + gap, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text, galley, c.text);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row-tall square that folds a section or a task's sub-tasks, drawing
/// only a chevron that points down while `open`. `None` leaves the square
/// blank, so checkboxes line up at every depth.
fn toggle(ui: &mut Ui, open: Option<bool>, stroke: Stroke, hover: Color32) -> bool {
    let sense = if open.is_some() { Sense::click() } else { Sense::hover() };
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let Some(open) = open else { return false };
    let color = if response.hovered() { hover } else { stroke.color };
    paint_chevron(ui, rect.center(), open, Stroke { color, ..stroke });
    response.clicked()
}

fn glyph_size(ui: &Ui) -> Vec2 {
    Vec2::new(CHEVRON, ui.spacing().interact_size.y)
}

/// A chevron centred on `center`, pointing down when `open` and right
/// otherwise.
pub(crate) fn paint_chevron(ui: &Ui, center: egui::Pos2, open: bool, stroke: Stroke) {
    let r = CHEVRON / 4.0;
    let points = if open {
        vec![center + vec2(-r, -r / 2.0), center + vec2(0.0, r / 2.0), center + vec2(r, -r / 2.0)]
    } else {
        vec![center + vec2(-r / 2.0, -r), center + vec2(r / 2.0, 0.0), center + vec2(-r / 2.0, r)]
    };
    ui.painter().add(Shape::line(points, stroke));
}

/// A filled button whose fill and label change under the pointer and
/// again while pressed, outlined in the label color while either holds.
fn button(ui: &mut Ui, label: &str, b: ButtonStyle) -> Response {
    let galley =
        WidgetText::from(label).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
    let padding = ui.spacing().button_padding;
    let (rect, response) = ui.allocate_exact_size(galley.size() + 2.0 * padding, Sense::click());
    let (fill, text) = if response.is_pointer_button_down_on() {
        (b.pressed_fill, b.hover_text)
    } else if response.hovered() {
        (b.hover_fill, b.hover_text)
    } else {
        (b.fill, b.text)
    };
    let radius = ui.visuals().widgets.inactive.corner_radius;
    ui.painter().rect_filled(rect, radius, fill);
    if response.hovered() {
        ui.painter().rect_stroke(rect, radius, Stroke::new(1.0_f32, text), StrokeKind::Inside);
    }
    ui.painter().galley(rect.min + padding, galley, text);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Keeps where this row's text lines start for the arrows, and, when it has
/// focus, which line the cursor is on.
fn record_spot(view: &mut TasksView, id: &str, output: &egui::text_edit::TextEditOutput) {
    let lines = &output.galley.rows;
    let before_last = lines.len().saturating_sub(1);
    view.drawing.push(RowSpot {
        id: id.to_string(),
        first_line_chars: lines.first().map_or(0, |l| l.char_count_excluding_newline()),
        last_line_start: lines[..before_last]
            .iter()
            .map(|l| l.char_count_including_newline())
            .sum(),
        last_line_chars: lines.last().map_or(0, |l| l.char_count_excluding_newline()),
    });
    if !output.response.has_focus() {
        return;
    }
    view.caret = output.cursor_range.filter(|range| range.is_empty()).map(|range| {
        let at = range.primary.rcursor;
        Caret {
            id: id.to_string(),
            first_line: at.row == 0,
            last_line: at.row >= before_last,
            column: at.column,
        }
    });
}

/// Up on the focused row's first line moves to the row above, landing on its
/// last line at the same column, and Down on the last line moves to the row
/// below. Unmodified only, so Shift+arrows still select.
fn step_at_edge(ui: &mut Ui, view: &mut TasksView, id: &str) {
    let Some(caret) = view.caret.as_ref().filter(|c| c.id == id) else { return };
    if !ui.memory(|m| m.has_focus(Id::new(("task-row", id)))) {
        return;
    }
    let (up, down) = ui.input_mut(|i| {
        (
            caret.first_line && take_plain(i, Key::ArrowUp),
            caret.last_line && take_plain(i, Key::ArrowDown),
        )
    });
    let Some(at) = view.spots.iter().position(|s| s.id == id) else { return };
    let target = match (up, down) {
        (true, _) => at.checked_sub(1).and_then(|i| view.spots.get(i)),
        (_, true) => view.spots.get(at + 1),
        _ => None,
    };
    let Some(target) = target else { return };
    let index = match up {
        true => target.last_line_start + caret.column.min(target.last_line_chars),
        false => caret.column.min(target.first_line_chars),
    };
    let target_id = Id::new(("task-row", &target.id));
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), target_id).unwrap_or_default();
    let cursor = egui::text::CCursorRange::one(egui::text::CCursor::new(index));
    state.cursor.set_char_range(Some(cursor));
    state.store(ui.ctx(), target_id);
    ui.memory_mut(|m| m.request_focus(target_id));
    view.current = Some(target.id.clone());
    view.caret = None;
}

fn take_plain(input: &mut egui::InputState, key: Key) -> bool {
    let pressed = |e: &egui::Event| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none());
    let at = input.events.iter().position(pressed);
    at.map(|at| input.events.remove(at)).is_some()
}

/// A description is one line, so a pasted break or tab becomes a space.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ").trim().to_string()
}

fn row_menu(ui: &mut Ui, view: &mut TasksView, id: &str, shortcuts: &Shortcuts) {
    for (label, start) in [("Start", true), ("Stop", false)] {
        if ui.button(label).clicked() {
            let edit = if start { Edit::Start(id.to_string()) } else { Edit::Stop(id.to_string()) };
            view.write_one(id, edit);
            ui.close_menu();
        }
    }
    // A row inside a selection of several deletes all of it.
    let picked = view.selection();
    let batch = picked.len() > 1 && picked.iter().any(|p| p == id);
    for action in RowAction::MENU {
        if action == RowAction::Delete || action == RowAction::Indent {
            ui.separator();
        }
        let batch_delete = batch && action == RowAction::Delete;
        let label = match batch_delete {
            true => format!("Delete {} tasks", picked.len()),
            false => action.label().to_string(),
        };
        let mut button = egui::Button::new(label);
        if let Some(keys) = crate::command_palette::first_key(shortcuts, action.named()) {
            button = button.shortcut_text(keys);
        }
        if ui.add(button).clicked() {
            match batch_delete {
                true => view.request_delete(picked.clone()),
                false => view.act_on(id, action),
            }
            ui.close_menu();
        }
    }
}

const GRIP_WIDTH: f32 = 12.0;

fn grip(ui: &mut Ui, c: Style, sense: Sense) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(GRIP_WIDTH, ui.spacing().interact_size.y), sense);
    let lit = response.dragged() || (response.hovered() && sense.senses_drag());
    let color = if lit { c.text } else { c.dim };
    let font = egui::FontId::proportional(12.0);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "⠿", font, color);
    response
}

/// A task dragged over `row` from the same section lands beside it, on the
/// side of the row the pointer is on. Read from the raw payload rather than a
/// drop zone widget, so no extra rect takes the row's own hover.
fn accept_drop(
    ui: &mut Ui,
    view: &mut TasksView,
    index: usize,
    row: &Row,
    tasks: &[Task],
    rect: egui::Rect,
    c: Style,
) {
    let Some(dragged) = DragAndDrop::payload::<DraggedTask>(ui.ctx()) else { return };
    let node = view.section(index).node.clone();
    if dragged.node != node || dragged.id == row.id {
        return;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos()).filter(|p| rect.contains(*p));
    let Some(pointer) = pointer else { return };
    let landing = if pointer.y < rect.center().y { Landing::Before } else { Landing::After };
    let y = match landing {
        Landing::Before => rect.top(),
        Landing::After => rect.bottom(),
    };
    ui.painter().hline(rect.x_range(), y, Stroke::new(2.0_f32, c.text));
    if ui.input(|i| i.pointer.any_released()) {
        let refs: Vec<&Task> = tasks.iter().collect();
        let edits = tree::move_to(&refs, &dragged.id, &row.id, landing);
        view.rearrange(&dragged.id, edits);
        DragAndDrop::clear_payload(ui.ctx());
    }
}

fn show_confirm(ctx: &egui::Context, view: &mut TasksView) {
    let Some(confirm) = &view.confirm else { return };
    let mut answer = None;
    let modal = Modal::new(Id::new("tasks-confirm-delete")).show(ctx, |ui| {
        ui.label(&confirm.prompt);
        ui.horizontal(|ui| {
            if ui.button("Delete").clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            answer = Some(true);
        }
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    match answer {
        Some(true) => {
            let ids = view.confirm.take().map(|c| c.ids).expect("present above");
            view.delete(&ids);
        },
        Some(false) => view.confirm = None,
        None => {},
    }
}

/// A store may reject an empty description, so a new row exists only here
/// until it has text; leaving it empty drops it. Each non-empty line of the
/// text becomes its own task, so a pasted list adds one task per line.
fn show_new_row(ui: &mut Ui, view: &mut TasksView, index: usize, c: Style) {
    let Some(new) = view.new_row.as_mut() else { return };
    let mut committed = None;
    let mut dropped = false;
    let mut focused = false;
    ui.horizontal_top(|ui| {
        ui.add_space(new.depth as f32 * INDENT + GRIP_WIDTH);
        toggle(ui, None, c.chevron, c.chevron_hover);
        // Multiline only so a long task wraps. Enter still ends the task
        // instead of breaking the line.
        let edit = ui.add(
            TextEdit::multiline(&mut new.text)
                .hint_text("new task")
                .desired_rows(1)
                .return_key(None)
                .lock_focus(true)
                .desired_width(f32::INFINITY),
        );
        if edit.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            edit.surrender_focus();
        }
        if new.focus {
            edit.request_focus();
            new.focus = false;
        }
        focused = edit.has_focus();
        if edit.lost_focus() {
            let lines: Vec<String> =
                new.text.lines().map(one_line).filter(|l| !l.is_empty()).collect();
            if lines.is_empty() { dropped = true } else { committed = Some(lines) }
        }
    });
    if focused {
        view.current = None;
    }
    if dropped {
        view.new_row = None;
    }
    if let Some(lines) = committed {
        let new = view.new_row.take().expect("present above");
        let refs: Vec<&Task> = view.sections()[index].tasks.iter().collect();
        let edits = tree::insert_after(&refs, &new.node, new.after.as_deref(), &lines);
        view.write(None, edits);
        view.show_added(lines.into_iter().map(|text| NewRow { text, ..new.clone() }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritree_tasks::fake::{FakeBackend, task};
    use std::path::PathBuf;

    fn project(root: &str, name: &str) -> Project {
        Project {
            root: PathBuf::from(root),
            name: name.into(),
            label: None,
            vcs: None,
            trunk: None,
            checkouts: Vec::new(),
            expanded: false,
            shell_override: None,
            home: None,
        }
    }

    fn worktree(path: &str, name: &str, branch: Option<&str>) -> Checkout {
        Checkout {
            name: name.into(),
            path: PathBuf::from(path),
            head: alacritree_vcs::Head { name: branch.map(Into::into), ..Default::default() },
            is_main: false,
            gone: false,
            upstream: None,
        }
    }

    #[test]
    fn home_has_only_global() {
        let s = Scope::for_workspace(None, None);
        assert_eq!((s.repo, s.workspace), (None, None));
    }

    #[test]
    fn a_worktree_names_repo_and_branch() {
        let p = project("C:/src/alacritree", "alacritree");
        let w = worktree("C:/src/wt/feat", "feat", Some("feat/x"));
        let s = Scope::for_workspace(Some(&p), Some(&w));
        assert_eq!(s.repo.as_deref(), Some("alacritree"));
        assert_eq!(s.workspace.as_deref(), Some("alacritree.feat-x"));
    }

    #[test]
    fn a_detached_worktree_uses_its_directory_name() {
        let p = project("C:/src/r", "r");
        let w = worktree("C:/src/wt/review", "review", None);
        assert_eq!(Scope::for_workspace(Some(&p), Some(&w)).workspace.as_deref(), Some("r.review"));
    }

    /// The first frame reads the nodes the store holds the tasks under, even
    /// when the sidebar's names differ from git's and git never answers.
    #[test]
    fn the_first_scope_names_the_nodes_git_would() {
        let main = worktree("C:/src/monorepo", "main", Some("trunk"));
        let linked = worktree("C:/src/wt/feat", "feat", Some("feat/x"));
        let detached = alacritree_vcs::Head {
            name: None,
            revision: Some("abc1234".into()),
            ..Default::default()
        };
        let review = Checkout { head: detached, ..worktree("C:/src/wt/review", "review", None) };
        let project = Project {
            name: "feat".into(),
            checkouts: vec![Checkout { is_main: true, ..main }, linked.clone(), review.clone()],
            ..project("C:/src/wt/feat", "feat")
        };
        let git = |checkout: &Checkout| {
            crate::tasks::facts::place_from(Some(&alacritree_vcs::Located {
                main: PathBuf::from("C:/src/monorepo"),
                bare: false,
                checkout: Some(checkout.path.clone()),
                head: checkout.head.clone(),
            }))
        };
        for checkout in [&linked, &review] {
            let seeded = Scope::for_workspace(Some(&project), Some(checkout));
            let mut answered = seeded.clone();
            answered.adopt(&git(checkout));
            assert_eq!(seeded, answered, "{}", checkout.path.display());
        }
    }

    /// Polls at `now` until the lookup in flight lands, the way frames do.
    fn settle(resolver: &mut Resolver, now: Instant) -> Option<Place> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(place) = resolver.poll(now) {
                return Some(place);
            }
            resolver.lookup.as_ref()?;
            assert!(Instant::now() < deadline, "the lookup never landed");
            std::thread::yield_now();
        }
    }

    /// A worktree the sidebar placed in a repository is in one, so git
    /// finding none means it could not be asked, and it is asked again.
    #[test]
    fn a_lookup_that_finds_no_repository_is_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("myrepo");
        std::fs::create_dir(&path).unwrap();
        let backends = vec![Vcs::Git(alacritree_git::GitBackend)];
        let mut resolver = Resolver::new(path.clone(), backends);
        let start = Instant::now();
        assert_eq!(settle(&mut resolver, start), None, "no answer while git finds nothing");

        alacritree_git::test_support::init_repo_on(&path, "trunk");
        assert_eq!(settle(&mut resolver, start), None, "the retry waits");
        let later = start + Duration::from_secs(3600);
        assert_eq!(
            settle(&mut resolver, later),
            Some(Place::Workspace { repo: "myrepo".into(), branch: "trunk".into() })
        );
    }

    /// Polls until the listing in flight lands, the way frames do.
    fn settle_reload(view: &mut TasksView) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            view.tick();
            if view.reload.is_none() && view.stamp_job.is_none() {
                return;
            }
            assert!(Instant::now() < deadline, "the listing never landed");
            std::thread::yield_now();
        }
    }

    /// An unchanged replica lists once. Later ticks inside the reload
    /// interval, and past it, run no second export.
    #[test]
    fn an_unchanged_replica_lists_once() {
        let store =
            alacritree_tasks::fake::FakeBackend::with_tasks(vec![alacritree_tasks::fake::task(
                "a", "r",
            )]);
        let scope = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        let mut view = TasksView::new(
            Backend::Fake(store.clone()),
            scope,
            None,
            Vec::new(),
            Prefs::default(),
            None,
        );
        settle_reload(&mut view);
        assert_eq!(store.listings(), 1);
        view.tick();
        assert!(view.reload.is_none());
        view.last_reload = Some(Instant::now() - RELOAD_EVERY - Duration::from_millis(1));
        view.tick();
        assert!(view.reload.is_none(), "a due tick still lists");
        assert_eq!(store.listings(), 1);
    }

    fn tab(store: &alacritree_tasks::fake::FakeBackend) -> TasksView {
        let scope = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        TasksView::new(
            Backend::Fake(store.clone()),
            scope,
            None,
            Vec::new(),
            Prefs::default(),
            None,
        )
    }
    fn due(view: &mut TasksView) {
        view.last_reload = Some(Instant::now() - RELOAD_EVERY - Duration::from_millis(1));
        view.tick();
    }
    #[test]
    fn a_write_elsewhere_lists_on_the_next_due_tick() {
        let store =
            alacritree_tasks::fake::FakeBackend::with_tasks(vec![alacritree_tasks::fake::task(
                "a", "r",
            )]);
        let mut view = tab(&store);
        settle_reload(&mut view);
        store.written_elsewhere(vec![alacritree_tasks::fake::task("b", "r")]);
        due(&mut view);
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2);
        assert_eq!(view.tasks.len(), 1);
    }
    fn land(view: &mut TasksView, place: Place) {
        view.resolving = Some(Resolver {
            dir: PathBuf::from("."),
            backends: Vec::new(),
            lookup: Some(Job::ready(place)),
            retry_at: Instant::now(),
            backoff: FIRST_RETRY,
        });
        view.tick();
    }
    #[test]
    fn our_own_write_lists_without_waiting_for_the_stamp() {
        let store =
            alacritree_tasks::fake::FakeBackend::with_tasks(vec![alacritree_tasks::fake::task(
                "a", "r",
            )]);
        let mut view = tab(&store);
        settle_reload(&mut view);
        let stamp = store.stamp();
        view.write(None, vec![Edit::Done("a".into())]);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            view.tick();
            let written = store.edits().iter().any(|edit| matches!(edit, Edit::Done(_)));
            let listed_after = store.listed_after().iter().any(|n| *n >= 1);
            if written && listed_after && view.reload.is_none() {
                break;
            }
            assert!(Instant::now() < deadline, "the write never reloaded");
            std::thread::yield_now();
        }
        assert_eq!(store.listings(), 2, "a finished write lists at once");
        assert_eq!(store.stamp(), stamp, "our write does not move the stamp");
        assert_eq!(view.tasks[0].status, Status::Completed);
        assert!(view.last_reload.is_some_and(|t| t.elapsed() < RELOAD_EVERY));
    }
    #[test]
    fn a_scope_change_lists_without_waiting_for_the_stamp() {
        let store =
            alacritree_tasks::fake::FakeBackend::with_tasks(vec![alacritree_tasks::fake::task(
                "a", "r",
            )]);
        let mut view = tab(&store);
        settle_reload(&mut view);
        land(&mut view, Place::Project { repo: "other".into() });
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2);
    }
    #[test]
    fn a_change_of_what_the_tab_lists_reloads_without_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let repo = alacritree_git::test_support::init_repo_on(&dir.path().join("other"), "trunk");
        let store = alacritree_tasks::fake::FakeBackend::with_tasks(vec![
            alacritree_tasks::fake::task("a", "r"),
            alacritree_tasks::fake::task("b", "other"),
        ]);
        let scope = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        let mut view = TasksView::new(
            Backend::Fake(store.clone()),
            scope,
            Some(repo),
            crate::vcs::backends(&Default::default()),
            Prefs::default(),
            None,
        );
        settle_reload(&mut view);
        assert_eq!(view.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let stamp = store.stamp();
        let deadline = Instant::now() + Duration::from_secs(10);
        while view.resolving.is_some() {
            view.tick();
            assert!(Instant::now() < deadline, "git never named the worktree");
            std::thread::yield_now();
        }
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2, "a new filter lists at once");
        assert_eq!(store.stamp(), stamp, "a filter change does not move the stamp");
        assert_eq!(view.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert!(view.last_reload.is_some_and(|t| t.elapsed() < RELOAD_EVERY));
    }

    /// After the first listing, an unchanged WSL replica runs no task command.
    #[cfg(windows)]
    #[test]
    fn an_unchanged_wsl_replica_runs_no_task_command() {
        let Some(distro) = wsl::distros().into_iter().find(|d| d.is_default).map(|d| d.name) else {
            return;
        };
        let root = PathBuf::from(format!(r"\\wsl.localhost\{distro}\tmp"));
        let Ok(dir) = tempfile::Builder::new().prefix("alacritree-task").tempdir_in(&root) else {
            return;
        };
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("taskchampion.sqlite3"), b"db").unwrap();
        let log = dir.path().join("spawns.txt");
        let linux = wsl::windows_to_linux(dir.path()).expect("the temp dir is inside the distro");
        let script = format!("{linux}/task");
        let body = format!(
            "#!/bin/sh\necho spawn >> {linux}/spawns.txt\ncase $* in\n*_get*) printf \
             {linux}/data;;\n*) printf []\nesac\n"
        );
        std::fs::write(dir.path().join("task"), body.replace("\r\n", "\n")).unwrap();
        alacritree_common::command_ext::hidden("wsl.exe")
            .args(["-d", &distro, "--exec", "chmod", "+x", &script])
            .output()
            .unwrap();
        let _guard = alacritree_common::tools::test_configuration_lock().lock().unwrap();
        let saved = alacritree_common::tools::test_configuration();
        let mut paths = saved.clone();
        paths[alacritree_common::tools::Tool::Task as usize].wsl = Some(script);
        alacritree_common::tools::configure(paths);
        let was_enabled = alacritree_common::wsl_helper::enabled();
        alacritree_common::wsl_helper::set_enabled(false);
        let scope = Scope { side: Side::Wsl(distro), repo: Some("r".into()), workspace: None };
        let mut view = TasksView::new(
            Backend::Taskwarrior(alacritree_taskwarrior::Taskwarrior::default()),
            scope,
            None,
            Vec::new(),
            Prefs::default(),
            None,
        );
        settle_reload(&mut view);
        let after_listing = std::fs::read_to_string(&log).unwrap_or_default().lines().count();
        view.last_reload = Some(Instant::now() - RELOAD_EVERY - Duration::from_millis(1));
        view.tick();
        view.last_reload = Some(Instant::now() - RELOAD_EVERY - Duration::from_millis(1));
        view.tick();
        let after_ticks = std::fs::read_to_string(&log).unwrap_or_default().lines().count();
        alacritree_common::wsl_helper::set_enabled(was_enabled);
        alacritree_common::tools::configure(saved);
        assert!(view.load_error().is_none(), "{}", view.load_error().unwrap_or_default());
        assert_eq!(after_ticks, after_listing, "unchanged ticks ran a task command");
        assert!(after_listing >= 1, "the first listing located the replica");
    }
    /// Several unchanged reload ticks of the kali-linux replica start no task after the first
    /// listing.
    #[ignore = "runs task inside kali-linux and prints the process count"]
    #[cfg(windows)]
    #[test]
    fn unchanged_kali_reload_ticks_start_no_task() {
        let side = Side::Wsl("kali-linux".into());
        let tw = alacritree_taskwarrior::Taskwarrior::default();
        let mut known = None;
        alacritree_taskwarrior::reset_task_runs();
        let mut stamps = Vec::new();
        for _ in 0..4 {
            let (stamp, dir) = jobs::on_this_thread(|b| {
                backend::replica_stamp(
                    &Backend::Taskwarrior(tw.clone()),
                    &side,
                    known.as_deref(),
                    b,
                )
            });
            known = dir;
            stamps.push(stamp.is_some());
        }
        let runs = alacritree_taskwarrior::task_runs();
        println!("kali-linux task processes: {runs}");
        assert!(stamps.iter().all(|found| *found), "a tick found no replica");
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_replica_with_no_stamp_reloads_every_interval() {
        let store = alacritree_tasks::fake::FakeBackend::default().unstamped();
        let mut view = tab(&store);
        settle_reload(&mut view);
        due(&mut view);
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2);
    }

    /// A listing that panicked frees the slot and the next due tick lists
    /// again, instead of treating the missed export as current.
    #[test]
    fn a_listing_that_panicked_lists_again_on_the_next_due_tick() {
        let store = alacritree_tasks::fake::FakeBackend::default();
        let mut view = tab(&store);
        settle_reload(&mut view);
        view.reload = Some(PendingReload {
            epoch: view.reload_epoch(),
            scope: view.scope.clone(),
            job: Job::panicked(),
        });
        view.tick();
        assert!(view.reload.is_none());
        due(&mut view);
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2, "a panicked listing is listed again");
    }

    /// A listing that returns an error is listed again on the next due tick.
    #[test]
    fn a_listing_that_fails_lists_again_on_the_next_due_tick() {
        let store = alacritree_tasks::fake::FakeBackend::default();
        let mut view = tab(&store);
        settle_reload(&mut view);
        let failed = Err(TaskError::Failed { program: "fake".into(), stderr: "down".into() });
        view.reload = Some(PendingReload {
            epoch: view.reload_epoch(),
            scope: view.scope.clone(),
            job: Job::ready(failed),
        });
        view.tick();
        due(&mut view);
        settle_reload(&mut view);
        assert_eq!(store.listings(), 2, "a failed listing is listed again");
    }

    /// A listing started for the repository subtree must not land in the
    /// workspace cache once git has named the workspace.
    #[test]
    fn a_listing_from_an_earlier_scope_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let store = FakeBackend::with_tasks(vec![
            task("repo", "r"),
            task("feat", "r.feat"),
            task("sibling", "r.main"),
            task("session", "r.main.codex-9"),
        ]);
        let started = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        let mut view = TasksView::new(
            Backend::Fake(store),
            started.clone(),
            None,
            Vec::new(),
            Prefs::default(),
            Some(dir.path().to_path_buf()),
        );
        view.stale = true;
        view.last_reload = Some(Instant::now());
        view.tick();
        let started_listing =
            view.reload.as_ref().expect("the repository listing is in flight").scope.clone();
        assert_eq!(started_listing, started);
        view.scope.adopt(&Place::Workspace { repo: "r".into(), branch: "feat".into() });
        let current = view.scope.clone();
        assert_ne!(started_listing, current);
        // A due tick would start the current-scope listing in the same call
        // that discards this one, and that listing would fill the tab.
        view.last_reload = Some(Instant::now());

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            view.tick();
            if view.reload.is_none() {
                break;
            }
            view.last_reload = Some(Instant::now());
            assert!(Instant::now() < deadline, "the listing never landed");
            std::thread::yield_now();
        }

        let nodes: Vec<&str> = view.tasks.iter().map(Task::node).collect();
        assert!(nodes.is_empty(), "the earlier listing is not the tab's: {nodes:?}");
        assert!(view.cache_write.is_none(), "no cache write is queued");
        assert!(!current.cache_file(dir.path()).exists(), "the workspace cache stays empty");
        assert!(!started.cache_file(dir.path()).exists(), "the old scope is not written either");
    }

    #[test]
    fn a_detached_checkout_names_the_node_task_scope_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = alacritree_git::test_support::init_repo(&dir.path().join("review"));
        alacritree_git::test_support::detach(&root);

        let project = jobs::on_this_thread(|b| {
            Project::discover(root.clone(), &crate::vcs::backends(&Default::default()), false, b)
        })
        .project;
        let worktree = &project.checkouts[0];
        let backends = crate::vcs::backends(&Default::default());
        let (_, place) =
            jobs::on_this_thread(|b| crate::tasks::facts::place_for(&worktree.path, &backends, b));
        let mut scope = Scope::for_workspace(Some(&project), Some(worktree));
        scope.adopt(&place);
        assert_eq!(scope.workspace.as_deref(), Some("review.review"));
    }

    #[test]
    fn a_non_git_root_has_a_project_section_only() {
        let s = Scope::for_workspace(Some(&project("C:/notes", "notes")), None);
        assert_eq!((s.repo.as_deref(), s.workspace), (Some("notes"), None));
    }

    #[cfg(windows)]
    #[test]
    fn a_distro_project_runs_on_its_distro() {
        let p = project(r"\\wsl.localhost\Ubuntu\home\lev\r", "r");
        assert_eq!(Scope::for_workspace(Some(&p), None).side, Side::Wsl("Ubuntu".into()));
    }

    #[test]
    fn a_repo_lists_its_subtree_and_the_global_list() {
        let s = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        assert_eq!(s.filter().nodes, [
            NodeMatch::Exact(GLOBAL.into()),
            NodeMatch::Subtree("r".into()),
        ]);
    }

    /// A resolved worktree asks for its own sessions, never a sibling
    /// worktree's. The listing is what the tab holds after a tick.
    #[test]
    fn a_worktree_does_not_receive_a_sibling_worktrees_session_tasks() {
        let tasks = vec![
            task("repo", "r"),
            task("plan", "r.feat"),
            task("feat-session", "r.feat.claude-1"),
            task("feat-extra", "r.feat-extra"),
            task("main-session", "r.main.codex-9"),
            task("near", "r-web.main"),
            task("blank", ""),
            Task { project: None, ..task("errand", GLOBAL) },
        ];
        let nodes = |workspace| {
            let mut view = TasksView::new(
                Backend::Fake(FakeBackend::with_tasks(tasks.clone())),
                workspace_scope(workspace),
                None,
                Vec::new(),
                Prefs::default(),
                None,
            );
            view.stale = true;
            view.last_reload = Some(Instant::now());
            view.tick();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                view.tick();
                if view.reload.is_none() && view.loaded {
                    break;
                }
                view.last_reload = Some(Instant::now());
                assert!(Instant::now() < deadline, "the listing never landed");
                std::thread::yield_now();
            }
            view.tasks.iter().map(|t| t.node().to_string()).collect::<Vec<_>>()
        };
        let feat = nodes("r.feat");
        assert!(feat.contains(&"r.feat.claude-1".to_string()), "{feat:?}");
        assert!(feat.contains(&"r".to_string()), "{feat:?}");
        assert!(feat.iter().any(|n| n == GLOBAL), "{feat:?}");
        assert!(!feat.contains(&"r.main.codex-9".to_string()), "{feat:?}");
        assert!(!feat.contains(&"r.feat-extra".to_string()), "{feat:?}");
        assert!(!feat.contains(&"r-web.main".to_string()), "{feat:?}");
        assert!(!feat.contains(&String::new()), "{feat:?}");
        let main = nodes("r.main");
        assert!(main.contains(&"r.main.codex-9".to_string()), "{main:?}");
        assert!(!main.contains(&"r.feat.claude-1".to_string()), "{main:?}");
    }

    /// Git has not named the workspace yet, so the tab still asks for the
    /// repository subtree the sidebar already knows, and nothing narrower.
    #[test]
    fn an_unresolved_workspace_still_lists_the_repository_subtree() {
        let scope = Scope { side: Side::Native, repo: Some("r".into()), workspace: None };
        let store = FakeBackend::with_tasks(vec![
            task("repo", "r"),
            task("feat", "r.feat"),
            task("session", "r.feat.claude-1"),
            task("other", "other.main"),
            task("near", "r-web"),
            task("blank", ""),
        ]);
        let listed = jobs::on_this_thread(|b| store.list(&scope.side, &scope.filter(), b)).unwrap();
        let nodes: Vec<&str> = listed.iter().map(Task::node).collect();
        assert_eq!(nodes, ["r", "r.feat", "r.feat.claude-1"]);
    }

    fn workspace_scope(workspace: &str) -> Scope {
        Scope { side: Side::Native, repo: Some("r".into()), workspace: Some(workspace.into()) }
    }

    fn in_workspace(tasks: Vec<Task>) -> TasksView {
        let scope =
            Scope { side: Side::Native, repo: Some("r".into()), workspace: Some("r.feat".into()) };
        let backend = Backend::from_config(&Default::default());
        let mut view = TasksView::new(backend, scope, None, Vec::new(), Prefs::default(), None);
        view.tasks = tasks;
        view
    }

    /// A second frame of an unchanged listing draws the sections already
    /// built. A listing or a pending edit that changes what the rows show
    /// builds them again.
    #[test]
    fn a_redraw_reuses_sections_until_the_listing_or_a_pending_edit_changes() {
        let task = alacritree_tasks::fake::task;
        let mut h = Harness::with_view(in_workspace(vec![
            task("plan", "r.feat"),
            task("step", "r.feat.claude-1"),
        ]));
        let built = h.view.builds();
        h.frame(Vec::new());
        assert_eq!(h.view.builds(), built, "a second tab frame does not rebuild");
        let mut view = h.view;

        view.finish_reload(view.reload_epoch(), &view.scope.clone(), Ok(vec![task("plan", "r.feat")]));
        let rows: Vec<_> =
            view.sections().iter().flat_map(|b| &b.section.rows).map(|r| r.id.as_str()).collect();
        assert_eq!(rows, ["plan"]);
        let listed = view.builds();
        assert!(listed > built, "a new listing builds again");
        let _ = view.sections().len();
        assert_eq!(view.builds(), listed, "reading the new listing does not build again");

        view.show_status("plan".into(), Status::Completed);
        let _ = view.sections().len();
        let edited = view.builds();
        assert!(edited > listed, "a pending edit builds again");
        let status = view
            .sections()
            .iter()
            .flat_map(|b| &b.section.rows)
            .find(|r| r.id == "plan")
            .map(|r| r.status);
        assert_eq!(status, Some(Status::Completed));
        assert_eq!(view.builds(), edited, "the edit's sections stay built");

        let mut empty = Harness::with_view(in_workspace(Vec::new()));
        let built = empty.view.builds();
        empty.frame(Vec::new());
        assert_eq!(empty.view.builds(), built, "an empty listing's second frame does not rebuild");
        empty.view.finish_reload(empty.view.reload_epoch(), &empty.view.scope.clone(), Ok(Vec::new()));
        let _ = empty.view.sections().len();
        let reloaded = empty.view.builds();
        assert!(reloaded > built, "the reload builds once");
        empty.frame(Vec::new());
        assert_eq!(empty.view.builds(), reloaded, "the frame after the reload reuses it");
    }

    /// Git's names replace the sidebar's, and the sections built under the
    /// sidebar's names are dropped.
    #[test]
    fn git_names_drop_the_sections_built_under_the_sidebars() {
        let mut view = TasksView::new(
            Backend::from_config(&Default::default()),
            Scope { side: Side::Native, repo: Some("feat".into()), workspace: None },
            None,
            Vec::new(),
            Prefs::default(),
            None,
        );
        view.tasks = vec![alacritree_tasks::fake::task("plan", "r.feat")];
        let before: Vec<_> = view.sections().iter().map(|b| b.section.node.clone()).collect();
        assert!(before.contains(&"feat".to_string()));
        view.resolving = Some(Resolver {
            dir: PathBuf::new(),
            backends: Vec::new(),
            lookup: Some(Job::ready(Place::Workspace { repo: "r".into(), branch: "feat".into() })),
            retry_at: Instant::now(),
            backoff: FIRST_RETRY,
        });
        view.tick();
        let after: Vec<_> = view.sections().iter().map(|b| b.section.node.clone()).collect();
        assert_ne!(after, before);
        assert!(after.contains(&"r".to_string()));
        assert!(after.contains(&"r.feat".to_string()));
    }

    /// Times one frame of listing work on both bench inputs.
    ///
    ///     cargo test -p alacritree --release -- --ignored task_sections_bench --nocapture
    #[test]
    #[ignore]
    fn task_sections_bench() {
        println!(
            "The workcell listing was replaced by the generated one because it no longer exists."
        );
        let (generated, repo, workspace) = issue_shaped_listing();
        println!("{}", time_listing("generated", generated, repo, workspace));
        let devkit = PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
            .join(r"alacritree\tasks\wsl-kali-linux-devkit.json");
        let cached: Vec<Task> = serde_json::from_slice(&std::fs::read(&devkit).unwrap()).unwrap();
        let (repo, workspace) = scope_of(&cached);
        println!("{}", time_listing(&devkit.display().to_string(), cached, repo, workspace));
    }

    /// 5609 tasks on 1183 project nodes, 317 of them pending. `workcell.main`
    /// holds about 67 sections and 512 rows; the rest sit on other nodes.
    fn issue_shaped_listing() -> (Vec<Task>, Option<String>, Option<String>) {
        const TASKS: usize = 5609;
        const PROJECT_NODES: usize = 1183;
        const PENDING: usize = 317;
        const REPO: &str = "workcell";
        const WORKSPACE: &str = "workcell.main";
        const WORKSPACE_ROWS: usize = 512;
        const SESSION_SECTIONS: usize = 65;
        let per_section = WORKSPACE_ROWS / (SESSION_SECTIONS + 1);
        let remainder = WORKSPACE_ROWS % (SESSION_SECTIONS + 1);
        let nodes_left = PROJECT_NODES - (SESSION_SECTIONS + 2);
        let tasks_left = TASKS - WORKSPACE_ROWS - 1;
        let per_node = tasks_left / nodes_left;
        let node_remainder = tasks_left % nodes_left;
        let mut plan = vec![(REPO.to_string(), 1), (WORKSPACE.to_string(), per_section)];
        plan.extend((0..SESSION_SECTIONS).map(|session| {
            (
                format!("{WORKSPACE}.claude-{session}"),
                per_section + usize::from(session < remainder),
            )
        }));
        plan.extend(
            (0..nodes_left).map(|node| {
                (format!("repo.n{node}"), per_node + usize::from(node < node_remainder))
            }),
        );
        let mut tasks = Vec::with_capacity(TASKS);
        for (node, rows) in plan {
            for _ in 0..rows {
                let i = tasks.len();
                let mut task = alacritree_tasks::fake::task(&format!("t{i}"), &node);
                if i >= PENDING {
                    task.status = Status::Completed;
                }
                tasks.push(task);
            }
        }
        assert_eq!(tasks.len(), TASKS);
        assert_eq!(tasks.iter().filter(|t| t.status == Status::Pending).count(), PENDING);
        assert_eq!(scope_nodes(&tasks).len(), PROJECT_NODES);
        let scope = tree::sections(&tasks, Some(REPO), Some(WORKSPACE));
        let drawn: Vec<_> = scope.iter().filter(|s| !s.rows.is_empty()).collect();
        assert_eq!(
            drawn.len(),
            SESSION_SECTIONS + 2,
            "the project, the workspace and its sessions"
        );
        let prefix = format!("{WORKSPACE}.");
        let workspace_rows: usize = drawn
            .iter()
            .filter(|s| s.node == WORKSPACE || s.node.starts_with(&prefix))
            .map(|s| s.rows.len())
            .sum();
        assert_eq!(workspace_rows, WORKSPACE_ROWS);
        (tasks, Some(REPO.into()), Some(WORKSPACE.into()))
    }

    fn time_listing(
        label: &str,
        tasks: Vec<Task>,
        repo: Option<String>,
        workspace: Option<String>,
    ) -> String {
        let nodes = scope_nodes(&tasks);
        let pending = tasks.iter().filter(|t| t.status == Status::Pending).count();
        let task_count = tasks.len();
        let before = median(|| frame_before(&tasks, repo.as_deref(), workspace.as_deref()));
        let drawn = tree::sections(&tasks, repo.as_deref(), workspace.as_deref());
        let held: Vec<_> = drawn.iter().filter(|s| !s.rows.is_empty()).collect();
        let rows: usize = held.iter().map(|s| s.rows.len()).sum();
        let view = in_scope(tasks, repo.as_deref(), workspace.as_deref());
        view.sections();
        let after = median(|| frame_after(&view));
        format!(
            "file: {label}\nlisting: {task_count} tasks, {} project nodes, {pending} \
             pending\nworkspace {}: {} sections, {rows} rows\nbefore: {before:?}\nafter: {after:?}",
            nodes.len(),
            workspace.as_deref().unwrap_or("(none)"),
            held.len(),
        )
    }

    /// One frame after the sections are kept: read them, then prepare the
    /// rows each drawn section shows, the way a frame does before egui lays
    /// them out.
    fn frame_after(view: &TasksView) {
        let drawn = view.sections().to_vec();
        let collapsed = &view.collapsed_tasks;
        for built in &drawn {
            let open;
            let rows: Vec<&Row> = match view.prefs.hide_completed {
                true => {
                    open = tree::open_rows(&built.section.rows);
                    open
                },
                false => built.section.rows.iter().collect(),
            };
            let _folded = tree::fold(&rows, collapsed);
            let _shown: Vec<&Row> = rows;
        }
    }

    /// One frame of listing work before the sections were kept: clone the
    /// listing and lay the pending edits over it, then clone and filter that
    /// listing again for each section the frame draws.
    fn frame_before(tasks: &[Task], repo: Option<&str>, workspace: Option<&str>) {
        let shown = tasks.to_vec();
        let sections = tree::sections(&shown, repo, workspace);
        for section in &sections {
            let _here: Vec<Task> =
                shown.clone().into_iter().filter(|t| t.node() == section.node).collect();
        }
    }

    /// The repository and workspace the listing's own nodes name.
    fn scope_of(tasks: &[Task]) -> (Option<String>, Option<String>) {
        let mut nodes: Vec<&str> = tasks.iter().map(|t| t.node()).collect();
        nodes.sort_by_key(|node| (node.matches('.').count(), *node));
        let workspace = nodes.iter().copied().find(|node| node.contains('.'));
        let repo = workspace
            .and_then(|node| node.split('.').next())
            .or_else(|| nodes.iter().copied().find(|node| *node != GLOBAL));
        (repo.map(str::to_string), workspace.map(str::to_string))
    }

    fn median(f: impl Fn()) -> Duration {
        let mut samples: Vec<Duration> = (0..50)
            .map(|_| {
                let started = Instant::now();
                f();
                started.elapsed()
            })
            .collect();
        samples.sort();
        samples[samples.len() / 2]
    }

    /// Every distinct node the listing holds.
    fn scope_nodes(tasks: &[Task]) -> Vec<String> {
        let mut nodes: Vec<String> = tasks
            .iter()
            .map(|t| t.node().to_string())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        nodes.sort();
        nodes
    }

    fn in_scope(tasks: Vec<Task>, repo: Option<&str>, workspace: Option<&str>) -> TasksView {
        let scope = Scope {
            side: Side::Native,
            repo: repo.map(str::to_string),
            workspace: workspace.map(str::to_string),
        };
        let backend = Backend::from_config(&Default::default());
        let mut view = TasksView::new(backend, scope, None, Vec::new(), Prefs::default(), None);
        view.finish_reload(view.reload_epoch(), &view.scope.clone(), Ok(tasks));
        view
    }

    #[test]
    fn the_sidebar_shows_the_checkout_sections_that_hold_tasks() {
        let view = in_workspace(vec![
            task("errand", GLOBAL),
            task("plan", "r.feat"),
            task("step", "r.feat.claude-1"),
        ]);
        let nodes: Vec<String> =
            view.sidebar_indexes().map(|i| view.section(i).node.clone()).collect();
        assert_eq!(nodes, ["r.feat", "r.feat.claude-1"], "no global list, no empty project");
    }

    #[test]
    fn a_section_is_named_below_the_workspace_it_sits_in() {
        let view = in_workspace(Vec::new());
        assert_eq!(view.short_name("r"), "r");
        assert_eq!(view.short_name("r.feat"), "feat");
        assert_eq!(view.short_name("r.feat.claude-1"), "claude-1");
        assert_eq!(view.short_name(GLOBAL), GLOBAL);
    }

    #[test]
    fn home_shows_the_global_list_in_the_sidebar() {
        let mut view = view_with(Vec::new(), None);
        view.tasks = vec![alacritree_tasks::fake::task("errand", GLOBAL)];
        let nodes: Vec<String> =
            view.sidebar_indexes().map(|i| view.section(i).node.clone()).collect();
        assert_eq!(nodes, [GLOBAL]);
    }

    #[test]
    fn home_filters_to_global() {
        let s = Scope::for_workspace(None, None);
        assert_eq!(s.filter().nodes, [NodeMatch::Exact(GLOBAL.into())]);
    }

    use egui::epaint::ClippedShape;
    use egui::{CentralPanel, Event, PointerButton, Pos2, RawInput, Rect, Vec2};

    fn test_style() -> Style {
        Style {
            text: Color32::WHITE,
            dim: Color32::GRAY,
            error: Color32::RED,
            chevron: Stroke::new(1.5_f32, Color32::GRAY),
            chevron_hover: Color32::WHITE,
            section_chevron: Stroke::new(2.5_f32, Color32::WHITE),
            hidden_count: Color32::GRAY,
            active_marker: Stroke::new(2.5_f32, Color32::WHITE),
            active_background: Color32::from_rgb(40, 40, 60),
            add_button: ButtonStyle {
                text: Color32::LIGHT_BLUE,
                hover_text: Color32::WHITE,
                fill: Color32::DARK_BLUE,
                hover_fill: Color32::BLUE,
                pressed_fill: Color32::BLUE,
            },
            selection: Color32::from_rgb(200, 200, 120),
            selected_text: Color32::BLACK,
            font: 12.5,
        }
    }

    #[test]
    fn the_pane_draws_in_the_terminal_font_at_the_configured_size() {
        let mut h = Harness::new(vec![pending("a", "iiii"), pending("b", "MMMM")]);
        assert_eq!(h.text("iiii").width(), h.text("MMMM").width(), "monospace");
        let (row, button) = (h.text("iiii").height(), h.text("hide completed").height());

        h.style.font *= 2.0;
        h.frame(Vec::new());
        assert!(h.text("iiii").height() > 1.8 * row);
        assert!(h.text("hide completed").height() > 1.8 * button);
    }

    fn pending(id: &str, text: &str) -> Task {
        Task {
            description: text.into(),
            order: Some(1024),
            ..alacritree_tasks::fake::task(id, GLOBAL)
        }
    }

    fn child(id: &str, text: &str, parent: &str) -> Task {
        Task { parent: Some(parent.into()), ..pending(id, text) }
    }

    fn view_with(tasks: Vec<Task>, cache_dir: Option<PathBuf>) -> TasksView {
        let scope = Scope::for_workspace(None, None);
        let backend = Backend::from_config(&Default::default());
        let mut view =
            TasksView::new(backend, scope, None, Vec::new(), Prefs::default(), cache_dir);
        if !tasks.is_empty() {
            view.tasks = tasks;
        }
        view
    }

    /// The tab drawn frame by frame with real egui input and no backend:
    /// writes collect in `ops` instead of reaching the pool.
    struct Harness {
        ctx: egui::Context,
        view: TasksView,
        shortcuts: Shortcuts,
        ops: Vec<QueuedWrite>,
        /// Painted text and where it shows, clipped to what is on screen.
        texts: Vec<(String, Rect)>,
        /// Filled rectangles and their color.
        fills: Vec<(Color32, Rect)>,
        /// The bounds of each stroked path, such as a chevron.
        paths: Vec<Rect>,
        background_clicked: bool,
        style: Style,
        /// The modifier keys held down through each frame.
        modifiers: Modifiers,
    }

    fn collect_fills(shape: &Shape, fills: &mut Vec<(Color32, Rect)>, paths: &mut Vec<Rect>) {
        match shape {
            Shape::Rect(r) => fills.push((r.fill, r.rect)),
            Shape::Path(p) => paths.push(Rect::from_points(&p.points)),
            Shape::Vec(shapes) => shapes.iter().for_each(|s| collect_fills(s, fills, paths)),
            _ => {},
        }
    }

    fn collect_texts(shape: &Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
        match shape {
            Shape::Text(t) => {
                let rect = t.galley.rect.translate(t.pos.to_vec2()).intersect(clip);
                if rect.is_positive() {
                    out.push((t.galley.text().to_string(), rect));
                }
            },
            Shape::Vec(shapes) => shapes.iter().for_each(|s| collect_texts(s, clip, out)),
            _ => {},
        }
    }

    impl Harness {
        fn new(tasks: Vec<Task>) -> Self {
            Self::with_view(view_with(tasks, None))
        }

        fn with_view(view: TasksView) -> Self {
            let mut h = Self {
                ctx: egui::Context::default(),
                view,
                shortcuts: Shortcuts::new(&[]),
                ops: Vec::new(),
                texts: Vec::new(),
                fills: Vec::new(),
                paths: Vec::new(),
                background_clicked: false,
                style: test_style(),
                modifiers: Modifiers::NONE,
            };
            h.frame(Vec::new());
            h
        }

        fn frame(&mut self, events: Vec<Event>) {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                events,
                modifiers: self.modifiers,
                ..Default::default()
            };
            let (view, shortcuts) = (&mut self.view, &self.shortcuts);
            let mut clicked = false;
            let style = self.style;
            let out = self.ctx.run(input, |ctx| {
                CentralPanel::default().show(ctx, |ui| {
                    clicked = draw(ui, view, true, shortcuts, style).clicked();
                });
            });
            self.background_clicked = clicked;
            self.ops.append(&mut self.view.outbox);
            self.texts.clear();
            self.fills.clear();
            self.paths.clear();
            let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
            for ClippedShape { clip_rect, shape } in &out.shapes {
                collect_texts(shape, clip_rect.intersect(screen), &mut self.texts);
                collect_fills(shape, &mut self.fills, &mut self.paths);
            }
        }

        fn visible(&self, text: &str) -> Option<Rect> {
            self.texts.iter().find(|(t, _)| t == text).map(|(_, r)| *r)
        }

        fn text(&self, text: &str) -> Rect {
            self.visible(text).unwrap_or_else(|| panic!("{text:?} not on screen: {:?}", self.texts))
        }

        /// The checkbox drawn just left of the row showing `text`.
        fn checkbox(&self, text: &str) -> Pos2 {
            let rect = self.text(text);
            Pos2::new(rect.left() - 18.0, rect.center().y)
        }

        /// The centre of the chevron painted on the row showing `text`.
        fn painted_chevron(&self, text: &str) -> Pos2 {
            let row = self.text(text);
            let on_row =
                |r: &&Rect| r.right() < row.left() && (r.center().y - row.center().y).abs() < 4.0;
            let leftmost =
                self.paths.iter().filter(on_row).min_by(|a, b| a.left().total_cmp(&b.left()));
            leftmost.unwrap_or_else(|| panic!("no chevron on {text:?}")).center()
        }

        /// The sub-task toggle left of the checkbox on the row showing `text`.
        fn chevron(&self, text: &str) -> Pos2 {
            self.checkbox(text) - vec2(22.0, 0.0)
        }

        fn press(&mut self, pos: Pos2, button: PointerButton) {
            let event =
                |pressed| Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE };
            self.frame(vec![Event::PointerMoved(pos), event(true)]);
            self.frame(vec![event(false)]);
            self.frame(Vec::new());
        }

        fn click(&mut self, pos: Pos2) {
            self.press(pos, PointerButton::Primary);
        }

        /// Holds `modifiers` down, points at `pos` for a frame, then clicks
        /// it, the way a hand holds a key before clicking.
        fn click_with(&mut self, pos: Pos2, modifiers: Modifiers) {
            self.modifiers = modifiers;
            self.frame(vec![Event::PointerMoved(pos)]);
            let event = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers,
            };
            self.frame(vec![event(true)]);
            self.frame(vec![event(false)]);
            self.modifiers = Modifiers::NONE;
            self.frame(Vec::new());
        }

        fn key(&mut self, key: Key) {
            self.key_with(key, Modifiers::NONE);
        }

        fn key_with(&mut self, key: Key, modifiers: Modifiers) {
            self.frame(vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }]);
            self.frame(Vec::new());
        }

        /// Focuses the row showing `text`, with the cursor at its end.
        fn edit_end(&mut self, text: &str) {
            let rect = self.text(text);
            self.click(Pos2::new(rect.right() + 200.0, rect.center().y));
        }

        fn edits(&self) -> Vec<&Edit> {
            self.ops.iter().flat_map(|(_, edits)| edits).collect()
        }

        fn deleted(&self) -> bool {
            self.edits().iter().any(|e| matches!(e, Edit::Delete(_)))
        }
    }

    #[test]
    fn clicking_a_checkbox_marks_the_task_done_at_once() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.click(h.checkbox("one"));
        assert_eq!(h.ops, [(Some("a".to_string()), vec![Edit::Done("a".into())])]);
        assert_eq!(h.view.plain_lines(), ["## global", "- [x] one"]);
    }

    #[test]
    fn a_toggle_holds_until_a_reload_that_follows_the_write() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.click(h.checkbox("one"));
        let before_the_write = h.view.reload_epoch();
        h.view.finish_write(Some("a".into()), Ok(()));
        h.view.finish_reload(before_the_write, &h.view.scope.clone(), Ok(vec![pending("a", "one")]));
        assert_eq!(h.view.plain_lines()[1], "- [x] one", "a reload older than the write");
        let after_the_write = h.view.reload_epoch();
        h.view.finish_reload(after_the_write, &h.view.scope.clone(), Ok(vec![pending("a", "one")]));
        assert_eq!(h.view.plain_lines()[1], "- [ ] one", "the store has the last word");
    }

    #[test]
    fn clicking_empty_space_reaches_the_pane() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        let pos = Pos2::new(400.0, 550.0);
        h.frame(vec![Event::PointerMoved(pos), Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }]);
        h.frame(vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
        assert!(h.background_clicked);
    }

    #[test]
    fn backspace_on_the_last_character_keeps_the_task() {
        let mut h = Harness::new(vec![pending("a", "x")]);
        h.edit_end("x");
        h.key(Key::Backspace);
        assert_eq!(h.view.drafts.get("a").map(String::as_str), Some(""));
        assert!(!h.deleted());
    }

    #[test]
    fn backspace_on_an_empty_row_deletes_it() {
        let mut h = Harness::new(vec![pending("a", "x")]);
        h.edit_end("x");
        h.key(Key::Backspace);
        h.key(Key::Backspace);
        assert!(h.deleted());
        assert_eq!(h.view.plain_lines(), ["## global"], "gone before the store answers");
    }

    #[test]
    fn a_cleared_row_left_behind_shows_its_text_again() {
        let mut h = Harness::new(vec![pending("a", "x")]);
        h.edit_end("x");
        h.key(Key::Backspace);
        h.click(Pos2::new(400.0, 550.0));
        assert!(h.view.drafts.is_empty());
        assert!(h.ops.is_empty(), "{:?}", h.ops);
    }

    #[test]
    fn a_row_error_is_drawn_on_screen() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.view.row_errors.insert("a".into(), "task failed: gone".into());
        h.frame(Vec::new());
        let rect = h.text("task failed: gone");
        assert!(rect.left() < 800.0, "{rect:?}");
    }

    #[test]
    fn a_failed_add_outlives_the_reload_after_it() {
        let mut h = Harness::new(Vec::new());
        let refused = TaskError::Failed { program: "task".into(), stderr: "no".into() };
        h.view.finish_write(None, Err(refused));
        let epoch = h.view.reload_epoch();
        h.view.finish_reload(epoch, &h.view.scope.clone(), Ok(Vec::new()));
        h.frame(Vec::new());
        h.text("task failed: no");
    }

    #[test]
    fn a_new_task_shows_until_the_reload_lands() {
        let mut h = Harness::new(Vec::new());
        let rect = h.text("+ add a task");
        h.click(rect.center());
        h.frame(vec![Event::Text("milk".into())]);
        h.key(Key::Enter);
        assert!(matches!(h.ops.as_slice(), [(None, edits)] if edits.len() == 1), "{:?}", h.ops);

        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] milk"]);
        let grips = h.texts.iter().filter(|(t, _)| t == "⠿").count();
        assert_eq!(grips, 1, "the new row has its grip before the store answers");
    }

    fn added(h: &Harness) -> Vec<(&str, i64)> {
        fn add(e: &Edit) -> Option<(&str, i64)> {
            match e {
                Edit::Add { description, order, .. } => Some((description.as_str(), *order)),
                _ => None,
            }
        }
        h.ops.iter().flat_map(|(_, edits)| edits).filter_map(add).collect()
    }

    #[test]
    fn pasted_lines_become_one_task_each() {
        let mut h = Harness::new(Vec::new());
        let rect = h.text("+ add a task");
        h.click(rect.center());
        h.frame(vec![Event::Paste("milk\r\neggs\n\n  bread \n".into())]);
        h.key(Key::Enter);
        assert_eq!(h.ops.len(), 1, "one write for the whole paste: {:?}", h.ops);
        let names: Vec<_> = added(&h).into_iter().map(|(d, _)| d).collect();
        assert_eq!(names, ["milk", "eggs", "bread"]);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] milk", "- [ ] eggs", "- [ ] bread"]);
    }

    #[test]
    fn pasted_lines_land_in_order_between_rows() {
        let mut h = two_rows();
        h.edit_end("one");
        h.key(Key::Enter);
        h.frame(vec![Event::Paste("x\ny".into())]);
        h.key(Key::Enter);
        assert_eq!(h.view.plain_lines(), [
            "## global",
            "- [ ] one",
            "- [ ] x",
            "- [ ] y",
            "- [ ] two"
        ]);
        let orders: Vec<_> = added(&h).into_iter().map(|(_, o)| o).collect();
        assert!(orders.windows(2).all(|w| w[0] < w[1]), "{:?}", h.ops);
        assert!(orders.iter().all(|&o| o < 2048), "{:?}", h.ops);
    }

    #[test]
    fn enter_in_a_row_opens_a_new_row_below_it() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.edit_end("one");
        h.key(Key::Enter);
        h.frame(vec![Event::Text("two".into())]);
        h.key(Key::Enter);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] one", "- [ ] two"]);
        assert!(!h.edits().iter().any(|e| matches!(e, Edit::Describe { .. })), "{:?}", h.ops);
    }

    fn two_rows() -> Harness {
        let mut b = pending("b", "two");
        b.order = Some(2048);
        Harness::new(vec![pending("a", "one"), b])
    }

    #[test]
    fn down_on_the_last_line_moves_to_the_next_row_at_the_same_column() {
        let mut h = two_rows();
        h.edit_end("one");
        h.key(Key::ArrowDown);
        assert_eq!(h.view.current.as_deref(), Some("b"));
        h.frame(vec![Event::Text("!".into())]);
        assert_eq!(h.view.drafts.get("b").map(String::as_str), Some("two!"));
    }

    #[test]
    fn up_on_the_first_line_moves_to_the_row_above() {
        let mut h = two_rows();
        h.edit_end("two");
        h.key(Key::ArrowUp);
        assert_eq!(h.view.current.as_deref(), Some("a"));
    }

    #[test]
    fn up_on_the_top_row_stays_put() {
        let mut h = two_rows();
        h.edit_end("one");
        h.key(Key::ArrowUp);
        assert_eq!(h.view.current.as_deref(), Some("a"));
    }

    #[test]
    fn shift_down_selects_instead_of_leaving_the_row() {
        let mut h = two_rows();
        h.edit_end("one");
        h.key_with(Key::ArrowDown, Modifiers::SHIFT);
        assert_eq!(h.view.current.as_deref(), Some("a"));
    }

    #[test]
    fn down_inside_a_wrapped_row_moves_within_it() {
        let long = "word ".repeat(60).trim_end().to_string();
        let mut h = Harness::new(vec![pending("a", &long), Task {
            order: Some(2048),
            ..pending("b", "two")
        }]);
        let rect = h.text(&long);
        h.click(Pos2::new(rect.left() + 2.0, rect.top() + 3.0));
        h.key(Key::ArrowDown);
        assert_eq!(h.view.current.as_deref(), Some("a"));
    }

    #[test]
    fn a_long_task_wraps_beside_its_checkbox() {
        let long = "word ".repeat(60).trim_end().to_string();
        let h = Harness::new(vec![pending("a", &long)]);
        let rect = h.text(&long);
        let line = h.text("+ add a task").height();
        assert!(rect.height() > 2.0 * line, "{rect:?}");
        assert!(rect.right() <= 800.0, "{rect:?}");
    }

    #[test]
    fn a_long_new_task_wraps_while_typed() {
        let long = "word ".repeat(60).trim_end().to_string();
        let mut h = Harness::new(Vec::new());
        let add = h.text("+ add a task");
        h.click(add.center());
        h.frame(vec![Event::Text(long.clone())]);
        h.frame(Vec::new());
        let rect = h.text(&long);
        assert!(rect.height() > 2.0 * add.height(), "{rect:?}");
        assert!(rect.right() <= 800.0, "{rect:?}");
        h.key(Key::Enter);
        assert_eq!(h.view.plain_lines(), ["## global", format!("- [ ] {long}").as_str()]);
    }

    #[test]
    fn a_parent_shows_how_many_subtasks_are_done() {
        let mut done = child("a1", "first", "a");
        done.status = Status::Completed;
        let h = Harness::new(vec![pending("a", "big"), done, child("a2", "second", "a")]);
        h.text("(1/2)");
    }

    #[test]
    fn hiding_completed_leaves_them_out_and_is_remembered() {
        let mut finished = pending("b", "finished");
        finished.status = Status::Completed;
        let mut h = Harness::new(vec![pending("a", "open"), finished]);
        h.click(h.text("hide completed").center());
        assert!(h.visible("finished").is_none());
        h.text("open");
        assert_eq!(h.view.take_pref_changes(), [PrefChange::HideCompleted(true)]);
    }

    #[test]
    fn folding_a_section_is_remembered() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.click(h.text("global  0/1").center());
        assert!(h.visible("one").is_none());
        assert_eq!(h.view.take_pref_changes(), [PrefChange::Collapsed {
            node: GLOBAL.into(),
            collapsed: true
        }]);
    }

    #[test]
    fn a_task_with_subtasks_is_deleted_only_once_confirmed() {
        let mut h = Harness::new(vec![pending("a", "big"), child("a1", "small", "a")]);
        h.view.act_on("a", RowAction::Delete);
        // A new modal spends its first frame sizing itself, drawn invisible.
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!h.deleted());
        h.click(h.text("Delete").center());
        assert_eq!(h.edits(), [&Edit::Delete("a1".into()), &Edit::Delete("a".into())]);
        assert_eq!(h.view.plain_lines(), ["## global"]);
    }

    #[test]
    fn a_cancelled_delete_keeps_the_task() {
        let mut h = Harness::new(vec![pending("a", "big"), child("a1", "small", "a")]);
        h.view.act_on("a", RowAction::Delete);
        h.frame(Vec::new());
        h.frame(Vec::new());
        h.click(h.text("Cancel").center());
        assert!(!h.deleted());
        assert!(h.view.confirm.is_none());
    }

    #[test]
    fn the_row_menu_deletes_a_task() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.press(h.text("one").center(), PointerButton::Secondary);
        h.click(h.text("Delete").center());
        assert_eq!(h.edits(), [&Edit::Delete("a".into())]);
    }

    /// A delete from a row menu lands while later rows of the same frame are
    /// still drawn from the indexes taken at the start. Those indexes stay
    /// valid, and the next frame shows the row gone.
    #[test]
    fn a_delete_during_drawing_keeps_later_row_indexes_valid() {
        let task = |id: &str, node: &str| {
            let mut task = pending(id, id);
            task.project = Some(node.into());
            task
        };
        let mut h = Harness::with_view(in_workspace(vec![
            task("keep", "r.feat"),
            task("gone", "r.feat.claude-1"),
            task("after", "r.feat.claude-2"),
        ]));
        h.press(h.text("gone").center(), PointerButton::Secondary);
        let delete = h.text("Delete").center();
        set_drawn_row_check(|view, index, row| {
            let held = view
                .sections()
                .get(index)
                .is_some_and(|built| built.section.rows.iter().any(|r| r.id == row.id));
            if !held {
                STALE_INDEX.with(|slot| *slot.borrow_mut() = Some((index, row.id.clone())));
            }
        });
        h.click(delete);
        clear_drawn_row_check();
        assert_eq!(stale_section_index(), None, "a drawn row's section index went stale");
        assert!(h.visible("gone").is_none(), "the next frame shows the row gone");
        h.text("keep");
        h.text("after");

        let built = h.view.builds();
        h.view.delete(&[]);
        h.view.delete(&["gone".into(), "  ".into()]);
        assert_eq!(h.view.builds(), built, "a blank or repeated delete does not rebuild");
        h.frame(Vec::new());
        assert_eq!(h.view.plain_lines().iter().filter(|l| l.contains("gone")).count(), 0);
    }

    fn three_selectable_rows() -> Harness {
        Harness::new(vec![
            pending("a", "one"),
            Task { order: Some(2048), ..pending("b", "two") },
            Task { order: Some(3072), ..pending("c", "three") },
        ])
    }

    #[test]
    fn ctrl_clicked_rows_are_deleted_together() {
        let mut h = three_selectable_rows();
        h.click_with(h.text("one").center(), Modifiers::COMMAND);
        h.click_with(h.text("three").center(), Modifiers::COMMAND);
        h.view.act(RowAction::Delete);
        h.frame(Vec::new());
        assert_eq!(h.ops, [(Some("a".to_string()), vec![
            Edit::Delete("a".into()),
            Edit::Delete("c".into())
        ])]);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] two"]);
    }

    #[test]
    fn ctrl_clicking_a_selected_row_leaves_it_out() {
        let mut h = three_selectable_rows();
        h.click_with(h.text("one").center(), Modifiers::COMMAND);
        h.click_with(h.text("two").center(), Modifiers::COMMAND);
        h.click_with(h.text("one").center(), Modifiers::COMMAND);
        h.view.act(RowAction::Delete);
        h.frame(Vec::new());
        assert_eq!(h.edits(), [&Edit::Delete("b".into())]);
    }

    #[test]
    fn shift_click_selects_every_row_from_the_edited_one() {
        let mut h = three_selectable_rows();
        h.edit_end("one");
        h.click_with(h.text("three").center(), Modifiers::SHIFT);
        h.view.act(RowAction::Delete);
        h.frame(Vec::new());
        assert_eq!(h.edits(), [
            &Edit::Delete("a".into()),
            &Edit::Delete("b".into()),
            &Edit::Delete("c".into())
        ]);
    }

    #[test]
    fn a_selected_row_is_highlighted() {
        let mut h = three_selectable_rows();
        h.click_with(h.text("two").center(), Modifiers::COMMAND);
        let lit = |text: &str| {
            let at = h.text(text).center();
            h.fills.iter().any(|(c, r)| *c == h.style.selection && r.contains(at))
        };
        assert!(lit("two"));
        assert!(!lit("one"));
    }

    #[test]
    fn editing_a_row_drops_the_selection() {
        let mut h = three_selectable_rows();
        h.click_with(h.text("one").center(), Modifiers::COMMAND);
        h.edit_end("two");
        h.view.act(RowAction::Delete);
        h.frame(Vec::new());
        assert_eq!(h.edits(), [&Edit::Delete("b".into())], "the edited row, not the selection");
    }

    #[test]
    fn the_row_menu_deletes_every_selected_row() {
        let mut h = three_selectable_rows();
        h.click_with(h.text("one").center(), Modifiers::COMMAND);
        h.click_with(h.text("two").center(), Modifiers::COMMAND);
        h.press(h.text("two").center(), PointerButton::Secondary);
        h.click(h.text("Delete 2 tasks").center());
        assert_eq!(h.edits(), [&Edit::Delete("a".into()), &Edit::Delete("b".into())]);
    }

    #[test]
    fn a_selection_that_takes_subtasks_waits_for_a_yes() {
        let mut h = Harness::new(vec![pending("a", "big"), child("a1", "small", "a"), Task {
            order: Some(2048),
            ..pending("b", "other")
        }]);
        h.click_with(h.text("big").center(), Modifiers::COMMAND);
        h.click_with(h.text("other").center(), Modifiers::COMMAND);
        h.view.act(RowAction::Delete);
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!h.deleted());
        h.text("Delete 2 tasks and their 1 subtask?");
        h.click(h.text("Delete").center());
        assert_eq!(h.edits(), [
            &Edit::Delete("a1".into()),
            &Edit::Delete("a".into()),
            &Edit::Delete("b".into())
        ]);
    }

    #[test]
    fn a_move_shows_before_the_store_confirms_it() {
        let mut b = pending("b", "two");
        b.order = Some(2048);
        let mut h = Harness::new(vec![pending("a", "one"), b]);
        h.view.act_on("a", RowAction::MoveDown);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] two", "- [ ] one"]);
        h.view.act_on("a", RowAction::MoveUp);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] one", "- [ ] two"]);
    }

    #[test]
    fn dragging_a_row_above_another_moves_it_there() {
        let mut b = pending("b", "two");
        b.order = Some(2048);
        let mut h = Harness::new(vec![pending("a", "one"), b]);
        let grip = h.text("⠿").center();
        let from = Pos2::new(grip.x, h.text("two").center().y);
        let to = Pos2::new(400.0, h.text("one").top() + 1.0);
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        h.frame(vec![Event::PointerMoved(from), button(from, true)]);
        h.frame(vec![Event::PointerMoved(from + Vec2::new(0.0, -8.0))]);
        h.frame(vec![Event::PointerMoved(to)]);
        h.frame(vec![button(to, false)]);
        assert_eq!(h.view.plain_lines(), ["## global", "- [ ] two", "- [ ] one"]);
    }

    #[test]
    fn the_tab_opens_on_the_cached_listing() {
        let dir = tempfile::tempdir().unwrap();
        let scope = Scope::for_workspace(None, None);
        write_cache(&scope.cache_file(dir.path()), &[pending("a", "cached")]);
        let view = view_with(Vec::new(), Some(dir.path().to_path_buf()));
        assert_eq!(view.plain_lines(), ["## global", "- [ ] cached"]);
    }

    #[test]
    fn separate_repos_and_sides_cache_apart() {
        let dir = Path::new("cache");
        let file = |side, repo: Option<&str>| {
            Scope { side, repo: repo.map(Into::into), workspace: None }.cache_file(dir)
        };
        assert_ne!(file(Side::Native, Some("r")), file(Side::Native, Some("s")));
        assert_ne!(file(Side::Native, Some("r")), file(Side::Wsl("Ubuntu".into()), Some("r")));
        assert_eq!(file(Side::Native, Some("a/b")), dir.join("native-a_b.json"));
    }

    /// Each workspace opens on its own file, so one worktree's listing does
    /// not replace the other's.
    #[test]
    fn two_workspaces_of_one_repository_keep_separate_cached_listings() {
        let dir = tempfile::tempdir().unwrap();
        let feat = workspace_scope("r.feat");
        let main = workspace_scope("r.main");
        write_cache(&feat.cache_file(dir.path()), &[pending("feat", "feat task")]);
        write_cache(&main.cache_file(dir.path()), &[pending("main", "main task")]);
        // A blank listing and a file the tab cannot read leave that workspace
        // empty instead of taking the sibling's listing.
        write_cache(&workspace_scope("r.blank").cache_file(dir.path()), &[]);
        std::fs::write(workspace_scope("r.bad").cache_file(dir.path()), b"not json").unwrap();

        let opened = |scope: Scope| {
            let backend = Backend::Fake(FakeBackend::default());
            TasksView::new(
                backend,
                scope,
                None,
                Vec::new(),
                Prefs::default(),
                Some(dir.path().to_path_buf()),
            )
        };
        assert!(opened(feat.clone()).plain_lines().contains(&"- [ ] feat task".to_string()));
        assert!(!opened(feat.clone()).plain_lines().contains(&"- [ ] main task".to_string()));
        assert!(opened(main.clone()).plain_lines().contains(&"- [ ] main task".to_string()));
        assert!(!opened(main.clone()).plain_lines().contains(&"- [ ] feat task".to_string()));
        assert_ne!(feat.cache_file(dir.path()), main.cache_file(dir.path()));
        let blank = opened(workspace_scope("r.blank"));
        let bad = opened(workspace_scope("r.bad"));
        assert!(!blank.plain_lines().iter().any(|line| line.starts_with("- [")));
        assert!(!bad.plain_lines().iter().any(|line| line.starts_with("- [")));
    }

    #[test]
    fn collapsing_a_task_hides_its_subtasks_until_expanded() {
        let mut h = Harness::new(vec![
            pending("a", "parent"),
            child("b", "child", "a"),
            child("c", "grandchild", "b"),
        ]);
        let toggle = h.chevron("parent");
        h.click(toggle);
        assert_eq!(h.visible("child"), None);
        assert_eq!(h.visible("grandchild"), None);
        h.text("+2");
        assert_eq!(h.view.plain_lines().len(), 4, "agents still read every task");

        h.click(toggle);
        h.text("child");
        h.text("grandchild");
        assert_eq!(h.visible("+2"), None);
    }

    #[test]
    fn indenting_under_a_collapsed_task_expands_it() {
        let mut h = Harness::new(vec![pending("a", "parent"), child("a1", "child", "a"), Task {
            order: Some(2048),
            ..pending("b", "next")
        }]);
        h.click(h.chevron("parent"));
        h.edit_end("next");
        h.view.act(RowAction::Indent);
        h.frame(Vec::new());
        h.text("child");
    }

    #[test]
    fn a_leaf_lines_up_with_a_parent_and_has_no_toggle() {
        let mut h = Harness::new(vec![pending("a", "parent"), child("a1", "child", "a"), Task {
            order: Some(2048),
            ..pending("b", "alone")
        }]);
        assert_eq!(h.text("parent").left(), h.text("alone").left());
        h.click(h.chevron("alone"));
        assert!(h.view.collapsed_tasks.is_empty());
    }

    #[test]
    fn a_started_task_sits_on_the_active_background() {
        let mut idle = pending("b", "idle");
        idle.order = Some(2048);
        let h = Harness::new(vec![Task { started: true, ..pending("a", "busy") }, idle]);
        let filled = |text: &str| {
            let at = h.text(text).center();
            h.fills.iter().any(|(c, r)| *c == h.style.active_background && r.contains(at))
        };
        assert!(filled("busy"));
        assert!(!filled("idle"));
    }

    #[test]
    fn the_toggle_answers_across_a_square_the_row_tall() {
        let mut h = Harness::new(vec![pending("a", "parent"), child("a1", "child", "a")]);
        // egui otherwise counts a click near a widget as on it, which would
        // hide how far the toggle's own rect reaches.
        h.ctx.style_mut(|s| s.interaction.interact_radius = 0.0);
        let reach = h.ctx.style().spacing.interact_size.y / 2.0 - 1.0;
        h.click(h.painted_chevron("parent") - vec2(reach, reach));
        assert_eq!(h.visible("child"), None);
    }

    #[test]
    fn a_section_folds_from_its_chevron_and_its_label() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        h.click(h.painted_chevron("global  0/1"));
        assert_eq!(h.visible("one"), None);
        h.click(h.text("global  0/1").center());
        h.text("one");
    }

    #[test]
    fn a_section_heading_highlights_and_folds_as_one_target() {
        let mut h = Harness::new(vec![pending("a", "one")]);
        let (chevron, label) = (h.painted_chevron("global  0/1"), h.text("global  0/1"));
        let gap = Pos2::new((chevron.x + label.left()) / 2.0, label.center().y);
        h.frame(vec![Event::PointerMoved(gap)]);
        let hover = h.ctx.style().visuals.widgets.hovered.weak_bg_fill;
        let lit = |at: Pos2| h.fills.iter().any(|(c, r)| *c == hover && r.contains(at));
        assert!(lit(chevron) && lit(gap) && lit(label.center()));

        h.click(gap);
        assert_eq!(h.visible("one"), None);
    }
}
