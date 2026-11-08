use alacritree_common::settings::{ClosedSet, IconStyle, RawIconStyle};
use alacritree_common::tools::{Tool, tool_config};
use schemars::JsonSchema;
use serde::Deserialize;
use strum::{EnumIter, IntoStaticStr};

use crate::DEFAULT_ICON;

/// Which backend runs the terminal of a new session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, EnumIter, IntoStaticStr)]
#[serde(into = "&'static str")]
#[strum(serialize_all = "snake_case")]
pub enum SessionBackendChoice {
    /// A PTY that alacritree owns, which ends when alacritree does.
    #[default]
    Native,
    /// A terminal that the side's herdr-core core owns.
    HerdrCore,
}

/// `[integrations.herdr_core]`: where the `hc` binary lives on each side, the
/// glyph that marks its terminals, whether new sessions run in its cores, and
/// whether the terminals that alacritree did not create are listed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HerdrCoreConfig {
    /// The native `hc` binary.
    pub path: String,
    /// The `hc` binary inside WSL, or `None` to find it by name there.
    pub wsl_path: Option<String>,
    /// The glyph on a herdr-core pane's sidebar row and palette entry.
    pub icon: IconStyle,
    /// Which backend runs the terminal of a new session.
    pub session_backend: SessionBackendChoice,
    /// List the core's terminals that alacritree did not create as panes.
    pub enabled: bool,
}

impl Default for HerdrCoreConfig {
    fn default() -> Self {
        RawHerdrCore::default().resolve()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RawHerdrCore {
    /// The `hc` program to run on Windows or natively. Its own name is looked
    /// up on PATH, then in the folder hc's installer uses; any other value
    /// runs as written. hc finds its core's
    /// socket in its own order: `HC_SOCKET`, then the `socket` key in its
    /// `config.toml`, then its runtime directory.
    path: String,
    /// The `hc` program to run inside every WSL distro, as written. Empty
    /// finds it by name through the distro's login shell.
    wsl_path: String,
    /// The glyph on a herdr-core pane's sidebar row and palette entry. A bare
    /// string sets the glyph; a table also styles its color, weight, slant
    /// and size, the way `[ui.icons]` keys do. An ordinary character such as
    /// `"◫"` is drawn by your own fonts, and a glyph from the bundled symbol
    /// font by that font.
    #[schemars(default = "default_icon")]
    icon: Option<RawIconStyle>,
    /// Which backend runs the terminal of a new session.
    ///
    /// "native" opens a PTY that alacritree owns. "herdr_core" runs the
    /// session as a terminal in the core of the session's side, through
    /// `hc connect`. A native shell runs in the native core, and a shell in
    /// a WSL distro runs in the distro's core. Such a terminal outlives
    /// alacritree while its core runs. Quitting leaves it running, and the
    /// next launch puts it back in its workspace. So that WSL does not stop
    /// an idle distro, quitting also leaves a hidden `wsl.exe` keeper for
    /// each distro whose core has live terminals, and the keeper exits with
    /// the last one. When that core cannot be used, the session opens native
    /// and a notice names the cause. When the side has no `hc`, the notice
    /// instead offers hc's official installer and shows its command, which
    /// runs only once the user confirms. When the core takes the request and
    /// gives no answer, no session opens until the core reports the
    /// terminal. Diff panes stay native.
    session_backend: ClosedSet<SessionBackendChoice>,
    /// List the native core's terminals that alacritree did not create, such
    /// as the ones herdr-app or an agent started, as panes in the sidebar.
    ///
    /// Opening such a pane attaches to it, and closing that session only
    /// detaches. Its row's menu can also watch it, which never types into it
    /// or resizes it, and kill it after a confirmation. alacritree reads the
    /// list from a core that already runs, and never starts one for it.
    enabled: bool,
}

impl Default for RawHerdrCore {
    fn default() -> Self {
        Self {
            path: Tool::Hc.name().to_string(),
            wsl_path: String::new(),
            icon: None,
            session_backend: ClosedSet::default(),
            enabled: true,
        }
    }
}

fn default_icon() -> RawIconStyle {
    RawIconStyle::Glyph(DEFAULT_ICON.to_string())
}

impl RawHerdrCore {
    pub fn resolve(self) -> HerdrCoreConfig {
        let tool = tool_config(self.path, self.wsl_path, Tool::Hc);
        HerdrCoreConfig {
            path: tool.path,
            wsl_path: tool.wsl_path,
            icon: self.icon.unwrap_or_else(default_icon).into(),
            session_backend: self.session_backend.get(),
            enabled: self.enabled,
        }
    }
}
