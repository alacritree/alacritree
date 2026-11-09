//! The labels that tie a core terminal to its place in alacritree.
//!
//! Every label alacritree writes is `alacritree.*`, plus the `where` label
//! that `hc ls` and the `hc` picker show. A `herdr.*` label would make
//! herdr-app adopt the terminal, or kill it, so none is ever written. A
//! terminal is alacritree's when it carries [`WORKSPACE`], which is also how
//! an agent puts a terminal into a workspace:
//! `hc run --keep --label alacritree.workspace=<path>`.

use std::collections::HashMap;
use std::path::PathBuf;

use alacritree_session_backend::Placement;
use serde_json::{Map, Value};

/// The checkout the terminal's session belongs to, or [`HOME`].
pub(crate) const WORKSPACE: &str = "alacritree.workspace";
/// The session's index among its workspace's sessions.
pub(crate) const POSITION: &str = "alacritree.position";
/// How `hc ls` names the terminal's place, as `<project> / <checkout>`.
pub(crate) const WHERE: &str = "where";
/// The [`WORKSPACE`] value of the home tab.
pub(crate) const HOME: &str = "~";
/// The herdr-app space that holds the terminal. herdr-app sets it, and an
/// empty value means the terminal is parked in no space. alacritree only
/// reads it.
pub(crate) const HERDR_SPACE: &str = "herdr.space";

/// The labels that record `placement`. A placement with no title clears
/// `where`, since the core removes a label set to the empty string.
pub(crate) fn of(placement: &Placement) -> Map<String, Value> {
    let workspace = placement
        .workspace
        .as_ref()
        .map_or_else(|| HOME.to_string(), |path| path.display().to_string());
    let mut labels = Map::new();
    labels.insert(WORKSPACE.into(), workspace.into());
    labels.insert(POSITION.into(), placement.position.to_string().into());
    labels.insert(WHERE.into(), placement.title.clone().unwrap_or_default().into());
    labels
}

/// The labels a new terminal starts with, leaving out what is empty.
pub(crate) fn for_run(placement: &Placement) -> Map<String, Value> {
    let mut labels = of(placement);
    labels.retain(|_, value| value.as_str().is_some_and(|value| !value.is_empty()));
    labels
}

/// The placement a terminal's labels record, if it is alacritree's. A
/// missing or unreadable position puts it after its workspace's sessions.
pub(crate) fn placement(labels: &HashMap<String, String>) -> Option<Placement> {
    let workspace = labels.get(WORKSPACE)?;
    Some(Placement {
        workspace: (workspace != HOME).then(|| PathBuf::from(workspace)),
        position: labels.get(POSITION).and_then(|p| p.parse().ok()).unwrap_or(usize::MAX),
        title: labels.get(WHERE).cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back(labels: Map<String, Value>) -> Option<Placement> {
        let labels = labels
            .into_iter()
            .map(|(key, value)| (key, value.as_str().unwrap_or_default().to_string()))
            .collect();
        placement(&labels)
    }

    #[test]
    fn a_placement_reads_back_from_its_labels() {
        let placement = Placement {
            workspace: Some(PathBuf::from("/src/app-feature")),
            position: 2,
            title: Some("app / feature".into()),
        };

        assert_eq!(read_back(for_run(&placement)), Some(placement));
    }

    #[test]
    fn the_home_tab_has_a_workspace_label_and_no_where() {
        let labels = for_run(&Placement::default());

        assert_eq!(labels[WORKSPACE], HOME);
        assert!(!labels.contains_key(WHERE));
        assert!(labels.keys().all(|key| !key.starts_with("herdr.")));
        assert_eq!(read_back(labels).map(|p| p.workspace), Some(None));
    }

    /// An agent that names only the workspace still gets its terminal
    /// shown, after the sessions already there.
    #[test]
    fn a_workspace_label_alone_places_the_terminal_last() {
        let labels = HashMap::from([(WORKSPACE.to_string(), "/src/app".to_string())]);

        let placement = placement(&labels).expect("a workspace label makes it alacritree's");

        assert_eq!(placement.workspace, Some(PathBuf::from("/src/app")));
        assert_eq!(placement.position, usize::MAX);
    }

    #[test]
    fn a_terminal_without_a_workspace_label_is_not_alacritrees() {
        let labels = HashMap::from([(WHERE.to_string(), "app / main".to_string())]);

        assert_eq!(placement(&labels), None);
    }
}
