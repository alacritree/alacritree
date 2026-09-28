//! Runs the real task editor against a private list for the UI catalog.
//! Drawing skips `tick`, and queued edits settle in memory: no backend,
//! repository discovery, cache, or persisted preferences are touched.

use super::*;

pub(crate) struct Preview {
    view: TasksView,
    next_id: usize,
}

impl Preview {
    pub(crate) fn new(tasks: Vec<Task>, scope: Scope) -> Self {
        let mut view = TasksView::new(
            Backend::from_config(&Default::default()),
            scope,
            None,
            Vec::new(),
            Prefs::default(),
            None,
        );
        view.tasks = tasks;
        view.loaded = true;
        Self { view, next_id: 0 }
    }

    pub(crate) fn view(&mut self) -> &mut TasksView {
        &mut self.view
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, shortcuts: &Shortcuts, style: Style) {
        draw(ui, &mut self.view, true, shortcuts, style);
        self.settle();
    }

    fn settle(&mut self) {
        let pending = std::mem::take(&mut self.view.outbox);
        if pending.is_empty() {
            return;
        }
        let mut tasks = self.view.tasks.clone();
        for (row, edits) in pending {
            for edit in edits {
                match edit {
                    Edit::Add { project, description, parent, order } => {
                        // Prefixes separate newly added rows from fixture IDs,
                        // including duplicate descriptions.
                        self.next_id += 1;
                        tasks.push(Task {
                            id: format!("catalog-added-{}", self.next_id),
                            project: Some(project),
                            description,
                            parent,
                            order: Some(order),
                            status: Status::Pending,
                            started: false,
                            entry: None,
                            modified: None,
                        });
                    },
                    Edit::Delete(id) => tasks.retain(|task| task.id != id),
                    Edit::Move { .. } | Edit::Reorder { .. } => tree::replay(&mut tasks, &edit),
                    Edit::Describe { id, description } => {
                        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
                            task.description = description;
                        }
                    },
                    Edit::Done(ref id) | Edit::Undone(ref id) => {
                        if let Some(task) = tasks.iter_mut().find(|task| &task.id == id) {
                            task.status = if matches!(edit, Edit::Done(_)) {
                                Status::Completed
                            } else {
                                Status::Pending
                            };
                        }
                    },
                    Edit::Start(ref id) | Edit::Stop(ref id) => {
                        if let Some(task) = tasks.iter_mut().find(|task| &task.id == id) {
                            task.started = matches!(edit, Edit::Start(_));
                        }
                    },
                }
            }
            self.view.finish_write(row, Ok(()));
        }
        self.view.finish_reload(self.view.reload_epoch(), Ok(tasks));
        self.view.pref_changes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_edits_settle_locally_including_duplicate_descriptions() {
        let mut preview = Preview::new(Vec::new(), Scope::for_workspace(None, None));
        for _ in 0..2 {
            preview.view.write(
                None,
                vec![Edit::Add {
                    project: "global".into(),
                    description: "Same name".into(),
                    parent: None,
                    order: 100,
                }],
            );
        }
        preview.settle();
        assert_eq!(preview.view.tasks.len(), 2);
        let first = preview.view.tasks[0].id.clone();
        let second = preview.view.tasks[1].id.clone();
        assert_ne!(first, second);
        preview.view.write(
            None,
            vec![
                Edit::Describe { id: first.clone(), description: "Edited".into() },
                Edit::Move { id: second.clone(), parent: Some(first.clone()), order: 200 },
                Edit::Done(first.clone()),
                Edit::Start(second.clone()),
            ],
        );
        preview.settle();
        assert_eq!(preview.view.tasks[0].description, "Edited");
        assert_eq!(preview.view.tasks[0].status, Status::Completed);
        assert_eq!(preview.view.tasks[1].parent.as_deref(), Some(first.as_str()));
        assert!(preview.view.tasks[1].started);
        preview.view.write(None, vec![Edit::Delete(second)]);
        preview.settle();
        assert_eq!(preview.view.tasks.len(), 1);
        assert!(preview.view.reload.is_none());
        assert!(preview.view.writes.is_empty());
        assert!(preview.view.cache_write.is_none());
    }
}
