//! The labels that tie a core terminal to its place in alacritree, and to
//! its place in herdr-app.
//!
//! alacritree writes its own `alacritree.*` labels, plus the `where` label
//! that `hc ls` and the `hc` picker show. It writes `herdr.*` labels only on
//! its own terminals, and only to show them in herdr-app or to take them out
//! again. herdr-app adopts a terminal that has a `herdr.space`, and closing
//! its tab there kills the terminal. Each space and tab id that alacritree
//! makes starts with [`SHARED_PREFIX`], so alacritree takes out only the
//! terminals that it put in. A terminal is alacritree's when it carries
//! [`WORKSPACE`], which is also how an agent puts a terminal into a
//! workspace: `hc run --keep --label alacritree.workspace=<path>`.

use std::collections::HashMap;
use std::path::PathBuf;

use alacritree_common::digest::stable_digest;
use alacritree_session_backend::Placement;
use serde_json::{Map, Value};

/// The checkout the terminal's session belongs to, or [`HOME`].
pub(crate) const WORKSPACE: &str = "alacritree.workspace";
/// The session's index among its workspace's sessions.
pub(crate) const POSITION: &str = "alacritree.position";
/// The session's id, which the programs in the terminal carry in
/// `ALACRITREE_SESSION_ID`, so the next launch gives it back.
pub(crate) const SESSION: &str = "alacritree.session";
/// How `hc ls` names the terminal's place, as `<project> / <checkout>`.
pub(crate) const WHERE: &str = "where";
/// The [`WORKSPACE`] value of the home tab.
pub(crate) const HOME: &str = "~";
/// The herdr-app space that holds the terminal. herdr-app clears it to park
/// the terminal in no space.
pub(crate) const HERDR_SPACE: &str = "herdr.space";
/// The herdr-app tab that holds the terminal. A second terminal with the same
/// tab joins that tab as a split.
const HERDR_TAB: &str = "herdr.tab";
/// The last turn a person looked at, which herdr-app counts unread turns
/// from.
const HERDR_SEEN: &str = "herdr.seen";
/// The start of each herdr-app space and tab id that alacritree makes. It
/// differs from herdr-app's own ids, such as `sp1` and `tb1`.
const SHARED_PREFIX: &str = "at-";

/// The labels that record `placement`. A placement with no title or no
/// session id clears that label, since the core removes a label set to the
/// empty string.
pub(crate) fn of(placement: &Placement) -> Map<String, Value> {
    let workspace = placement
        .workspace
        .as_ref()
        .map_or_else(|| HOME.to_string(), |path| path.display().to_string());
    let mut labels = Map::new();
    labels.insert(WORKSPACE.into(), workspace.into());
    labels.insert(POSITION.into(), placement.position.to_string().into());
    labels.insert(WHERE.into(), placement.title.clone().unwrap_or_default().into());
    let session = placement.session_id.map(|id| id.to_string()).unwrap_or_default();
    labels.insert(SESSION.into(), session.into());
    labels
}

/// The labels a new terminal starts with, leaving out what is empty.
pub(crate) fn for_run(placement: &Placement) -> Map<String, Value> {
    let mut labels = of(placement);
    labels.retain(|_, value| value.as_str().is_some_and(|value| !value.is_empty()));
    labels
}

/// The labels that show the terminal at `address` in herdr-app, or take it
/// out, as `share` says. `None` when its labels already agree, and for a
/// terminal that is not alacritree's.
///
/// A shared terminal goes to the space of its checkout, and to a tab of its
/// own. A terminal that herdr-app parked has no space, so it goes back. A
/// space that herdr-app made stays as it is.
pub(crate) fn to_share(
    address: &str,
    labels: &HashMap<String, String>,
    share: bool,
) -> Option<Map<String, Value>> {
    let workspace = labels.get(WORKSPACE)?;
    let space = labels.get(HERDR_SPACE).filter(|space| !space.is_empty());
    let mut changed = Map::new();
    match space {
        None if share => {
            changed.insert(HERDR_SPACE.into(), space_id(workspace).into());
            changed.insert(HERDR_TAB.into(), format!("{SHARED_PREFIX}{address}").into());
            if !labels.contains_key(HERDR_SEEN) {
                changed.insert(HERDR_SEEN.into(), "0".into());
            }
        },
        Some(space) if !share && space.starts_with(SHARED_PREFIX) => {
            changed.insert(HERDR_SPACE.into(), "".into());
            changed.insert(HERDR_TAB.into(), "".into());
        },
        _ => return None,
    }
    Some(changed)
}

/// The herdr-app space of `workspace`, a [`WORKSPACE`] value. A checkout has
/// the same id on every run and every platform.
fn space_id(workspace: &str) -> String {
    if workspace == HOME {
        return format!("{SHARED_PREFIX}home");
    }
    let digest = stable_digest(workspace.as_bytes()) & 0xffff_ffff_ffff;
    format!("{SHARED_PREFIX}{digest:012x}")
}

/// The placement a terminal's labels record, if it is alacritree's. A
/// missing or unreadable position puts it after its workspace's sessions.
pub(crate) fn placement(labels: &HashMap<String, String>) -> Option<Placement> {
    let workspace = labels.get(WORKSPACE)?;
    Some(Placement {
        workspace: (workspace != HOME).then(|| PathBuf::from(workspace)),
        position: labels.get(POSITION).and_then(|p| p.parse().ok()).unwrap_or(usize::MAX),
        title: labels.get(WHERE).cloned(),
        session_id: labels.get(SESSION).and_then(|id| id.parse().ok()),
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
            session_id: Some(43),
        };

        assert_eq!(read_back(for_run(&placement)), Some(placement.clone()));
        assert_eq!(read_back(of(&placement)), Some(placement));
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
