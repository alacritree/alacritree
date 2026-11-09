//! The core's terminals that alacritree did not create, listed as the panes
//! of a multiplexer.
//!
//! The listing reads the connection that sessions share, so a poll makes no
//! process call. A pane is a session backend's terminal: the app opens it
//! through [`crate::HerdrCore`], and its session never kills it on close.
//! Only a core that already serves is followed, since `hc connect` would
//! start one just to be listed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use alacritree_common::settings::IconStyle;
use alacritree_multiplexer::{
    AttachAnswer, AttachRequest, CreateAnswer, CreateRequest, Launch, ListedPane, Managed,
    MultiplexerKind, MultiplexerSession, Pane, PaneError, PaneKey, PaneTarget, SessionId, Side,
    ViewState, ViewStep,
};
use serde_json::{Value, json};

use crate::connection::{Facts, State};
use crate::{HerdrCore, HerdrCoreConfig, labels};

/// How long a look for a serving core that found none waits before the
/// next one.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(10);

/// Why a herdr-core pane was not created or attached the way another
/// multiplexer's is.
#[derive(Debug, thiserror::Error)]
pub enum PanesError {
    #[error(
        "herdr-core lists the terminals other clients started and does not create them; set \
         [integrations.herdr_core] session_backend = \"herdr_core\" to run new sessions in the \
         core"
    )]
    NoCreate,
    #[error("a herdr-core pane opens through the session backend, not an attach client")]
    NoAttachClient,
}

/// The native core's terminals that carry no `alacritree.workspace` label.
pub struct HerdrCorePanes {
    core: HerdrCore,
    config: HerdrCoreConfig,
    panes: Vec<Pane>,
    generation: u64,
    /// A look for a serving core is running.
    following: Arc<AtomicBool>,
    followed_at: Option<Instant>,
    refused_attaches: Vec<AttachAnswer>,
    refused_creates: Vec<CreateAnswer>,
    /// A test handed the listing its panes, so a poll keeps them.
    #[cfg(feature = "test-support")]
    fixed: bool,
}

impl HerdrCorePanes {
    /// `core` is the backend the app's herdr-core sessions run on, so the
    /// listing and the sessions share one connection.
    pub fn new(core: HerdrCore, config: HerdrCoreConfig) -> Self {
        Self {
            core,
            config,
            panes: Vec::new(),
            generation: 0,
            following: Default::default(),
            followed_at: None,
            refused_attaches: Vec::new(),
            refused_creates: Vec::new(),
            #[cfg(feature = "test-support")]
            fixed: false,
        }
    }

    /// List `panes` in place of what the core reports, from now on.
    #[cfg(feature = "test-support")]
    pub fn adopt_for_test(&mut self, panes: Vec<Pane>) {
        self.fixed = true;
        self.set_panes(panes);
    }

    fn set_panes(&mut self, panes: Vec<Pane>) {
        if panes != self.panes {
            self.panes = panes;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Open a connection on a thread when a core serves, at most once per
    /// [`FOLLOW_INTERVAL`], and wake the app once it is open.
    fn follow(&mut self) {
        let due = self.followed_at.is_none_or(|at| at.elapsed() >= FOLLOW_INTERVAL);
        if !due || self.following.swap(true, Ordering::AcqRel) {
            return;
        }
        self.followed_at = Some(Instant::now());
        let (core, following) = (self.core.core.clone(), self.following.clone());
        let spawned = std::thread::Builder::new().name("hc-follow".into()).spawn(move || {
            if core.serving() && core.connection().is_ok() {
                if let Some(wake) = core.waker().get() {
                    wake();
                }
            }
            following.store(false, Ordering::Release);
        });
        if let Err(error) = spawned {
            log::warn!("herdr-core follow thread: {error}");
            self.following.store(false, Ordering::Release);
        }
    }
}

/// The pane a terminal is, when it is live and not alacritree's own.
pub(crate) fn foreign_pane(address: &str, facts: &Facts) -> Option<Pane> {
    if facts.state != State::Live || labels::placement(&facts.labels).is_some() {
        return None;
    }
    let foreground = facts.foreground.as_ref();
    let title = facts
        .name
        .clone()
        .or_else(|| facts.labels.get(labels::WHERE).cloned())
        .or_else(|| facts.title.clone())
        .filter(|title| !title.trim().is_empty());
    let cwd = facts.cwd.as_ref().map(|cwd| cwd.display().to_string());
    // herdr-app keeps a terminal in its space however the shell moves, and
    // names a new space after the folder its first terminal started in.
    let in_herdr_space = facts.labels.get(labels::HERDR_SPACE).is_some_and(|s| !s.is_empty());
    Some(Pane {
        terminal_id: address.to_string(),
        pane_id: address.to_string(),
        tab_id: None,
        kind: facts.app.clone().or_else(|| foreground.map(|foreground| foreground.name.clone())),
        title,
        status: None,
        focused: false,
        workspace_cwd: cwd.clone().filter(|_| in_herdr_space),
        cwd,
        foreground_cwd: foreground.map(|foreground| foreground.cwd.display().to_string()),
    })
}

impl MultiplexerSession for HerdrCorePanes {
    fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn icon(&self) -> &IconStyle {
        &self.config.icon
    }

    /// The core's own event stream keeps the connection's facts current, so
    /// a poll only reads them. With no connection the listing is empty.
    fn poll(&mut self, _attached: &dyn Fn(&Side) -> bool) {
        #[cfg(feature = "test-support")]
        if self.fixed {
            return;
        }
        if !self.config.enabled {
            return self.set_panes(Vec::new());
        }
        let Some(connection) = self.core.core.live() else {
            self.follow();
            return self.set_panes(Vec::new());
        };
        let mut panes = connection.pick_terminals(foreign_pane);
        // Addresses count up from `t1`, so the oldest terminal lists first.
        panes.sort_by(|a, b| {
            (a.terminal_id.len(), &a.terminal_id).cmp(&(b.terminal_id.len(), &b.terminal_id))
        });
        self.set_panes(panes);
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn panes(&self) -> Vec<(&Side, &Pane)> {
        self.panes.iter().map(|pane| (&Side::Native, pane)).collect()
    }

    fn pane_index(&self, side: &Side, terminal_id: &str) -> Option<usize> {
        (*side == Side::Native)
            .then(|| self.panes.iter().position(|pane| pane.terminal_id == terminal_id))
            .flatten()
    }

    fn pane_count(&self) -> usize {
        self.panes.len()
    }

    fn find(&self, side: &Side, terminal_id: &str) -> Option<&Pane> {
        self.pane_index(side, terminal_id).map(|at| &self.panes[at])
    }

    fn retained(&self, side: &Side, terminal_id: &str) -> Option<(&Pane, bool)> {
        self.find(side, terminal_id).map(|pane| (pane, true))
    }

    /// A foreign terminal works anywhere, so one matching no workspace is
    /// listed under Home rather than hidden.
    fn listed(&self, claimed: &[PaneKey], workspaces: &[PathBuf]) -> Vec<ListedPane<'_>> {
        if !self.config.enabled {
            return Vec::new();
        }
        self.panes
            .iter()
            .map(|pane| (self.key(&pane.terminal_id), pane))
            .filter(|(key, _)| !claimed.contains(key))
            .map(|(key, pane)| ListedPane {
                workspace: pane.workspace(&Side::Native, workspaces),
                key,
                pane,
            })
            .collect()
    }

    fn default_side(&self) -> Result<Side, PaneError> {
        Ok(Side::Native)
    }

    /// The session's own stream reports the end of its terminal, and the
    /// listing empties while a connection reopens, which is no end.
    fn gone_since(&self, _side: &Side, _terminal_id: &str, _bound_at: Instant) -> Option<Instant> {
        None
    }

    fn pane_json(&self, side: &Side, terminal_id: &str, _pane: Option<&Pane>) -> Value {
        json!({
            "name": MultiplexerKind::HerdrCore.to_string(),
            "side": side.name(),
            "terminal_id": terminal_id,
            "pane_id": terminal_id,
        })
    }

    /// Closing the session is the way out, so there is no chord to name.
    fn managed(&self, _side: &Side, pane: Option<&Pane>) -> Managed {
        Managed {
            multiplexer: MultiplexerKind::HerdrCore,
            detach: None,
            shared_view: false,
            kind: pane.and_then(|pane| pane.kind.clone()),
            title: pane.and_then(|pane| pane.title.clone()),
            status: None,
        }
    }

    fn attaches_directly(&self, _side: &Side, _has_agent: bool) -> bool {
        true
    }

    fn open_directly(&self, _target: &PaneTarget) -> Option<Launch> {
        None
    }

    fn shared_view(&self, _key: &PaneKey) -> Option<Launch> {
        None
    }

    fn queue_attach(&mut self, key: PaneKey, _target: PaneTarget, request: AttachRequest) {
        let launch = Err(PaneError::Backend(Box::new(PanesError::NoAttachClient)));
        self.refused_attaches.push(AttachAnswer { key, request, launch });
    }

    fn poll_attach(&mut self) -> (Option<AttachAnswer>, bool) {
        (self.refused_attaches.pop(), false)
    }

    fn queue_create(&mut self, side: Side, _cwd: Option<String>, request: CreateRequest) {
        let pane = Err(PaneError::Backend(Box::new(PanesError::NoCreate)));
        self.refused_creates.push(CreateAnswer { side, request, pane });
    }

    fn poll_create(&mut self) -> Option<CreateAnswer> {
        self.refused_creates.pop()
    }

    fn sync_view(&mut self, _state: ViewState<'_>) -> ViewStep {
        ViewStep::default()
    }

    fn view_attached(&mut self, _id: SessionId, _key: &PaneKey) {}

    fn view_refused(&mut self, _key: &PaneKey) {}

    fn session_closed(&mut self, _id: SessionId, _key: Option<&PaneKey>) {}

    fn backend_terminals(&self) -> bool {
        true
    }

    fn kill_terminal(&mut self, key: &PaneKey) {
        self.core.kill_terminal(&key.terminal_id);
    }
}

impl HerdrCorePanes {
    fn key(&self, terminal_id: &str) -> PaneKey {
        PaneKey {
            multiplexer: MultiplexerKind::HerdrCore,
            side: Side::Native,
            terminal_id: terminal_id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::connection::Foreground;

    fn live(labels: &[(&str, &str)]) -> Facts {
        Facts {
            state: State::Live,
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Facts::default()
        }
    }

    /// A terminal herdr-app or an agent started is a pane. One that
    /// alacritree's labels claim, or whose program ended, is not.
    #[test]
    fn only_a_live_terminal_without_alacritrees_labels_is_a_pane() {
        assert!(foreign_pane("t1", &live(&[("herdr.tab", "tb1")])).is_some());
        assert!(foreign_pane("t2", &live(&[(labels::WORKSPACE, "~")])).is_none());
        assert!(foreign_pane("t3", &Facts { state: State::Finished, ..live(&[]) }).is_none());
    }

    /// The name a client gave the terminal wins, then the place `hc ls`
    /// shows, then the program's own title.
    #[test]
    fn a_pane_is_named_by_its_name_then_where_then_title() {
        let titled = Facts { title: Some("vim".into()), ..live(&[(labels::WHERE, "app / 1")]) };
        let named = Facts { name: Some("review".into()), ..titled.clone() };

        assert_eq!(foreign_pane("t1", &named).and_then(|p| p.title).as_deref(), Some("review"));
        assert_eq!(foreign_pane("t1", &titled).and_then(|p| p.title).as_deref(), Some("app / 1"));
        let bare = Facts { labels: HashMap::new(), ..titled };
        assert_eq!(foreign_pane("t1", &bare).and_then(|p| p.title).as_deref(), Some("vim"));
    }

    const CHECKOUT_A: &str = "/src/app";
    const CHECKOUT_B: &str = "/src/app-feature";

    /// A terminal that started in checkout A and whose shell then moved to
    /// checkout B.
    fn moved_from_a_to_b(labels: &[(&str, &str)]) -> Pane {
        let shell = Foreground {
            pid: 10,
            name: "nu".into(),
            argv: vec!["nu".into()],
            cwd: format!("{CHECKOUT_B}/src").into(),
        };
        let facts = Facts { foreground: Some(shell), cwd: Some(CHECKOUT_A.into()), ..live(labels) };
        foreign_pane("t1", &facts).expect("a live foreign terminal is a pane")
    }

    fn workspace(pane: &Pane) -> Option<PathBuf> {
        pane.workspace(&Side::Native, &[PathBuf::from(CHECKOUT_A), PathBuf::from(CHECKOUT_B)])
    }

    /// herdr-app keeps a terminal in its space after the shell moves, so the
    /// row stays under the checkout the terminal started in.
    #[test]
    fn a_terminal_in_a_herdr_app_space_lists_where_it_started() {
        let pane = moved_from_a_to_b(&[(labels::HERDR_SPACE, "sp1")]);

        assert_eq!(workspace(&pane), Some(PathBuf::from(CHECKOUT_A)));
    }

    #[test]
    fn a_terminal_in_no_herdr_app_space_lists_where_its_shell_is() {
        let unlabelled = moved_from_a_to_b(&[]);
        let parked = moved_from_a_to_b(&[(labels::HERDR_SPACE, "")]);

        assert_eq!(workspace(&unlabelled), Some(PathBuf::from(CHECKOUT_B)));
        assert_eq!(workspace(&parked), Some(PathBuf::from(CHECKOUT_B)));
    }

    /// Only the row's group stays put. Anything that works in the terminal's
    /// directory, such as the palette, still gets the shell's.
    #[test]
    fn a_terminal_in_a_herdr_app_space_works_where_its_shell_is() {
        let pane = moved_from_a_to_b(&[(labels::HERDR_SPACE, "sp1")]);

        assert_eq!(pane.working_directory(), Some(format!("{CHECKOUT_B}/src").as_str()));
    }
}
