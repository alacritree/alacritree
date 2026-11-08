use alacritree_common::settings::{ClosedSet, IconStyle, RawIconStyle};
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

/// `[integrations.herdr_core]`: where the `hc` binary lives, the glyph that
/// marks its terminals, and whether new sessions run in its core.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HerdrCoreConfig {
    /// The native `hc` binary.
    pub path: String,
    /// The glyph on a herdr-core pane's sidebar row and palette entry.
    pub icon: IconStyle,
    /// Which backend runs the terminal of a new session on the native side.
    pub session_backend: SessionBackendChoice,
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
    /// up on PATH; any other value runs as written. hc finds its core's
    /// socket in its own order: `HC_SOCKET`, then the `socket` key in its
    /// `config.toml`, then its runtime directory.
    path: String,
    /// The glyph on a herdr-core pane's sidebar row and palette entry. A bare
    /// string sets the glyph; a table also styles its color, weight, slant
    /// and size, the way `[ui.icons]` keys do. An ordinary character such as
    /// `"◫"` is drawn by your own fonts, and a glyph from the bundled symbol
    /// font by that font.
    #[schemars(default = "default_icon")]
    icon: Option<RawIconStyle>,
    /// Which backend runs the terminal of a new session on the native side.
    ///
    /// "native" opens a PTY that alacritree owns. "herdr_core" runs the
    /// session as a terminal in the native side's herdr-core core, through
    /// `hc connect`. Such a terminal outlives alacritree: quitting leaves it
    /// running, and the next launch puts it back in its workspace. When that
    /// core cannot be used, the session opens native and a notice names the
    /// cause. WSL sessions and diff panes stay native.
    session_backend: ClosedSet<SessionBackendChoice>,
}

impl Default for RawHerdrCore {
    fn default() -> Self {
        Self { path: "hc".to_string(), icon: None, session_backend: ClosedSet::default() }
    }
}

fn default_icon() -> RawIconStyle {
    RawIconStyle::Glyph(DEFAULT_ICON.to_string())
}

impl RawHerdrCore {
    pub fn resolve(self) -> HerdrCoreConfig {
        HerdrCoreConfig {
            path: Some(self.path)
                .filter(|path| !path.trim().is_empty())
                .unwrap_or_else(|| RawHerdrCore::default().path),
            icon: self.icon.unwrap_or_else(default_icon).into(),
            session_backend: self.session_backend.get(),
        }
    }
}
