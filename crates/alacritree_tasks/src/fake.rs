//! A store held in memory that records every edit, for tests of code that
//! drives a backend. Clones share one store, so a test keeps a clone to read
//! after handing the backend away.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use alacritree_common::jobs::Blocking;
use alacritree_common::side::Side;

use crate::{Edit, Filter, Status, Task, TaskBackend, TaskError};

#[derive(Debug, Clone, Default)]
pub struct FakeBackend {
    store: Arc<Mutex<Store>>,
}

#[derive(Debug)]
struct Store {
    tasks: Vec<Task>,
    edits: Vec<Edit>,
    stamped: bool,
    stamp: u64,
    listings: u64,
    /// Edits already applied when each listing started.
    listed_after: Vec<usize>,
    hold: Option<Arc<(Mutex<bool>, Condvar)>>,
    refusing: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            tasks: Vec::new(),
            edits: Vec::new(),
            stamped: true,
            stamp: 0,
            listings: 0,
            listed_after: Vec::new(),
            hold: None,
            refusing: false,
        }
    }
}

impl FakeBackend {
    pub fn with_tasks(tasks: Vec<Task>) -> Self {
        let backend = Self::default();
        backend.store().tasks = tasks;
        backend
    }

    /// Holds the next apply until release, so a test can see the tick that spawned it.
    pub fn holding(self) -> Held {
        let gate = Arc::new((Mutex::new(true), Condvar::new()));
        self.store().hold = Some(Arc::clone(&gate));
        Held { store: self, gate }
    }

    /// Fails every edit, as a store that is down would.
    pub fn refusing(self) -> Self {
        self.store().refusing = true;
        self
    }

    pub fn tasks(&self) -> Vec<Task> {
        self.store().tasks.clone()
    }

    /// Every edit asked for so far, refused ones included, in order.
    pub fn edits(&self) -> Vec<Edit> {
        self.store().edits.clone()
    }

    /// A replica whose files the tab cannot find.
    pub fn unstamped(self) -> Self {
        self.store().stamped = false;
        self
    }

    pub fn has_stamp(&self) -> bool {
        self.store().stamped
    }

    /// The replica stamp the tab reads before a reload.
    pub fn stamp(&self) -> u64 {
        self.store().stamp
    }

    /// A write by another process: the stamp moves and the tasks change.
    pub fn written_elsewhere(&self, tasks: Vec<Task>) {
        let mut store = self.store();
        store.stamp += 1;
        store.tasks = tasks;
    }

    /// How many times the tab has listed this store.
    pub fn listings(&self) -> u64 {
        self.store().listings
    }

    /// How many edits were already applied when each listing started.
    pub fn listed_after(&self) -> Vec<usize> {
        self.store().listed_after.clone()
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl TaskBackend for FakeBackend {
    fn list(&self, _: &Side, filter: &Filter, _: &Blocking) -> Result<Vec<Task>, TaskError> {
        let mut store = self.store();
        store.listings += 1;
        let applied = store.edits.len();
        store.listed_after.push(applied);
        Ok(store.tasks.iter().filter(|t| filter.matches(t)).cloned().collect())
    }

    fn apply(&self, _: &Side, edit: &Edit, _: &Blocking) -> Result<(), TaskError> {
        if let Some(gate) = self.store().hold.clone() {
            drop(self.store());
            wait_out(&gate);
        }
        let mut store = self.store();
        store.edits.push(edit.clone());
        if store.refusing {
            return Err(TaskError::Failed { program: "fake".into(), stderr: "refused".into() });
        }
        apply(&mut store.tasks, edit);
        Ok(())
    }

    fn agent_guide(&self, project: &str) -> String {
        format!("Write your tasks to `{project}`.\n")
    }
}

/// A held apply. Dropping it releases the apply.
pub struct Held {
    store: FakeBackend,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Held {
    pub fn store(&self) -> &FakeBackend {
        &self.store
    }

    pub fn release(&self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = false;
        cv.notify_all();
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.release();
    }
}

fn wait_out(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &**gate;
    let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
    while *held {
        held = cv.wait(held).unwrap_or_else(|e| e.into_inner());
    }
}

/// A pending task on `project` whose description is its id.
pub fn task(id: &str, project: &str) -> Task {
    Task {
        id: id.into(),
        description: id.into(),
        status: Status::Pending,
        started: false,
        parent: None,
        order: None,
        project: Some(project.into()),
        entry: None,
        modified: None,
    }
}

/// `tasks` as a store holds them after `edit`. A new task takes its
/// description as its id, so a test can name it.
pub fn apply(tasks: &mut Vec<Task>, edit: &Edit) {
    fn find<'a>(tasks: &'a mut [Task], id: &str) -> Option<&'a mut Task> {
        tasks.iter_mut().find(|t| t.id == id)
    }
    match edit {
        Edit::Add { project, description, parent, order } => {
            let mut t = task(description, project);
            t.parent = parent.clone();
            t.order = Some(*order);
            tasks.push(t);
        },
        Edit::Move { id, parent, order } => {
            if let Some(t) = find(tasks, id) {
                t.parent = parent.clone();
                t.order = Some(*order);
            }
        },
        Edit::Reorder { id, order } => {
            find(tasks, id).into_iter().for_each(|t| t.order = Some(*order))
        },
        Edit::Describe { id, description } => {
            find(tasks, id).into_iter().for_each(|t| t.description = description.clone());
        },
        Edit::Done(id) => find(tasks, id).into_iter().for_each(|t| t.status = Status::Completed),
        Edit::Undone(id) => find(tasks, id).into_iter().for_each(|t| t.status = Status::Pending),
        Edit::Start(id) => find(tasks, id).into_iter().for_each(|t| t.started = true),
        Edit::Stop(id) => find(tasks, id).into_iter().for_each(|t| t.started = false),
        Edit::Delete(id) => tasks.retain(|t| &t.id != id),
    }
}
